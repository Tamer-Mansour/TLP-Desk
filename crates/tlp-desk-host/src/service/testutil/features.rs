//! Fake user-session features for the agent (clipboard, shell offers, overlays, markers,
//! local-input hooks, shields, audio): recorded in a [`FeatureOs`] the test inspects.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_input::whiteboard::{ClearRequestFn, SharedBoard, WhiteboardOverlay};
use tlp_desk_input::{
    ActivityStamp, InputError, LocalActivity, MonitorRect, PointerMarker, PrivacyScreenText,
    PrivacyShield, ShieldSignalFn,
};
use tlp_desk_proto::TransferItem;
use tokio::sync::watch;

use crate::service::ipc::{ClipData, WireError};
use crate::service::{AgentFeatures, AudioSource, StageLink};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Default)]
pub(crate) struct OverlayLog {
    pub target: Option<MonitorRect>,
    pub changed: u32,
    pub board: SharedBoard,
    pub on_clear: Option<ClearRequestFn>,
    pub dropped: bool,
}

#[derive(Default)]
pub(crate) struct MarkerLog {
    pub label: String,
    pub visible: bool,
    pub dropped: bool,
}

#[derive(Default)]
pub(crate) struct ShieldLog {
    pub engaged: bool,
    pub text: PrivacyScreenText,
    pub releases: u32,
    pub on_signal: Option<ShieldSignalFn>,
}

pub(crate) type Offer = (u64, PathBuf, Vec<TransferItem>, Arc<dyn StageLink>);

/// What the fake desktop of the user looks like.
pub(crate) struct FeatureOs {
    pub clip: Mutex<Option<ClipData>>,
    pub seq: AtomicU64,
    pub clip_tx: watch::Sender<u64>,
    pub files: Mutex<Option<Vec<String>>>,
    pub offers: Mutex<Vec<Offer>>,
    pub cleared: Mutex<Vec<u64>>,
    pub overlays: Mutex<Vec<Arc<Mutex<OverlayLog>>>>,
    pub markers: Mutex<Vec<Arc<Mutex<MarkerLog>>>>,
    pub activity: Mutex<Option<Arc<ActivityStamp>>>,
    pub activity_starts: AtomicU32,
    pub shields: Mutex<Vec<Arc<Mutex<ShieldLog>>>>,
    pub audio: Mutex<Vec<(u32, u16, Vec<f32>)>>,
    pub audio_closed: AtomicU32,
}

impl Default for FeatureOs {
    fn default() -> Self {
        Self {
            clip: Mutex::default(),
            seq: AtomicU64::new(0),
            clip_tx: watch::channel(0).0,
            files: Mutex::default(),
            offers: Mutex::default(),
            cleared: Mutex::default(),
            overlays: Mutex::default(),
            markers: Mutex::default(),
            activity: Mutex::default(),
            activity_starts: AtomicU32::new(0),
            shields: Mutex::default(),
            audio: Mutex::default(),
            audio_closed: AtomicU32::new(0),
        }
    }
}

impl FeatureOs {
    /// The local user copies something.
    pub(crate) fn user_copies(&self, clip: ClipData) {
        *lock(&self.clip) = Some(clip);
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.clip_tx.send_replace(seq);
    }

    /// The newest overlay the agent created.
    pub(crate) fn overlay(&self) -> Arc<Mutex<OverlayLog>> {
        lock(&self.overlays).last().cloned().expect("an overlay")
    }

    /// The newest marker the agent created.
    pub(crate) fn marker(&self) -> Arc<Mutex<MarkerLog>> {
        lock(&self.markers).last().cloned().expect("a marker")
    }

    /// The newest shield the agent created.
    pub(crate) fn shield(&self) -> Arc<Mutex<ShieldLog>> {
        lock(&self.shields).last().cloned().expect("a shield")
    }
}

/// Fake [`AgentFeatures`] over a [`FeatureOs`].
#[derive(Clone, Default)]
pub(crate) struct FakeFeatures(pub Arc<FeatureOs>);

struct Overlay(Arc<Mutex<OverlayLog>>);

impl WhiteboardOverlay for Overlay {
    fn set_target(&mut self, rect: MonitorRect) {
        lock(&self.0).target = Some(rect);
    }
    fn changed(&mut self) {
        lock(&self.0).changed += 1;
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        lock(&self.0).dropped = true;
    }
}

struct Marker(Arc<Mutex<MarkerLog>>);

impl PointerMarker for Marker {
    fn set_visible(&mut self, visible: bool) {
        lock(&self.0).visible = visible;
    }
    fn set_label(&mut self, label: &str) {
        label.clone_into(&mut lock(&self.0).label);
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        lock(&self.0).dropped = true;
    }
}

struct Shield(Arc<Mutex<ShieldLog>>);

impl PrivacyShield for Shield {
    fn engage(&mut self, text: &PrivacyScreenText) -> Result<(), InputError> {
        let mut l = lock(&self.0);
        l.engaged = true;
        l.text = text.clone();
        Ok(())
    }
    fn set_text(&mut self, text: &PrivacyScreenText) {
        lock(&self.0).text = text.clone();
    }
    fn release(&mut self) {
        let mut l = lock(&self.0);
        l.engaged = false;
        l.releases += 1;
    }
    fn is_engaged(&self) -> bool {
        lock(&self.0).engaged
    }
}

struct Audio(Arc<FeatureOs>);

impl AudioSource for Audio {
    fn next_chunk(&mut self, timeout: Duration) -> Result<Option<(u32, u16, Vec<f32>)>, WireError> {
        let next = {
            let mut q = lock(&self.0.audio);
            (!q.is_empty()).then(|| q.remove(0))
        };
        if next.is_none() {
            std::thread::sleep(timeout.min(Duration::from_millis(20)));
        }
        Ok(next)
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.0.audio_closed.fetch_add(1, Ordering::SeqCst);
    }
}

impl AgentFeatures for FakeFeatures {
    fn clipboard_read(&self) -> Result<Option<ClipData>, WireError> {
        Ok(lock(&self.0.clip).clone())
    }
    fn clipboard_write(&self, clip: &ClipData) -> Result<u64, WireError> {
        *lock(&self.0.clip) = Some(clip.clone());
        Ok(self.0.seq.fetch_add(1, Ordering::SeqCst) + 1)
    }
    fn clipboard_files(&self) -> Option<Vec<String>> {
        lock(&self.0.files).clone()
    }
    fn clipboard_changes(&self) -> Option<watch::Receiver<u64>> {
        Some(self.0.clip_tx.subscribe())
    }
    fn shell_offer(
        &self,
        offer: u64,
        dir: PathBuf,
        items: Vec<TransferItem>,
        stage: Arc<dyn StageLink>,
    ) {
        lock(&self.0.offers).push((offer, dir, items, stage));
    }
    fn shell_clear(&self, offer: u64) {
        lock(&self.0.cleared).push(offer);
    }
    fn open_overlay(
        &self,
        board: SharedBoard,
        on_clear: ClearRequestFn,
    ) -> Result<Box<dyn WhiteboardOverlay>, InputError> {
        let log = Arc::new(Mutex::new(OverlayLog {
            board,
            on_clear: Some(on_clear),
            ..OverlayLog::default()
        }));
        lock(&self.0.overlays).push(log.clone());
        Ok(Box::new(Overlay(log)))
    }
    fn open_marker(&self, label: &str) -> Result<Box<dyn PointerMarker>, InputError> {
        let log = Arc::new(Mutex::new(MarkerLog {
            label: label.to_owned(),
            ..MarkerLog::default()
        }));
        lock(&self.0.markers).push(log.clone());
        Ok(Box::new(Marker(log)))
    }
    fn start_activity(&self) -> Result<Arc<dyn LocalActivity>, InputError> {
        self.0.activity_starts.fetch_add(1, Ordering::SeqCst);
        let stamp = Arc::new(ActivityStamp::new());
        *lock(&self.0.activity) = Some(stamp.clone());
        Ok(stamp)
    }
    fn open_shield(&self, on_signal: ShieldSignalFn) -> Result<Box<dyn PrivacyShield>, InputError> {
        let log = Arc::new(Mutex::new(ShieldLog {
            on_signal: Some(on_signal),
            ..ShieldLog::default()
        }));
        lock(&self.0.shields).push(log.clone());
        Ok(Box::new(Shield(log)))
    }
    fn open_audio(&self) -> Result<Box<dyn AudioSource>, WireError> {
        Ok(Box::new(Audio(self.0.clone())))
    }
}

/// Polls `f` until it holds (10 s at most).
pub(crate) fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
