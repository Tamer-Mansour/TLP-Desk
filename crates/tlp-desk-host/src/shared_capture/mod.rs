//! One screen capturer per monitor, shared by every session that streams it.
//!
//! Each session's video pipeline still opens "its" capturer through the [`CapturerFactory`]
//! and keeps its own encoder, quality preset, adaptive ladder and dirty tracking. With
//! [`share`] in front of the real factory, sessions that ask for the same monitor with the
//! same options (size cap, backend) get a [`SharedCapturer`] handle on one **hub** instead of a
//! capturer of their own:
//!
//! * the hub thread owns the real capturer: it waits for frames, polls the cursor and keeps
//!   only the newest frame (newest wins, like the pipeline's own mailbox);
//! * every handle hands out the newest frame it has not seen yet. With one subscriber the
//!   frame is moved out (no copy); with several each gets its own copy (the encoder draws the
//!   cursor into it), which is far cheaper than a second GPU capture + readback;
//! * cursor shapes are tracked per handle, so each session still receives a changed shape
//!   exactly once (the [`ScreenCapturer::cursor`] contract);
//! * a lost surface (desktop switch, mode change) or a capturer that keeps failing ends the
//!   hub: every handle reports the error once, the pipelines reopen, and the first reopen
//!   starts a fresh hub that the others join;
//! * the hub stops when its last handle is dropped.
//!
//! Sessions on different monitors, or whose adaptive resolution steps differ, simply get
//! different hubs, so they stay as independent as before.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use tlp_desk_capture::{
    BgraFrame, CaptureBackend, CaptureError, CaptureOptions, CursorShape, CursorState,
    MonitorInfo, ScreenCapturer,
};

use crate::CapturerFactory;

/// How long the hub thread waits for one frame before polling the cursor again.
const HUB_WAIT: Duration = Duration::from_millis(10);
/// Consecutive capture errors (other than a lost surface) before the hub gives up.
const MAX_HUB_ERRORS: u32 = 100;
/// Spare frame buffers kept for reuse.
const MAX_SPARE: usize = 3;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Wraps `inner` so sessions asking for the same monitor and options share one capturer.
pub(crate) fn share(inner: CapturerFactory) -> CapturerFactory {
    let registry = Arc::new(Registry {
        inner,
        hubs: Mutex::default(),
    });
    Arc::new(move |index, options| registry.open(index, options))
}

struct Registry {
    inner: CapturerFactory,
    /// Live hubs by (monitor index, options). A handful at most: a linear scan is fine.
    hubs: Mutex<Vec<(u32, CaptureOptions, Weak<Hub>)>>,
}

impl Registry {
    fn open(
        &self,
        index: u32,
        options: CaptureOptions,
    ) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
        // Held while a new hub opens, so two sessions starting together share one capturer.
        let mut hubs = lock(&self.hubs);
        hubs.retain(|(_, _, w)| w.upgrade().is_some_and(|h| h.alive()));
        if let Some(hub) = hubs
            .iter()
            .find(|(i, o, _)| *i == index && *o == options)
            .and_then(|(_, _, w)| w.upgrade())
        {
            tracing::debug!(monitor = index, "joining the shared capturer");
            return Ok(Box::new(SharedCapturer::subscribe(hub)));
        }
        let hub = Hub::start(&self.inner, index, options)?;
        hubs.push((index, options, Arc::downgrade(&hub)));
        Ok(Box::new(SharedCapturer::first(hub)))
    }
}

/// Why a hub ended (re-created as a fresh error for each handle).
#[derive(Clone)]
enum Ended {
    SurfaceLost,
    Failed(String),
}

impl Ended {
    fn error(&self) -> CaptureError {
        match self {
            Ended::SurfaceLost => CaptureError::SurfaceLost,
            Ended::Failed(e) => CaptureError::Platform(e.clone()),
        }
    }
}

#[derive(Default)]
struct HubState {
    /// Newest frame and its generation (0 = none yet).
    frame: Option<Arc<BgraFrame>>,
    frame_gen: u64,
    /// Latest cursor position/visibility.
    cursor: Option<(i32, i32, bool)>,
    /// Latest cursor shape and its generation (0 = none yet).
    shape: Option<Arc<CursorShape>>,
    shape_gen: u64,
    subscribers: usize,
    ended: Option<Ended>,
    /// Buffers handed back by the pipelines, for the real capturer to reuse.
    spare: Vec<BgraFrame>,
}

struct Hub {
    state: Mutex<HubState>,
    cv: Condvar,
    monitor: MonitorInfo,
    frame_size: (u32, u32),
    backend: Option<CaptureBackend>,
}

impl Hub {
    fn alive(&self) -> bool {
        let s = lock(&self.state);
        s.ended.is_none() && s.subscribers > 0
    }

    /// Opens the real capturer on a new hub thread (so it lives and dies on one thread).
    fn start(
        factory: &CapturerFactory,
        index: u32,
        options: CaptureOptions,
    ) -> Result<Arc<Hub>, CaptureError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let factory = Arc::clone(factory);
        std::thread::Builder::new()
            .name(format!("tlp-host-capture-{index}"))
            .spawn(move || {
                let mut cap = match factory(index, options) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                let hub = Arc::new(Hub {
                    state: Mutex::new(HubState {
                        // The opener subscribes right away; counted here so the loop does
                        // not see zero subscribers and stop before that.
                        subscribers: 1,
                        ..HubState::default()
                    }),
                    cv: Condvar::new(),
                    monitor: cap.monitor().clone(),
                    frame_size: cap.frame_size(),
                    backend: cap.backend(),
                });
                if tx.send(Ok(Arc::clone(&hub))).is_err() {
                    return;
                }
                tracing::info!(monitor = index, "shared capturer started");
                hub.run(cap.as_mut());
                tracing::info!(monitor = index, "shared capturer stopped");
            })
            .map_err(|e| CaptureError::Platform(format!("cannot spawn capture thread: {e}")))?;
        rx.recv()
            .unwrap_or_else(|_| Err(CaptureError::Platform("capture thread ended".into())))
    }

    fn run(&self, cap: &mut dyn ScreenCapturer) {
        let mut errors = 0u32;
        loop {
            let spare = {
                let mut s = lock(&self.state);
                if s.subscribers == 0 {
                    return;
                }
                std::mem::take(&mut s.spare)
            };
            for f in spare {
                cap.recycle(f);
            }
            match cap.next_frame(HUB_WAIT) {
                Ok(Some(frame)) => {
                    errors = 0;
                    let old = {
                        let mut s = lock(&self.state);
                        s.frame_gen = s.frame_gen.wrapping_add(1);
                        s.frame.replace(Arc::new(frame))
                    };
                    self.cv.notify_all();
                    // Nobody took the previous frame: give its buffer back.
                    if let Some(old) = old.and_then(|a| Arc::try_unwrap(a).ok()) {
                        cap.recycle(old);
                    }
                }
                Ok(None) => errors = 0,
                Err(CaptureError::SurfaceLost) => return self.end(Ended::SurfaceLost),
                Err(e) => {
                    errors += 1;
                    if errors >= MAX_HUB_ERRORS {
                        return self.end(Ended::Failed(e.to_string()));
                    }
                    tracing::debug!(%e, "shared capture error");
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            }
            match cap.cursor() {
                Ok(c) => {
                    let mut s = lock(&self.state);
                    s.cursor = Some((c.x, c.y, c.visible));
                    if let Some(shape) = c.shape {
                        s.shape = Some(Arc::new(shape));
                        s.shape_gen = s.shape_gen.wrapping_add(1);
                    }
                }
                Err(e) => tracing::trace!(%e, "cursor poll failed"),
            }
        }
    }

    fn end(&self, why: Ended) {
        lock(&self.state).ended = Some(why);
        self.cv.notify_all();
    }
}

/// A session's handle on a shared capturer (see the module docs).
pub(crate) struct SharedCapturer {
    hub: Arc<Hub>,
    seen_frame: u64,
    seen_shape: u64,
    /// The end was reported once already.
    ended_reported: bool,
}

impl SharedCapturer {
    /// The handle of the session that opened the hub (already counted by `Hub::start`).
    fn first(hub: Arc<Hub>) -> Self {
        Self {
            hub,
            seen_frame: 0,
            seen_shape: 0,
            ended_reported: false,
        }
    }

    /// A further session joining a running hub.
    fn subscribe(hub: Arc<Hub>) -> Self {
        lock(&hub.state).subscribers += 1;
        Self::first(hub)
    }
}

impl Drop for SharedCapturer {
    fn drop(&mut self) {
        let mut s = lock(&self.hub.state);
        s.subscribers = s.subscribers.saturating_sub(1);
    }
}

impl ScreenCapturer for SharedCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let deadline = Instant::now() + timeout;
        let mut s = lock(&self.hub.state);
        loop {
            if let Some(why) = &s.ended {
                if !self.ended_reported {
                    self.ended_reported = true;
                    return Err(why.error());
                }
                return Err(CaptureError::SurfaceLost);
            }
            if s.frame_gen != self.seen_frame {
                if let Some(frame) = s.frame.clone() {
                    self.seen_frame = s.frame_gen;
                    if s.subscribers <= 1 {
                        s.frame = None;
                    }
                    let spare = s.spare.pop();
                    drop(s);
                    return Ok(Some(take_or_copy(frame, spare)));
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            s = self
                .hub
                .cv
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        let s = lock(&self.hub.state);
        let (x, y, visible) = s.cursor.unwrap_or((0, 0, false));
        let shape = if s.shape_gen != self.seen_shape {
            self.seen_shape = s.shape_gen;
            s.shape.as_deref().cloned()
        } else {
            None
        };
        Ok(CursorState {
            x,
            y,
            visible,
            shape,
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.hub.monitor
    }

    fn recycle(&mut self, frame: BgraFrame) {
        let mut s = lock(&self.hub.state);
        if s.spare.len() < MAX_SPARE {
            s.spare.push(frame);
        }
    }

    fn backend(&self) -> Option<CaptureBackend> {
        self.hub.backend
    }

    fn frame_size(&self) -> (u32, u32) {
        self.hub.frame_size
    }
}

/// The frame itself when nobody else holds it, otherwise a copy (into `spare` when its
/// buffer is big enough).
fn take_or_copy(frame: Arc<BgraFrame>, spare: Option<BgraFrame>) -> BgraFrame {
    match Arc::try_unwrap(frame) {
        Ok(f) => f,
        Err(shared) => match spare {
            Some(mut buf) => {
                buf.data.clear();
                buf.data.extend_from_slice(&shared.data);
                buf.width = shared.width;
                buf.height = shared.height;
                buf.stride = shared.stride;
                buf.timestamp_us = shared.timestamp_us;
                buf
            }
            None => (*shared).clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    struct Fake {
        mon: MonitorInfo,
        n: u32,
        /// Return `SurfaceLost` from this frame on.
        lose_at: Option<u32>,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl ScreenCapturer for Fake {
        fn next_frame(&mut self, _t: Duration) -> Result<Option<BgraFrame>, CaptureError> {
            std::thread::sleep(Duration::from_millis(2));
            self.n += 1;
            if self.lose_at.is_some_and(|l| self.n >= l) {
                return Err(CaptureError::SurfaceLost);
            }
            let mut f = BgraFrame::new_tight(4, 2);
            f.data[0] = (self.n % 256) as u8;
            Ok(Some(f))
        }

        fn cursor(&mut self) -> Result<CursorState, CaptureError> {
            // The shape is reported once, with the first frame.
            let shape = (self.n == 1).then(|| CursorShape {
                width: 1,
                height: 1,
                hotspot_x: 0,
                hotspot_y: 0,
                rgba: vec![255; 4],
            });
            Ok(CursorState {
                x: 1,
                y: 2,
                visible: true,
                shape,
            })
        }

        fn monitor(&self) -> &MonitorInfo {
            &self.mon
        }
    }

    struct Rig {
        factory: CapturerFactory,
        opened: Arc<AtomicU32>,
        dropped: Arc<AtomicBool>,
    }

    fn rig(lose_at: Option<u32>) -> Rig {
        let opened = Arc::new(AtomicU32::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let (o, d) = (opened.clone(), dropped.clone());
        let inner: CapturerFactory = Arc::new(move |index, _| {
            let first = o.fetch_add(1, Ordering::SeqCst) == 0;
            Ok(Box::new(Fake {
                mon: MonitorInfo {
                    index,
                    name: "شاشة".into(),
                    width: 4,
                    height: 2,
                    primary: true,
                    x: 0,
                    y: 0,
                },
                n: 0,
                lose_at: if first { lose_at } else { None },
                dropped: d.clone(),
            }) as Box<dyn ScreenCapturer>)
        });
        Rig {
            factory: share(inner),
            opened,
            dropped,
        }
    }

    fn next(c: &mut Box<dyn ScreenCapturer>) -> BgraFrame {
        for _ in 0..500 {
            if let Some(f) = c.next_frame(Duration::from_millis(20)).unwrap() {
                return f;
            }
        }
        panic!("no frame");
    }

    #[test]
    fn same_monitor_and_options_share_one_capturer() {
        let r = rig(None);
        let opts = CaptureOptions::max_size(1920, 1080);
        let mut a = (r.factory)(0, opts).unwrap();
        let mut b = (r.factory)(0, opts).unwrap();
        assert_eq!(r.opened.load(Ordering::SeqCst), 1);
        let fa = next(&mut a);
        let fb = next(&mut b);
        assert_eq!((fa.width, fb.height), (4, 2));
        // Frames keep coming to both, newest first.
        let fa2 = next(&mut a);
        assert_ne!(fa.data[0], fa2.data[0]);
        assert_eq!(a.monitor().name, "شاشة");
        // Another monitor or another size cap: its own capturer.
        let _c = (r.factory)(1, opts).unwrap();
        let _d = (r.factory)(0, CaptureOptions::max_size(1280, 720)).unwrap();
        assert_eq!(r.opened.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn each_handle_gets_a_changed_cursor_shape_once() {
        let r = rig(None);
        let mut a = (r.factory)(0, CaptureOptions::default()).unwrap();
        let mut b = (r.factory)(0, CaptureOptions::default()).unwrap();
        next(&mut a);
        next(&mut b);
        // The hub polls the cursor right after publishing a frame.
        std::thread::sleep(Duration::from_millis(50));
        assert!(a.cursor().unwrap().shape.is_some());
        assert!(a.cursor().unwrap().shape.is_none());
        let c = b.cursor().unwrap();
        assert!(c.shape.is_some() && c.visible && (c.x, c.y) == (1, 2));
        assert!(b.cursor().unwrap().shape.is_none());
    }

    #[test]
    fn surface_loss_reaches_every_handle_and_a_reopen_starts_a_new_capturer() {
        let r = rig(Some(20));
        let mut a = (r.factory)(0, CaptureOptions::default()).unwrap();
        let mut b = (r.factory)(0, CaptureOptions::default()).unwrap();
        let lost = |c: &mut Box<dyn ScreenCapturer>| loop {
            match c.next_frame(Duration::from_millis(20)) {
                Err(CaptureError::SurfaceLost) => return,
                Err(e) => panic!("{e}"),
                Ok(_) => {}
            }
        };
        lost(&mut a);
        lost(&mut b);
        drop((a, b));
        let mut c = (r.factory)(0, CaptureOptions::default()).unwrap();
        assert_eq!(r.opened.load(Ordering::SeqCst), 2);
        next(&mut c);
    }

    #[test]
    fn the_capturer_stops_with_its_last_handle() {
        let r = rig(None);
        let a = (r.factory)(0, CaptureOptions::default()).unwrap();
        let mut b = (r.factory)(0, CaptureOptions::default()).unwrap();
        drop(a);
        next(&mut b);
        assert!(!r.dropped.load(Ordering::SeqCst));
        drop(b);
        for _ in 0..200 {
            if r.dropped.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the real capturer outlived its last handle");
    }

    #[test]
    fn a_failing_open_is_reported_and_not_cached() {
        let calls = Arc::new(AtomicU32::new(0));
        let c2 = calls.clone();
        let f = share(Arc::new(move |_, _| {
            c2.fetch_add(1, Ordering::SeqCst);
            Err(CaptureError::NoSuchMonitor(9))
        }));
        assert!(matches!(
            f(9, CaptureOptions::default()),
            Err(CaptureError::NoSuchMonitor(9))
        ));
        assert!(f(9, CaptureOptions::default()).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
