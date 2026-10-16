//! Shared hooks with fake activity monitors and markers (no OS hooks, no windows).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tlp_desk_input::{InputError, LocalActivity, PointerMarker};

use super::*;

type Log = Arc<Mutex<Vec<String>>>;

struct FakeActivity {
    stopped: Arc<AtomicUsize>,
}

impl LocalActivity for FakeActivity {
    fn last_physical_input(&self) -> Option<Instant> {
        None
    }
}

impl Drop for FakeActivity {
    fn drop(&mut self) {
        self.stopped.fetch_add(1, Ordering::SeqCst);
    }
}

struct FakeMarker {
    log: Log,
}

impl PointerMarker for FakeMarker {
    fn set_visible(&mut self, visible: bool) {
        self.log.lock().unwrap().push(format!("visible {visible}"));
    }

    fn set_label(&mut self, label: &str) {
        self.log.lock().unwrap().push(format!("label {label}"));
    }
}

struct Rig {
    hooks: Arc<SharedHooks>,
    log: Log,
    activity_created: Arc<AtomicUsize>,
    activity_stopped: Arc<AtomicUsize>,
    marker_created: Arc<AtomicUsize>,
}

fn rig() -> Rig {
    let log: Log = Arc::default();
    let activity_created = Arc::new(AtomicUsize::new(0));
    let activity_stopped = Arc::new(AtomicUsize::new(0));
    let marker_created = Arc::new(AtomicUsize::new(0));
    let hooks = SharedHooks::new(
        {
            let (c, s) = (activity_created.clone(), activity_stopped.clone());
            Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(FakeActivity { stopped: s.clone() }) as Arc<dyn LocalActivity>)
            })
        },
        {
            let (c, log) = (marker_created.clone(), log.clone());
            Arc::new(move |label: &str| {
                c.fetch_add(1, Ordering::SeqCst);
                log.lock().unwrap().push(format!("created {label}"));
                Ok(Box::new(FakeMarker { log: log.clone() }) as Box<dyn PointerMarker>)
            })
        },
    );
    Rig {
        hooks,
        log,
        activity_created,
        activity_stopped,
        marker_created,
    }
}

impl Rig {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[test]
fn two_sessions_share_one_activity_monitor() {
    let r = rig();
    let a = r.hooks.activity().expect("first lease");
    let b = r.hooks.activity().expect("second lease");
    assert_eq!(r.activity_created.load(Ordering::SeqCst), 1, "one hook set");
    assert!(Arc::ptr_eq(a.get(), b.get()));
    assert_eq!(r.hooks.activity_users(), 2);
    drop(a);
    assert_eq!(
        r.activity_stopped.load(Ordering::SeqCst),
        0,
        "still used by the other session"
    );
    drop(b);
    assert_eq!(r.activity_stopped.load(Ordering::SeqCst), 1);
    // A later session starts a fresh monitor.
    let _c = r.hooks.activity().expect("new lease");
    assert_eq!(r.activity_created.load(Ordering::SeqCst), 2);
}

#[test]
fn failing_activity_factory_gives_no_lease_and_is_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hooks = SharedHooks::new(
        {
            let calls = calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(InputError::Unsupported)
            })
        },
        Arc::new(|_: &str| Err(InputError::Unsupported)),
    );
    assert!(hooks.activity().is_none());
    assert!(hooks.activity().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(hooks.marker(1, "x").is_none());
    assert_eq!(hooks.activity_users(), 0);
}

#[test]
fn two_viewers_share_one_marker_and_it_shows_both_names() {
    let r = rig();
    let ali = r.hooks.marker(1, "Ali").expect("marker");
    let sara = r.hooks.marker(2, "سارة").expect("marker");
    assert_eq!(r.marker_created.load(Ordering::SeqCst), 1, "one window");

    ali.set_visible(true);
    assert_eq!(r.log(), vec!["created Ali", "visible true"]);
    sara.set_visible(true);
    assert_eq!(
        r.log().last().map(String::as_str),
        Some("label Ali, سارة"),
        "{:?}",
        r.log()
    );
    // One of them going idle keeps the marker for the other, labelled with that one only.
    ali.set_visible(false);
    let log = r.log();
    assert_eq!(
        log.last().map(String::as_str),
        Some("label سارة"),
        "{log:?}"
    );
    assert_eq!(log.iter().filter(|l| *l == "visible false").count(), 0);
    sara.set_visible(false);
    assert_eq!(r.log().last().map(String::as_str), Some("visible false"));
}

#[test]
fn setting_the_same_state_twice_does_nothing() {
    let r = rig();
    let a = r.hooks.marker(1, "Ali").unwrap();
    a.set_visible(true);
    a.set_visible(true);
    a.set_visible(false);
    a.set_visible(false);
    assert_eq!(
        r.log(),
        vec!["created Ali", "visible true", "visible false"]
    );
}

#[test]
fn marker_is_removed_with_the_last_session_and_hidden_first() {
    let r = rig();
    let a = r.hooks.marker(1, "Ali").unwrap();
    let b = r.hooks.marker(2, "Bo").unwrap();
    a.set_visible(true);
    drop(a);
    assert!(
        r.hooks.marker_exists(),
        "the other viewer still has a lease"
    );
    // The departing viewer no longer keeps the marker visible.
    assert_eq!(r.log().last().map(String::as_str), Some("visible false"));
    drop(b);
    assert!(!r.hooks.marker_exists());
    // A new session creates a new marker.
    let _c = r.hooks.marker(3, "Cy").unwrap();
    assert_eq!(r.marker_created.load(Ordering::SeqCst), 2);
}

#[test]
fn many_movers_name_at_most_three_viewers() {
    let r = rig();
    let leases: Vec<_> = ["A", "B", "C", "D"]
        .iter()
        .enumerate()
        .map(|(i, n)| r.hooks.marker(i as u64, n).unwrap())
        .collect();
    for l in &leases {
        l.set_visible(true);
    }
    assert_eq!(r.log().last().map(String::as_str), Some("label A, B, C"));
}
