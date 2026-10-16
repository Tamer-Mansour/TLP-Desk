//! Input fakes: a recording injector, a controllable local-activity source and a pointer
//! marker without a window.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use tlp_desk_host::{InjectorFactory, LocalActivityFactory, PointerMarkerFactory};
use tlp_desk_input::{
    InputError, InputEvent, InputInjector, LocalActivity, MonitorRect, PointerMarker,
};

use super::{Alive, AliveGuard, Kind};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the host injected, in order, plus the monitor rectangles injectors were made for.
/// Clones share state.
#[derive(Debug, Clone, Default)]
pub struct Recorded {
    events: Arc<Mutex<Vec<InputEvent>>>,
    rects: Arc<Mutex<Vec<MonitorRect>>>,
}

impl Recorded {
    /// Every injected event so far.
    pub fn events(&self) -> Vec<InputEvent> {
        lock(&self.events).clone()
    }

    /// Number of injected events.
    pub fn len(&self) -> usize {
        lock(&self.events).len()
    }

    /// Whether nothing was injected.
    pub fn is_empty(&self) -> bool {
        lock(&self.events).is_empty()
    }

    /// Forgets the events recorded so far.
    pub fn clear(&self) {
        lock(&self.events).clear();
    }

    /// Monitor rectangles the injector factory was asked for, in order.
    pub fn rects(&self) -> Vec<MonitorRect> {
        lock(&self.rects).clone()
    }
}

/// An [`InputInjector`] that records instead of injecting.
#[derive(Debug)]
pub struct RecordingInjector {
    rec: Recorded,
    _alive: AliveGuard,
}

impl RecordingInjector {
    /// Records into `rec`.
    pub fn new(rec: Recorded, alive: &Arc<Alive>) -> Self {
        Self {
            rec,
            _alive: AliveGuard::new(alive, Kind::Injector),
        }
    }
}

impl InputInjector for RecordingInjector {
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        lock(&self.rec.events).push(event.clone());
        Ok(())
    }
}

/// An [`InjectorFactory`] making [`RecordingInjector`]s that all record into `rec`.
pub fn injector_factory(rec: Recorded, alive: Arc<Alive>) -> InjectorFactory {
    Arc::new(move |rect| {
        lock(&rec.rects).push(rect);
        Ok(Box::new(RecordingInjector::new(rec.clone(), &alive)) as Box<dyn InputInjector>)
    })
}

/// "When did the host user last touch their own mouse/keyboard", set by the test.
#[derive(Debug, Default)]
pub struct FakeLocalActivity(Mutex<Option<Instant>>);

impl FakeLocalActivity {
    /// The host user just moved the mouse.
    pub fn touch(&self) {
        *lock(&self.0) = Some(Instant::now());
    }

    /// Forget any local input (the viewer gets priority back after the grace period).
    pub fn reset(&self) {
        *lock(&self.0) = None;
    }
}

impl LocalActivity for FakeLocalActivity {
    fn last_physical_input(&self) -> Option<Instant> {
        *lock(&self.0)
    }
}

/// A [`LocalActivityFactory`] handing out `local` (no low-level hooks are installed).
pub fn local_activity_factory(local: Arc<FakeLocalActivity>) -> LocalActivityFactory {
    Arc::new(move || Ok(local.clone() as Arc<dyn LocalActivity>))
}

/// A [`PointerMarker`] that only remembers whether it is shown.
#[derive(Debug)]
pub struct FakePointerMarker {
    visible: Arc<Mutex<bool>>,
    _alive: AliveGuard,
}

impl PointerMarker for FakePointerMarker {
    fn set_visible(&mut self, visible: bool) {
        *lock(&self.visible) = visible;
    }
}

/// A [`PointerMarkerFactory`] making [`FakePointerMarker`]s that report into `visible`.
pub fn pointer_marker_factory(
    visible: Arc<Mutex<bool>>,
    alive: Arc<Alive>,
) -> PointerMarkerFactory {
    Arc::new(move |_label| {
        Ok(Box::new(FakePointerMarker {
            visible: visible.clone(),
            _alive: AliveGuard::new(&alive, Kind::Marker),
        }) as Box<dyn PointerMarker>)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn injector_records_in_order() {
        let rec = Recorded::default();
        let alive = Arc::new(Alive::default());
        let f = injector_factory(rec.clone(), alive.clone());
        let mut inj = f(MonitorRect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        })
        .unwrap();
        inj.inject(&InputEvent::MouseMove { x: 1, y: 2 }).unwrap();
        inj.release_all().unwrap();
        assert_eq!(
            rec.events(),
            vec![InputEvent::MouseMove { x: 1, y: 2 }, InputEvent::ReleaseAll]
        );
        assert_eq!(rec.rects().len(), 1);
        assert_eq!(alive.injectors(), 1);
        drop(inj);
        assert_eq!(alive.injectors(), 0);
    }

    #[test]
    fn local_activity_is_controllable() {
        let l = FakeLocalActivity::default();
        assert!(!l.is_local_active(Duration::from_secs(1)));
        l.touch();
        assert!(l.is_local_active(Duration::from_secs(1)));
        l.reset();
        assert!(l.last_physical_input().is_none());
    }
}
