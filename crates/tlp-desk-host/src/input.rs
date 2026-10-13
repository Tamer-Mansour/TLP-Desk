//! Input injection on a dedicated std thread (`SendInput` must not run on the async runtime),
//! with local-user priority and the remote pointer marker.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_input::{InputEvent, InputInjector, LocalActivity, MonitorRect, PointerMarker};
use tokio::sync::{mpsc, watch};

use crate::priority::PriorityGate;
use crate::{InjectorFactory, LocalActivityFactory, PointerMarkerFactory};

/// How often the thread re-evaluates priority / marker visibility without remote input.
const TICK: Duration = Duration::from_millis(40);

pub(crate) struct InputCtx {
    pub factory: InjectorFactory,
    pub rect: watch::Receiver<Option<MonitorRect>>,
    pub events: Receiver<InputEvent>,
    /// Grace window for local priority; zero disables it.
    pub grace: Duration,
    pub activity_factory: Option<LocalActivityFactory>,
    pub marker_factory: Option<PointerMarkerFactory>,
    /// Shown under the green marker (the viewer's name).
    pub label: String,
    /// Local priority changes, for the control loop to forward to the viewer.
    pub priority: mpsc::UnboundedSender<bool>,
}

struct Injection {
    factory: InjectorFactory,
    injector: Option<Box<dyn InputInjector>>,
    current: Option<MonitorRect>,
}

impl Injection {
    /// Follows the captured monitor: retargets the injector in place when it supports it,
    /// otherwise recreates it. A failed creation is not retried until the rectangle changes.
    fn sync_rect(&mut self, latest: Option<MonitorRect>) {
        if latest == self.current {
            return;
        }
        self.current = latest;
        if let (Some(r), Some(i)) = (latest, self.injector.as_mut()) {
            if i.set_monitor(r) {
                tracing::debug!(?r, "input injector retargeted");
                return;
            }
        }
        if let Some(mut old) = self.injector.take() {
            let _ = old.release_all();
        }
        if let Some(r) = latest {
            match (self.factory)(r) {
                Ok(i) => {
                    tracing::debug!(?r, "input injector ready");
                    self.injector = Some(i);
                }
                Err(e) => tracing::warn!(%e, "cannot create input injector"),
            }
        }
    }

    fn inject(&mut self, ev: &InputEvent) {
        match self.injector.as_mut() {
            Some(i) => {
                if let Err(e) = i.inject(ev) {
                    tracing::debug!(%e, ?ev, "input injection failed");
                }
            }
            None => tracing::trace!(?ev, "input dropped: no injector yet"),
        }
    }

    fn release_all(&mut self) {
        if let Some(i) = self.injector.as_mut() {
            if let Err(e) = i.release_all() {
                tracing::debug!(%e, "release_all failed");
            }
        }
    }
}

/// Injects events until the sender is dropped, then releases everything still pressed.
///
/// The injector is created lazily once the video pipeline has published the captured
/// monitor's rectangle, and follows it when it changes (monitor switch, resolution change,
/// desktop switch). The local activity monitor and the marker window are started lazily on
/// the first remote input event (only permitted events reach this thread) and stopped when
/// the session ends.
pub(crate) fn run(ctx: InputCtx) {
    let InputCtx {
        factory,
        mut rect,
        events,
        grace,
        activity_factory,
        marker_factory,
        label,
        priority,
    } = ctx;
    let mut inj = Injection {
        factory,
        injector: None,
        current: None,
    };
    let mut gate = PriorityGate::new(grace);
    let mut activity: Option<Arc<dyn LocalActivity>> = None;
    let mut activity_tried = false;
    let mut marker: Option<Box<dyn PointerMarker>> = None;
    let mut marker_tried = false;
    let mut marker_shown = false;

    loop {
        let received = match events.recv_timeout(TICK) {
            Ok(ev) => Some(ev),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if received.is_some() && !activity_tried && !grace.is_zero() {
            activity_tried = true;
            if let Some(f) = &activity_factory {
                match f() {
                    Ok(a) => {
                        tracing::debug!("local activity monitor started");
                        activity = Some(a);
                    }
                    Err(e) => tracing::warn!(%e, "local input priority unavailable"),
                }
            }
        }
        let now = Instant::now();
        let last_local = activity.as_ref().and_then(|a| a.last_physical_input());

        let (step, ev) = match received {
            Some(ev) => {
                let (step, forward) = gate.filter(now, last_local, &ev);
                (step, forward.then_some(ev))
            }
            None => (gate.update(now, last_local), None),
        };
        if step.release_remote {
            tracing::info!("local user took over the input; releasing remote keys/buttons");
            inj.release_all();
        }
        if let Some(active) = step.changed {
            if !active {
                tracing::info!("local user idle; remote input resumes");
            }
            let _ = priority.send(active);
        }
        if let Some(ev) = ev {
            inj.sync_rect(*rect.borrow_and_update());
            inj.inject(&ev);
            if !marker_tried && matches!(ev, InputEvent::MouseMove { .. }) {
                marker_tried = true;
                if let Some(f) = &marker_factory {
                    match f(&label) {
                        Ok(m) => {
                            if !m.excluded_from_capture() {
                                tracing::warn!("pointer marker could not be excluded from capture; the viewer may see it");
                            }
                            marker = Some(m);
                        }
                        Err(e) => tracing::warn!(%e, "remote pointer marker unavailable"),
                    }
                }
            }
        } else if rect.has_changed().unwrap_or(false) {
            inj.sync_rect(*rect.borrow_and_update());
        }

        let want = gate.marker_visible(Instant::now());
        if want != marker_shown {
            marker_shown = want;
            if let Some(m) = marker.as_mut() {
                m.set_visible(want);
            }
        }
    }
    if let Some(m) = marker.as_mut() {
        m.set_visible(false);
    }
    drop(marker);
    drop(activity);
    if let Some(mut i) = inj.injector {
        if let Err(e) = i.release_all() {
            tracing::debug!(%e, "release_all failed");
        }
    }
    tracing::debug!("input thread stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tlp_desk_input::InputError;

    #[derive(Default)]
    struct FakeLocal(Mutex<Option<Instant>>);
    impl LocalActivity for FakeLocal {
        fn last_physical_input(&self) -> Option<Instant> {
            *self.0.lock().unwrap()
        }
    }

    type Log = Arc<Mutex<Vec<String>>>;

    struct RecInjector(Log);
    impl InputInjector for RecInjector {
        fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
            self.0.lock().unwrap().push(format!("{event:?}"));
            Ok(())
        }
    }

    struct RecMarker(Log);
    impl PointerMarker for RecMarker {
        fn set_visible(&mut self, visible: bool) {
            self.0.lock().unwrap().push(format!("marker {visible}"));
        }
    }

    struct Rig {
        tx: std::sync::mpsc::Sender<InputEvent>,
        log: Log,
        local: Arc<FakeLocal>,
        prio: mpsc::UnboundedReceiver<bool>,
        thread: std::thread::JoinHandle<()>,
        _rect: watch::Sender<Option<MonitorRect>>,
    }

    fn rig(grace: Duration) -> Rig {
        let log: Log = Arc::default();
        let local = Arc::new(FakeLocal::default());
        let (tx, events) = std::sync::mpsc::channel();
        let (rect_tx, rect) = watch::channel(Some(MonitorRect::new(0, 0, 100, 100)));
        let (ptx, prio) = mpsc::unbounded_channel();
        let ctx = InputCtx {
            factory: {
                let log = log.clone();
                Arc::new(move |_| Ok(Box::new(RecInjector(log.clone())) as Box<dyn InputInjector>))
            },
            rect,
            events,
            grace,
            activity_factory: Some({
                let local = local.clone();
                Arc::new(move || Ok(local.clone() as Arc<dyn LocalActivity>))
            }),
            marker_factory: Some({
                let log = log.clone();
                Arc::new(move |_: &str| {
                    log.lock().unwrap().push("marker created".into());
                    Ok(Box::new(RecMarker(log.clone())) as Box<dyn PointerMarker>)
                })
            }),
            label: "viewer".into(),
            priority: ptx,
        };
        let thread = std::thread::spawn(move || run(ctx));
        Rig {
            tx,
            log,
            local,
            prio,
            thread,
            _rect: rect_tx,
        }
    }

    fn wait_for(log: &Log, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if log.lock().unwrap().iter().any(|l| l == what) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("never saw {what:?} in {:?}", log.lock().unwrap());
    }

    /// The marker window is never created or shown without remote pointer movement (keys
    /// alone, or no input at all), and nothing is left visible when the session ends.
    #[test]
    fn marker_not_created_without_remote_pointer_moves() {
        let r = rig(Duration::from_millis(300));
        std::thread::sleep(Duration::from_millis(120));
        let key = InputEvent::Key {
            scancode: 0x1e,
            extended: false,
            down: true,
        };
        r.tx.send(key.clone()).unwrap();
        wait_for(&r.log, &format!("{key:?}"));
        std::thread::sleep(Duration::from_millis(120));
        drop(r.tx);
        r.thread.join().unwrap();
        let log = r.log.lock().unwrap();
        assert!(!log.iter().any(|l| l.starts_with("marker")), "{log:?}");
    }

    /// The marker hides on its own ~1.5 s after the last remote move.
    #[test]
    fn marker_hides_after_remote_pointer_goes_idle() {
        let r = rig(Duration::from_millis(300));
        r.tx.send(InputEvent::MouseMove { x: 1, y: 1 }).unwrap();
        wait_for(&r.log, "marker true");
        let shown = Instant::now();
        wait_for(&r.log, "marker false");
        let held = shown.elapsed();
        assert!(held >= Duration::from_millis(1300), "{held:?}");
        drop(r.tx);
        r.thread.join().unwrap();
    }

    /// Full thread behaviour with fake hooks/marker: marker shows for remote moves, local
    /// input releases the remote's keys, hides the marker, blocks remote input and notifies
    /// the viewer; after the grace window remote input flows again.
    #[test]
    fn local_takeover_and_resume_on_input_thread() {
        let mut r = rig(Duration::from_millis(300));
        let key = InputEvent::Key {
            scancode: 0x1e,
            extended: false,
            down: true,
        };
        r.tx.send(InputEvent::MouseMove { x: 5, y: 5 }).unwrap();
        r.tx.send(key.clone()).unwrap();
        wait_for(&r.log, &format!("{key:?}"));
        wait_for(&r.log, "marker true");

        *r.local.0.lock().unwrap() = Some(Instant::now());
        wait_for(&r.log, "ReleaseAll");
        wait_for(&r.log, "marker false");
        assert_eq!(r.prio.blocking_recv(), Some(true));
        let n = r.log.lock().unwrap().len();
        r.tx.send(InputEvent::MouseMove { x: 9, y: 9 }).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            r.log.lock().unwrap().len(),
            n,
            "remote input leaked during grace"
        );

        assert_eq!(r.prio.blocking_recv(), Some(false));
        r.tx.send(InputEvent::MouseMove { x: 7, y: 7 }).unwrap();
        wait_for(&r.log, "MouseMove { x: 7, y: 7 }");
        drop(r.tx);
        r.thread.join().unwrap();
        let log = r.log.lock().unwrap();
        assert_eq!(log.last().map(String::as_str), Some("ReleaseAll"));
        assert!(!log.iter().any(|l| l == "MouseMove { x: 9, y: 9 }"));
    }
}
