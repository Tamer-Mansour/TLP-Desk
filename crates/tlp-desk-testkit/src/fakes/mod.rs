//! Fakes for every hardware-facing piece of a session. They never touch the real desktop.
//!
//! The `*_factory` functions return the factory types [`tlp_desk_host::HostConfig`] expects,
//! so a test can swap one piece at a time:
//!
//! ```no_run
//! # use std::sync::Arc;
//! use tlp_desk_testkit::fakes;
//! let alive = Arc::new(fakes::Alive::default());
//! let cfg = tlp_desk_host::HostConfig {
//!     capturer_factory: fakes::capturer_factory(fakes::monitors(2), alive.clone()),
//!     monitor_lister: fakes::monitor_lister(fakes::monitors(2)),
//!     ..tlp_desk_host::HostConfig::default()
//! };
//! ```

mod audio;
mod capture;
mod clipboard;
mod encoder;
mod input;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub use audio::FakeAudioSource;
pub use capture::{capturer_factory, monitor, monitor_lister, monitors, FakeCapturer};
pub use capture::{DEFAULT_FPS, DEFAULT_H, DEFAULT_W};
pub use clipboard::{ClipboardContent, FakeClipboard};
pub use encoder::{encoder_factory, FakeEncoder};
pub use input::{
    injector_factory, local_activity_factory, pointer_marker_factory, FakeLocalActivity,
    FakePointerMarker, Recorded, RecordingInjector,
};

/// Live instances of each fake, shared by every fake created from one set of factories.
///
/// The host creates its capturer, encoder and injector on per-session threads and drops them
/// when those threads exit, so "all counts back to zero" after a session ended proves the
/// threads finished. Creating one `Alive` per test keeps parallel tests independent.
#[derive(Debug, Default)]
pub struct Alive {
    capturers: AtomicUsize,
    encoders: AtomicUsize,
    injectors: AtomicUsize,
    markers: AtomicUsize,
    /// How many of each were ever created.
    created: AtomicUsize,
}

/// Which counter a [`AliveGuard`] holds.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Capturer,
    Encoder,
    Injector,
    Marker,
}

impl Alive {
    fn counter(&self, kind: Kind) -> &AtomicUsize {
        match kind {
            Kind::Capturer => &self.capturers,
            Kind::Encoder => &self.encoders,
            Kind::Injector => &self.injectors,
            Kind::Marker => &self.markers,
        }
    }

    /// Live fake capturers.
    pub fn capturers(&self) -> usize {
        self.capturers.load(Ordering::Acquire)
    }

    /// Live fake encoders.
    pub fn encoders(&self) -> usize {
        self.encoders.load(Ordering::Acquire)
    }

    /// Live recording injectors.
    pub fn injectors(&self) -> usize {
        self.injectors.load(Ordering::Acquire)
    }

    /// Live fake pointer markers.
    pub fn markers(&self) -> usize {
        self.markers.load(Ordering::Acquire)
    }

    /// All live fakes together.
    pub fn total(&self) -> usize {
        self.capturers() + self.encoders() + self.injectors() + self.markers()
    }

    /// Fakes ever created (all kinds).
    pub fn created(&self) -> usize {
        self.created.load(Ordering::Acquire)
    }
}

/// Counts one live fake until dropped.
#[derive(Debug)]
struct AliveGuard {
    alive: Arc<Alive>,
    kind: Kind,
}

impl AliveGuard {
    fn new(alive: &Arc<Alive>, kind: Kind) -> Self {
        alive.counter(kind).fetch_add(1, Ordering::AcqRel);
        alive.created.fetch_add(1, Ordering::AcqRel);
        Self {
            alive: alive.clone(),
            kind,
        }
    }
}

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.alive.counter(self.kind).fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alive_counts_follow_guards() {
        let alive = Arc::new(Alive::default());
        let a = AliveGuard::new(&alive, Kind::Capturer);
        let b = AliveGuard::new(&alive, Kind::Injector);
        assert_eq!(
            (alive.capturers(), alive.injectors(), alive.total()),
            (1, 1, 2)
        );
        drop(a);
        assert_eq!(alive.total(), 1);
        drop(b);
        assert_eq!(alive.total(), 0);
        assert_eq!(alive.created(), 2);
    }
}
