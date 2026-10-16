//! Service side: the clipboard, Explorer-paste, copied-files and audio objects the session
//! features use, backed by the session agent through the host's `AgentFeatureHub`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_audio::{AudioCapture, AudioError, DeviceHandle, PcmChunk, PcmSink};
use tlp_desk_files::clipfiles::{ClipboardFileSource, SourceFactory};
use tlp_desk_files::shellclip::{ClipboardPort, StageState, StagedItems};
use tlp_desk_files::{ClipContent, ClipImage, ClipboardBackend, ClipboardError};
use tlp_desk_host::service::ipc::{ClipData, WireStage};
use tlp_desk_host::service::AgentFeatureHub;
use tokio::sync::watch;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Clipboard content -> wire form (file lists are not synced by the clipboard feature).
pub fn clip_data(c: &ClipContent) -> Option<ClipData> {
    match c {
        ClipContent::Text(t) => Some(ClipData::Text(t.clone())),
        ClipContent::Html { html, alt_text } => Some(ClipData::Html {
            html: html.clone(),
            alt_text: alt_text.clone(),
        }),
        ClipContent::Image(i) => Some(ClipData::Image {
            width: i.width,
            height: i.height,
            rgba: i.rgba.clone(),
        }),
        ClipContent::Files(_) => None,
    }
}

/// Wire form -> clipboard content.
pub fn clip_content(c: ClipData) -> ClipContent {
    match c {
        ClipData::Text(t) => ClipContent::Text(t),
        ClipData::Html { html, alt_text } => ClipContent::Html { html, alt_text },
        ClipData::Image {
            width,
            height,
            rgba,
        } => ClipContent::Image(ClipImage {
            width,
            height,
            rgba,
        }),
    }
}

/// The logged-on user's clipboard, read and written by the agent.
pub struct AgentClipboard {
    hub: Arc<AgentFeatureHub>,
}

impl AgentClipboard {
    pub fn new(hub: Arc<AgentFeatureHub>) -> Self {
        Self { hub }
    }
}

fn clip_err(e: impl std::fmt::Display) -> ClipboardError {
    ClipboardError::Clipboard(e.to_string())
}

impl ClipboardBackend for AgentClipboard {
    fn read(&self) -> Result<Option<ClipContent>, ClipboardError> {
        self.hub
            .clipboard_read()
            .map(|c| c.map(clip_content))
            .map_err(clip_err)
    }

    fn write(&self, content: &ClipContent) -> Result<u64, ClipboardError> {
        let data = clip_data(content).ok_or_else(|| clip_err("file lists are not written"))?;
        self.hub.clipboard_write(&data).map_err(clip_err)
    }

    fn changes(&self) -> Option<watch::Receiver<u64>> {
        Some(self.hub.clipboard_changes())
    }
}

/// The files the user copied in Explorer, asked from the agent (polled by the clipboard-files
/// feature while a session is connected).
pub fn agent_file_source(hub: Arc<AgentFeatureHub>) -> SourceFactory {
    struct Source(Arc<AgentFeatureHub>);
    impl ClipboardFileSource for Source {
        fn file_list(&mut self) -> Option<Vec<PathBuf>> {
            self.0
                .clipboard_files()
                .map(|v| v.into_iter().map(PathBuf::from).collect())
                .filter(|v: &Vec<PathBuf>| !v.is_empty())
        }
    }
    Arc::new(move || Some(Box::new(Source(hub.clone())) as Box<dyn ClipboardFileSource>))
}

fn wire_stage(s: &StageState) -> WireStage {
    match s {
        StageState::Idle => WireStage::Idle,
        StageState::Running => WireStage::Running,
        StageState::Finished => WireStage::Finished,
        StageState::Failed(e) => WireStage::Failed(e.clone()),
    }
}

/// How often the pull state of an offer is pushed to the agent.
const STAGE_PUSH: Duration = Duration::from_millis(150);

/// Puts the peer's copied files on the user's clipboard through the agent; Explorer's reads
/// there start and follow the pull here.
pub struct AgentShellPort {
    hub: Arc<AgentFeatureHub>,
    current: Mutex<Option<(u64, Arc<AtomicBool>)>>,
}

impl AgentShellPort {
    pub fn new(hub: Arc<AgentFeatureHub>) -> Arc<Self> {
        Arc::new(Self {
            hub,
            current: Mutex::new(None),
        })
    }
}

impl ClipboardPort for AgentShellPort {
    fn set(&self, offer: Arc<StagedItems>) {
        let id = self.hub.next_id();
        let alive = Arc::new(AtomicBool::new(true));
        if let Some((old, flag)) = lock(&self.current).replace((id, alive.clone())) {
            flag.store(false, Ordering::Release);
            self.hub.shell_clear(old);
        }
        let weak = Arc::downgrade(&offer);
        self.hub.shell_offer(
            id,
            offer.dir().to_string_lossy().into_owned(),
            offer.items().to_vec(),
            Arc::new(move |begin| {
                if let Some(o) = weak.upgrade() {
                    if begin {
                        o.begin();
                    } else {
                        o.cancel();
                    }
                }
            }),
        );
        // Keep the agent's copy of the pull state current while the offer lives.
        let hub = self.hub.clone();
        let weak = Arc::downgrade(&offer);
        let spawned = std::thread::Builder::new()
            .name("tlp-svc-paste-stage".into())
            .spawn(move || {
                let mut last: Option<WireStage> = None;
                while alive.load(Ordering::Acquire) {
                    let Some(o) = weak.upgrade() else { return };
                    if o.is_cancelled() {
                        return;
                    }
                    let now = wire_stage(&o.state());
                    drop(o);
                    if last.as_ref() != Some(&now) {
                        hub.shell_stage(id, now.clone());
                        last = Some(now);
                    }
                    std::thread::sleep(STAGE_PUSH);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(%e, "cannot follow the paste state");
        }
    }

    fn clear(&self) {
        if let Some((id, flag)) = lock(&self.current).take() {
            flag.store(false, Ordering::Release);
            self.hub.shell_clear(id);
        }
    }
}

/// System sound captured by the agent (loopback of the user's default output device).
pub struct AgentAudioCapture {
    hub: Arc<AgentFeatureHub>,
}

impl AgentAudioCapture {
    pub fn new(hub: Arc<AgentFeatureHub>) -> Self {
        Self { hub }
    }
}

impl AudioCapture for AgentAudioCapture {
    fn start(&self, sink: PcmSink) -> Result<DeviceHandle, AudioError> {
        let mut audio = self
            .hub
            .open_audio()
            .map_err(|e| AudioError::Device(e.to_string()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::Builder::new()
            .name("tlp-svc-audio".into())
            .spawn(move || {
                // Ends when the handle is dropped (checked at least every keep-alive) or the
                // agent stops; dropping the stream stops the agent's capture.
                while !flag.load(Ordering::Acquire) && !sink.is_closed() {
                    match audio.next_packet() {
                        Ok(Some((rate, channels, samples))) => {
                            sink.send(PcmChunk {
                                rate,
                                channels,
                                samples,
                            });
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::info!(%e, "agent audio ended");
                            return;
                        }
                    }
                }
            })
            .map_err(|e| AudioError::Device(e.to_string()))?;
        Ok(DeviceHandle::new(StopOnDrop(stop)))
    }
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_content_maps_both_ways_byte_exact() {
        let cases = [
            ClipContent::Text("مرحبا\u{200f}\r\n".into()),
            ClipContent::Html {
                html: "<b>نص</b>".into(),
                alt_text: "نص".into(),
            },
            ClipContent::Image(ClipImage {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 4],
            }),
        ];
        for c in cases {
            let back = clip_content(clip_data(&c).unwrap());
            assert_eq!(format!("{back:?}"), format!("{c:?}"));
            match (&c, &back) {
                (ClipContent::Text(a), ClipContent::Text(b)) => assert_eq!(a, b),
                (ClipContent::Html { html: a, .. }, ClipContent::Html { html: b, .. }) => {
                    assert_eq!(a, b)
                }
                (ClipContent::Image(a), ClipContent::Image(b)) => assert_eq!(a.rgba, b.rgba),
                _ => panic!("kind changed"),
            }
        }
        assert!(clip_data(&ClipContent::Files(vec![PathBuf::from("x")])).is_none());
    }

    #[test]
    fn stage_states_map() {
        assert_eq!(wire_stage(&StageState::Idle), WireStage::Idle);
        assert_eq!(
            wire_stage(&StageState::Failed("انقطع".into())),
            WireStage::Failed("انقطع".into())
        );
    }
}
