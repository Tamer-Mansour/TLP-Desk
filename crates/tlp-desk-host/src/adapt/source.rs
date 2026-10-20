//! Opening the capture source for the current quality target: one monitor, or all monitors
//! composed into one picture.

use tlp_desk_capture::{
    virtual_bounds, AllMonitorsCapturer, CaptureError, CaptureOptions, MonitorInfo, ScreenCapturer,
    ALL_MONITORS,
};

use super::quality::QualityTarget;
use crate::{CapturerFactory, MonitorLister};

/// An open capture source and the size cap it was opened with.
pub(crate) struct Source {
    pub(crate) cap: Box<dyn ScreenCapturer>,
    pub(crate) max_size: (u32, u32),
    /// For "all monitors": the monitor layout it was composed from (re-checked for hot-plug).
    pub(crate) monitors: Option<Vec<MonitorInfo>>,
}

/// The cap a source showing `monitor` should have for this quality target and resolution step.
pub(crate) fn wanted_cap(monitor: &MonitorInfo, quality: &QualityTarget, scale: u32) -> (u32, u32) {
    quality.cap_for((monitor.width, monitor.height), scale)
}

/// Opens monitor `index` (or [`ALL_MONITORS`]) capped by `quality` (its cap oriented like the
/// monitor, the viewer's viewport, scaled by `scale` per mille).
///
/// # Errors
/// The factory's error; [`CaptureError::NoSuchMonitor`] when "all monitors" is asked for and
/// the monitor list is unavailable or empty.
pub(crate) fn open_source(
    factory: &CapturerFactory,
    lister: &MonitorLister,
    index: u32,
    quality: &QualityTarget,
    scale: u32,
) -> Result<Source, CaptureError> {
    if index == ALL_MONITORS {
        let list = lister()?;
        let bounds = virtual_bounds(&list).ok_or(CaptureError::NoSuchMonitor(ALL_MONITORS))?;
        let max_size = wanted_cap(&bounds, quality, scale);
        let mut open = |i: u32, o: CaptureOptions| factory(i, o);
        let cap = AllMonitorsCapturer::open(&list, Some(max_size), &mut open)?;
        return Ok(Source {
            cap: Box::new(cap),
            max_size,
            monitors: Some(list),
        });
    }
    // Orient the cap like the monitor if we know its size; otherwise just scale it.
    let monitor = lister()
        .ok()
        .and_then(|l| l.into_iter().find(|m| m.index == index));
    let max_size = match &monitor {
        Some(m) => wanted_cap(m, quality, scale),
        None => quality.cap_for(quality.max_size, scale),
    };
    let cap = factory(index, CaptureOptions::max_size(max_size.0, max_size.1))?;
    Ok(Source {
        cap,
        max_size,
        monitors: None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn mon(index: u32, x: i32, width: u32, height: u32) -> MonitorInfo {
        MonitorInfo {
            index,
            name: format!("M{index}"),
            width,
            height,
            primary: index == 0,
            x,
            y: 0,
        }
    }

    struct Fake(MonitorInfo);

    impl ScreenCapturer for Fake {
        fn next_frame(
            &mut self,
            _: std::time::Duration,
        ) -> Result<Option<tlp_desk_capture::BgraFrame>, CaptureError> {
            Ok(None)
        }
        fn cursor(&mut self) -> Result<tlp_desk_capture::CursorState, CaptureError> {
            Err(CaptureError::Unsupported)
        }
        fn monitor(&self) -> &MonitorInfo {
            &self.0
        }
    }

    type Asked = Arc<Mutex<Vec<(u32, Option<(u32, u32)>)>>>;

    fn target(cap: (u32, u32)) -> QualityTarget {
        QualityTarget::host_default(cap, 60)
    }

    fn factory(list: Vec<MonitorInfo>) -> (CapturerFactory, Asked) {
        let asked: Asked = Arc::default();
        let a = asked.clone();
        let f: CapturerFactory = Arc::new(move |i, o: CaptureOptions| {
            a.lock().unwrap().push((i, o.max_size));
            let m = list
                .iter()
                .find(|m| m.index == i)
                .cloned()
                .ok_or(CaptureError::NoSuchMonitor(i))?;
            Ok(Box::new(Fake(m)) as Box<dyn ScreenCapturer>)
        });
        (f, asked)
    }

    #[test]
    fn single_monitor_cap_is_oriented_and_scaled() {
        let list = vec![mon(0, 0, 3840, 2160), mon(1, 3840, 2160, 3840)];
        let (f, asked) = factory(list.clone());
        let l: MonitorLister = Arc::new(move || Ok(list.clone()));
        let s = open_source(&f, &l, 1, &target((1920, 1200)), 1000).unwrap();
        assert_eq!(s.max_size, (1200, 1920));
        let s = open_source(&f, &l, 0, &target((1920, 1200)), 500).unwrap();
        assert_eq!(s.max_size, (960, 600));
        assert_eq!(
            *asked.lock().unwrap(),
            vec![(1, Some((1200, 1920))), (0, Some((960, 600)))]
        );
        // Unknown size (lister failed): the cap is used as given.
        let broken: MonitorLister = Arc::new(|| Err(CaptureError::Unsupported));
        let s = open_source(&f, &broken, 0, &target((1280, 720)), 1000).unwrap();
        assert_eq!(s.max_size, (1280, 720));
        assert!(matches!(
            open_source(&f, &broken, ALL_MONITORS, &target((1280, 720)), 1000),
            Err(CaptureError::Unsupported)
        ));
    }

    #[test]
    fn all_monitors_open_every_monitor_into_the_bounding_box() {
        let list = vec![mon(0, 0, 1920, 1080), mon(1, -1920, 1920, 1080)];
        let (f, asked) = factory(list.clone());
        let l: MonitorLister = Arc::new(move || Ok(list.clone()));
        let s = open_source(&f, &l, ALL_MONITORS, &target((1920, 1200)), 1000).unwrap();
        assert_eq!(s.max_size, (1920, 1200));
        let m = s.cap.monitor();
        assert_eq!((m.x, m.y, m.width, m.height), (-1920, 0, 3840, 1080));
        assert_eq!(s.cap.frame_size(), (1920, 540));
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 2);
        assert!(asked.iter().all(|(_, c)| *c == Some((960, 540))));
        let empty: MonitorLister = Arc::new(|| Ok(Vec::new()));
        assert!(matches!(
            open_source(&f, &empty, ALL_MONITORS, &target((1920, 1200)), 1000),
            Err(CaptureError::NoSuchMonitor(ALL_MONITORS))
        ));
    }
}
