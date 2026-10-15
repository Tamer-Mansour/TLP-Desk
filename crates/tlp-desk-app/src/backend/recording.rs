//! Session recording (TLP-recording): the payload types of the recording events and the
//! real backend's bookkeeping of running recordings.
//!
//! Recording happens on the viewer only, from video the host already sends; the host is not
//! told (the protocol has no message for it) and the viewer shows a red indicator the whole
//! time. See `tlp_desk_viewer::recording`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tlp_desk_viewer::recording::{default_folder, RecordingFailure};
use tlp_desk_viewer::{Recording, RecordingConfig, RecordingSummary, ViewerError, ViewerSession};
use tokio::runtime::Handle;

use super::{BackendEvent, SessionId};

/// Why a recording could not start or ended early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingProblem {
    /// The disk is (almost) full.
    DiskFull,
    /// No recordings folder could be determined (no user profile).
    NoFolder,
    /// Anything else (details for the message and the log).
    Failed(String),
}

impl RecordingProblem {
    fn from_failure(f: &RecordingFailure) -> Self {
        match f {
            RecordingFailure::DiskFull { .. } => Self::DiskFull,
            RecordingFailure::Write(d) => Self::Failed(d.clone()),
        }
    }

    fn from_error(e: &ViewerError) -> Self {
        match e {
            ViewerError::DiskFull { .. } => Self::DiskFull,
            other => Self::Failed(other.to_string()),
        }
    }
}

/// The recordings folder: `%USERPROFILE%\Videos\TLP-Desk`.
pub fn recordings_folder() -> Option<PathBuf> {
    default_folder()
}

/// Sends a backend event (and wakes the UI).
pub(crate) type Emit = Arc<dyn Fn(BackendEvent) + Send + Sync>;

fn stopped_event(session: SessionId, s: &RecordingSummary) -> BackendEvent {
    BackendEvent::RecordingStopped {
        session,
        files: s.files.clone(),
        problem: s.failure.as_ref().map(RecordingProblem::from_failure),
    }
}

/// Running recordings of the real backend, one per viewer session at most.
#[derive(Default)]
pub(crate) struct Recorders {
    active: Mutex<HashMap<SessionId, Recording>>,
    /// Overrides [`recordings_folder`] (tests).
    folder: Mutex<Option<PathBuf>>,
}

impl Recorders {
    fn active(&self) -> MutexGuard<'_, HashMap<SessionId, Recording>> {
        self.active.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Records into `folder` instead of the default recordings folder.
    pub fn set_folder(&self, folder: PathBuf) {
        *self.folder.lock().unwrap_or_else(|p| p.into_inner()) = Some(folder);
    }

    fn folder(&self) -> Option<PathBuf> {
        let custom = self
            .folder
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        custom.or_else(recordings_folder)
    }

    /// Starts recording `session` into the recordings folder. Answers with
    /// [`BackendEvent::RecordingStarted`], or [`BackendEvent::RecordingStopped`] with a
    /// problem. A recording that stops by itself (full disk) reports through `emit` too.
    pub fn start(&self, id: SessionId, session: &Arc<ViewerSession>, emit: Emit) {
        let mut active = self.active();
        if let Some(r) = active.get(&id) {
            if !r.status().stopped {
                return; // already recording
            }
            // It stopped by itself (and said so); its thread is done, dropping is instant.
            active.remove(&id);
        }
        let Some(folder) = self.folder() else {
            drop(active);
            emit(BackendEvent::RecordingStopped {
                session: id,
                files: Vec::new(),
                problem: Some(RecordingProblem::NoFolder),
            });
            return;
        };
        let mut cfg = RecordingConfig::new(folder.clone(), session.peer().name.clone());
        let on_stop = emit.clone();
        cfg.on_stopped = Some(Arc::new(move |s: &RecordingSummary| {
            on_stop(stopped_event(id, s));
        }));
        match Recording::start(session, cfg) {
            Ok(r) => {
                active.insert(id, r);
                drop(active);
                tracing::info!(session = id.0, "recording started");
                emit(BackendEvent::RecordingStarted {
                    session: id,
                    folder,
                });
            }
            Err(e) => {
                drop(active);
                tracing::warn!(session = id.0, %e, "recording could not start");
                emit(BackendEvent::RecordingStopped {
                    session: id,
                    files: Vec::new(),
                    problem: Some(RecordingProblem::from_error(&e)),
                });
            }
        }
    }

    /// Stops recording `session` (if it records) without blocking the caller: the file is
    /// closed on a blocking-pool thread, then [`BackendEvent::RecordingStopped`] follows.
    pub fn stop(&self, id: SessionId, rt: &Handle, emit: Emit) {
        let Some(rec) = self.active().remove(&id) else {
            return;
        };
        if rec.status().stopped {
            return; // already reported when it stopped by itself
        }
        rt.spawn_blocking(move || {
            let summary = rec.stop();
            emit(stopped_event(id, &summary));
        });
    }

    /// Stops every recording and waits for the files to be closed (app exit).
    pub fn stop_all_blocking(&self) {
        let all: Vec<Recording> = self.active().drain().map(|(_, r)| r).collect();
        for r in all {
            let _ = r.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_map_from_viewer_errors() {
        assert_eq!(
            RecordingProblem::from_error(&ViewerError::DiskFull {
                available: 1,
                needed: 2
            }),
            RecordingProblem::DiskFull
        );
        assert_eq!(
            RecordingProblem::from_failure(&RecordingFailure::DiskFull { available: 0 }),
            RecordingProblem::DiskFull
        );
        assert!(matches!(
            RecordingProblem::from_failure(&RecordingFailure::Write("x".into())),
            RecordingProblem::Failed(d) if d == "x"
        ));
    }

    #[test]
    fn stopping_an_unknown_session_is_a_no_op() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let recorders = Recorders::default();
        let emit: Emit = Arc::new(|_| panic!("nothing to report"));
        recorders.stop(SessionId(7), rt.handle(), emit);
        recorders.stop_all_blocking();
    }
}
