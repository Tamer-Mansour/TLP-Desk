//! Runs update steps on a background thread and exposes their state to the UI, which polls
//! [`UpdateWorker::phase`] each frame. Network and hashing never run on the UI thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{http, CheckConfig, Release, UpdateError};

/// Where the update flow is.
#[derive(Debug, Clone, Default)]
pub enum UpdatePhase {
    /// Nothing started (or dismissed with "Later").
    #[default]
    Idle,
    Checking,
    /// The check found nothing newer.
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        done: u64,
        /// 0 when the server sent no length.
        total: u64,
    },
    /// Downloaded and verified; waiting for the user (or launching).
    Verified {
        release: Release,
        installer: PathBuf,
    },
    /// The elevated installer was started; the app should exit now.
    Launched,
    Failed(Arc<UpdateError>),
}

impl UpdatePhase {
    /// A background step is running.
    pub fn busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Downloading { .. })
    }
}

type Repaint = Arc<dyn Fn() + Send + Sync>;

/// Owner of the update background thread(s). Cheap to poll.
pub struct UpdateWorker {
    phase: Arc<Mutex<UpdatePhase>>,
    cancel: Arc<AtomicBool>,
    repaint: Repaint,
}

impl std::fmt::Debug for UpdateWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateWorker")
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}

impl Default for UpdateWorker {
    fn default() -> Self {
        Self::new(Arc::new(|| {}))
    }
}

impl UpdateWorker {
    /// `repaint` is called from the worker thread after every state change.
    pub fn new(repaint: Repaint) -> Self {
        Self {
            phase: Arc::default(),
            cancel: Arc::default(),
            repaint,
        }
    }

    /// Current state (cloned).
    pub fn phase(&self) -> UpdatePhase {
        self.phase.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set(phase: &Mutex<UpdatePhase>, repaint: &Repaint, p: UpdatePhase) {
        *phase.lock().unwrap_or_else(|e| e.into_inner()) = p;
        repaint();
    }

    fn spawn(
        &self,
        first: UpdatePhase,
        job: impl FnOnce(&Mutex<UpdatePhase>, &Repaint, &AtomicBool) + Send + 'static,
    ) {
        if self.phase().busy() {
            return;
        }
        self.cancel.store(false, Ordering::Relaxed);
        Self::set(&self.phase, &self.repaint, first);
        let (phase, repaint, cancel) = (
            self.phase.clone(),
            self.repaint.clone(),
            self.cancel.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("tlp-update".into())
            .spawn(move || job(&phase, &repaint, &cancel));
        if let Err(e) = spawned {
            Self::set(
                &self.phase,
                &self.repaint,
                UpdatePhase::Failed(Arc::new(UpdateError::Io(e))),
            );
        }
    }

    /// Starts a check (ignored while another step runs).
    pub fn check(&self, cfg: CheckConfig) {
        self.spawn(UpdatePhase::Checking, move |phase, repaint, _| {
            let client = http::Client::new();
            let next = match super::check(&client, &cfg, &super::version::current()) {
                Ok(Some(r)) => UpdatePhase::Available(r),
                Ok(None) => UpdatePhase::UpToDate,
                Err(e) => {
                    tracing::info!(%e, "update check failed");
                    UpdatePhase::Failed(Arc::new(e))
                }
            };
            Self::set(phase, repaint, next);
        });
    }

    /// Downloads and verifies `release`.
    pub fn download(&self, cfg: CheckConfig, release: Release) {
        let first = UpdatePhase::Downloading {
            release: release.clone(),
            done: 0,
            total: release.files.as_ref().map_or(0, |f| f.setup.size),
        };
        self.spawn(first, move |phase, repaint, cancel| {
            let client = http::Client::new();
            let mut last_pct = u64::MAX;
            let mut progress = |done: u64, total: u64| {
                let pct = done
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(done >> 20);
                if pct != last_pct {
                    last_pct = pct;
                    Self::set(
                        phase,
                        repaint,
                        UpdatePhase::Downloading {
                            release: release.clone(),
                            done,
                            total,
                        },
                    );
                }
            };
            let next =
                match super::download_and_verify(&client, &cfg, &release, &mut progress, cancel) {
                    Ok(installer) => UpdatePhase::Verified {
                        release: release.clone(),
                        installer,
                    },
                    Err(UpdateError::Cancelled) => UpdatePhase::Available(release.clone()),
                    Err(e) => {
                        tracing::warn!(%e, "update download/verification failed");
                        UpdatePhase::Failed(Arc::new(e))
                    }
                };
            Self::set(phase, repaint, next);
        });
    }

    /// Re-verifies the installer and starts it elevated. On success the phase becomes
    /// [`UpdatePhase::Launched`] and the app should exit.
    pub fn install(&self, cfg: &CheckConfig, installer: &std::path::Path) {
        let next = match super::reverify(cfg, installer)
            .and_then(|()| super::install::launch_installer(installer))
        {
            Ok(()) => UpdatePhase::Launched,
            Err(e) => {
                tracing::warn!(%e, "installer not started");
                UpdatePhase::Failed(Arc::new(e))
            }
        };
        Self::set(&self.phase, &self.repaint, next);
    }

    /// Aborts a running download.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// "Later": forget the offer (keeps a running step running).
    pub fn dismiss(&self) {
        if !self.phase().busy() {
            Self::set(&self.phase, &self.repaint, UpdatePhase::Idle);
        }
    }

    /// Test/README hook: force a state.
    pub fn set_phase(&self, p: UpdatePhase) {
        Self::set(&self.phase, &self.repaint, p);
    }
}
