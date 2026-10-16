//! Agent side: the real user-session objects the session agent runs for the service (it lives
//! in the logged-on user's session): the system clipboard, Explorer's virtual files, the
//! whiteboard overlay, the pointer marker, the local-input hooks, the privacy shield and
//! loopback audio.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tlp_desk_audio::{AudioCapture, DeviceHandle, PcmChunk, PcmSink, SystemCapture};
use tlp_desk_files::shellclip::{ShellClipboard, StageState, StagedItems, Stager};
use tlp_desk_files::{ClipContent, ClipboardBackend, SystemClipboard};
use tlp_desk_host::service::ipc::{ClipData, WireError, WireStage};
use tlp_desk_host::service::{AgentFeatures, AudioSource, StageLink};
use tlp_desk_input::whiteboard::{ClearRequestFn, SharedBoard, WhiteboardOverlay};
use tlp_desk_input::{InputError, LocalActivity, PointerMarker, PrivacyShield, ShieldSignalFn};
use tlp_desk_proto::TransferItem;
use tokio::sync::{mpsc, watch};

use super::proxies::{clip_content, clip_data};

/// The agent's real features.
pub struct AppAgentFeatures {
    clipboard: Arc<SystemClipboard>,
    /// Created on the first offer: its COM thread must start after the agent set the
    /// process's COM security (`allow_interactive_com_calls`).
    shell: OnceLock<Arc<ShellClipboard>>,
}

impl Default for AppAgentFeatures {
    fn default() -> Self {
        Self::new()
    }
}

impl AppAgentFeatures {
    /// The system clipboard and shell clipboard of this (the user's) session.
    pub fn new() -> Self {
        Self {
            clipboard: Arc::new(SystemClipboard::new()),
            shell: OnceLock::new(),
        }
    }

    fn shell(&self) -> &Arc<ShellClipboard> {
        self.shell.get_or_init(ShellClipboard::system)
    }
}

/// Explorer's reads in the agent drive the pull in the service.
struct StageAdapter(Arc<dyn StageLink>);

impl Stager for StageAdapter {
    fn begin(&self) {
        self.0.begin();
    }

    fn state(&self) -> StageState {
        match self.0.state() {
            WireStage::Idle => StageState::Idle,
            WireStage::Running => StageState::Running,
            WireStage::Finished => StageState::Finished,
            WireStage::Failed(e) => StageState::Failed(e),
        }
    }

    fn cancel(&self) {
        self.0.cancel();
    }
}

/// Loopback chunks from the capture device's queue.
struct Loopback {
    _device: DeviceHandle,
    rx: mpsc::Receiver<PcmChunk>,
}

impl AudioSource for Loopback {
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<(u32, u16, Vec<f32>)>, WireError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.rx.try_recv() {
                Ok(c) => return Ok(Some((c.rate, c.channels, c.samples))),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(WireError::Other("capture stopped".into()))
                }
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl AgentFeatures for AppAgentFeatures {
    fn clipboard_read(&self) -> Result<Option<ClipData>, WireError> {
        match self.clipboard.read() {
            Ok(c) => Ok(c.as_ref().and_then(clip_data)),
            Err(e) => Err(WireError::Other(e.to_string())),
        }
    }

    fn clipboard_write(&self, clip: &ClipData) -> Result<u64, WireError> {
        let content: ClipContent = clip_content(clip.clone());
        self.clipboard
            .write(&content)
            .map_err(|e| WireError::Other(e.to_string()))
    }

    fn clipboard_files(&self) -> Option<Vec<String>> {
        let list = tlp_desk_files::clipfiles::local_file_list();
        (!list.is_empty()).then(|| {
            list.iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        })
    }

    fn clipboard_changes(&self) -> Option<watch::Receiver<u64>> {
        self.clipboard.changes()
    }

    fn shell_offer(
        &self,
        offer: u64,
        dir: PathBuf,
        items: Vec<TransferItem>,
        stage: Arc<dyn StageLink>,
    ) {
        let staged = StagedItems::new(dir, items, Arc::new(StageAdapter(stage)));
        self.shell().offer(offer, staged);
    }

    fn shell_clear(&self, offer: u64) {
        if let Some(shell) = self.shell.get() {
            shell.clear(offer);
        }
    }

    fn open_overlay(
        &self,
        board: SharedBoard,
        on_clear: ClearRequestFn,
    ) -> Result<Box<dyn WhiteboardOverlay>, InputError> {
        tlp_desk_input::whiteboard::start_whiteboard_overlay(board, on_clear)
    }

    fn open_marker(&self, label: &str) -> Result<Box<dyn PointerMarker>, InputError> {
        tlp_desk_input::start_pointer_marker(label)
    }

    fn start_activity(&self) -> Result<Arc<dyn LocalActivity>, InputError> {
        tlp_desk_input::start_local_activity_monitor()
    }

    fn open_shield(&self, on_signal: ShieldSignalFn) -> Result<Box<dyn PrivacyShield>, InputError> {
        tlp_desk_input::start_privacy_shield(on_signal)
    }

    fn open_audio(&self) -> Result<Box<dyn AudioSource>, WireError> {
        let (sink, rx) = PcmSink::channel();
        let device = SystemCapture
            .start(sink)
            .map_err(|e| WireError::Other(e.to_string()))?;
        Ok(Box::new(Loopback {
            _device: device,
            rx,
        }))
    }
}
