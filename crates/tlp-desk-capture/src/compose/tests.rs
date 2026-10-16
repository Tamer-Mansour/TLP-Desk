use super::*;

fn mon(index: u32, x: i32, y: i32, width: u32, height: u32) -> MonitorInfo {
    MonitorInfo {
        index,
        name: format!("FAKE{index}"),
        width,
        height,
        primary: x == 0 && y == 0,
        x,
        y,
    }
}

/// A monitor that shows one solid colour, honours `max_size` like the real capturers and
/// reports a scripted cursor.
struct Solid {
    info: MonitorInfo,
    size: (u32, u32),
    color: [u8; 4],
    pending: bool,
    cursor: CursorState,
}

impl ScreenCapturer for Solid {
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        if !std::mem::take(&mut self.pending) {
            return Ok(None);
        }
        let mut f = BgraFrame::new_tight(self.size.0, self.size.1);
        for px in f.data.chunks_exact_mut(4) {
            px.copy_from_slice(&self.color);
        }
        Ok(Some(f))
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        Ok(CursorState {
            shape: self.cursor.shape.take(),
            ..self.cursor.clone()
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.info
    }

    fn frame_size(&self) -> (u32, u32) {
        self.size
    }
}

fn hidden() -> CursorState {
    CursorState {
        x: 0,
        y: 0,
        visible: false,
        shape: None,
    }
}

const RED: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

fn open_all(
    monitors: &[MonitorInfo],
    cap: Option<(u32, u32)>,
    asked: &mut Vec<(u32, Option<(u32, u32)>)>,
) -> AllMonitorsCapturer {
    let list = monitors.to_vec();
    let mut open = |i: u32, o: CaptureOptions| -> Result<Box<dyn ScreenCapturer>, CaptureError> {
        asked.push((i, o.max_size));
        let info = list
            .iter()
            .find(|m| m.index == i)
            .cloned()
            .ok_or(CaptureError::NoSuchMonitor(i))?;
        let size = o.max_size.map_or((info.width, info.height), |c| {
            fit_within((info.width, info.height), c)
        });
        Ok(Box::new(Solid {
            info,
            size,
            color: if i == 0 { RED } else { GREEN },
            pending: true,
            cursor: hidden(),
        }))
    };
    AllMonitorsCapturer::open(monitors, cap, &mut open).expect("open all monitors")
}

fn px(f: &BgraFrame, x: u32, y: u32) -> [u8; 4] {
    let i = x as usize * 4;
    f.row(y)[i..i + 4].try_into().unwrap()
}

#[test]
fn bounds_include_negative_origins() {
    let list = [mon(0, 0, 0, 1920, 1080), mon(1, -1280, -200, 1280, 1024)];
    let b = virtual_bounds(&list).unwrap();
    assert_eq!((b.x, b.y, b.width, b.height), (-1280, -200, 3200, 1280));
    assert_eq!(b.index, ALL_MONITORS);
    assert!(virtual_bounds(&[]).is_none());
    assert!(virtual_bounds(&[mon(0, 0, 0, 0, 0)]).is_none());
}

#[test]
fn side_by_side_layout_tiles_exactly() {
    let list = [mon(0, 0, 0, 1920, 1080), mon(1, 1920, 0, 1920, 1080)];
    let l = ComposeLayout::new(&list, Some((1920, 1200))).unwrap();
    assert_eq!(l.canvas, (1920, 540));
    assert_eq!(
        l.parts,
        vec![
            Placement {
                index: 0,
                x: 0,
                y: 0,
                width: 960,
                height: 540
            },
            Placement {
                index: 1,
                x: 960,
                y: 0,
                width: 960,
                height: 540
            },
        ]
    );
    // No cap: native size.
    let l = ComposeLayout::new(&list, None).unwrap();
    assert_eq!(l.canvas, (3840, 1080));
}

#[test]
fn negative_origin_layout_has_no_gaps_or_overlaps() {
    // Secondary monitor left of and above the primary.
    let list = [mon(0, 0, 0, 2560, 1440), mon(1, -1920, -300, 1920, 1080)];
    let l = ComposeLayout::new(&list, Some((1920, 1200))).unwrap();
    let b = &l.bounds;
    assert_eq!((b.x, b.y, b.width, b.height), (-1920, -300, 4480, 1740));
    let (p0, p1) = (l.parts[0], l.parts[1]);
    // The secondary starts at the canvas origin; the primary starts where it ends.
    assert_eq!((p1.x, p1.y), (0, 0));
    assert_eq!(p1.x + p1.width, p0.x);
    assert!(p0.x + p0.width <= l.canvas.0 && p0.y + p0.height <= l.canvas.1);
    assert_eq!(p0.x + p0.width, l.canvas.0);
    assert_eq!(p0.y + p0.height, l.canvas.1);
}

#[test]
fn composes_each_monitor_into_its_slot_and_leaves_gaps_black() {
    // Primary 1920x1080 at 0,0; a smaller monitor to the right, lower down.
    let list = [mon(0, 0, 0, 1920, 1080), mon(1, 1920, 200, 1280, 720)];
    let mut asked = Vec::new();
    let mut cap = open_all(&list, Some((1600, 1000)), &mut asked);
    let layout = cap.layout().clone();
    assert_eq!(cap.monitor().index, ALL_MONITORS);
    assert_eq!(cap.frame_size(), layout.canvas);
    // Each monitor was opened capped to its slot (the GPU does the scaling).
    assert_eq!(asked.len(), 2);
    for (part, (i, max)) in layout.parts.iter().zip(&asked) {
        assert_eq!(*i, part.index);
        assert_eq!(*max, Some((part.width, part.height)));
    }
    let f = cap
        .next_frame(Duration::from_millis(10))
        .unwrap()
        .expect("frame");
    assert_eq!((f.width, f.height), layout.canvas);
    assert!(f.is_well_formed());
    let (p0, p1) = (layout.parts[0], layout.parts[1]);
    assert_eq!(px(&f, p0.x + 1, p0.y + 1), RED);
    assert_eq!(px(&f, p0.x + p0.width - 2, p0.y + p0.height - 2), RED);
    assert_eq!(px(&f, p1.x + 1, p1.y + 1), GREEN);
    assert_eq!(px(&f, p1.x + p1.width - 2, p1.y + p1.height - 2), GREEN);
    // Above the smaller monitor there is no screen: opaque black.
    assert_eq!(px(&f, p1.x + 5, 0), [0, 0, 0, 255]);
    // Nothing new: no frame; the recycled buffer is reused.
    cap.recycle(f);
    assert!(cap.next_frame(Duration::from_millis(5)).unwrap().is_none());
}

#[test]
fn cursor_is_translated_to_virtual_desktop_and_shape_follows_monitor() {
    let list = [mon(0, 0, 0, 1920, 1080), mon(1, -1280, -100, 1280, 1024)];
    let shape = |w| CursorShape {
        width: w,
        height: 1,
        hotspot_x: 0,
        hotspot_y: 0,
        rgba: vec![255; w as usize * 4],
    };
    let mut parts: Vec<Box<dyn ScreenCapturer>> = Vec::new();
    for m in &list {
        parts.push(Box::new(Solid {
            info: m.clone(),
            size: (m.width, m.height),
            color: RED,
            pending: false,
            cursor: CursorState {
                x: 10,
                y: 20,
                visible: m.index == 1,
                shape: Some(shape(m.index + 1)),
            },
        }));
    }
    let mut parts = parts.into_iter();
    let mut open = |_: u32, _: CaptureOptions| parts.next().ok_or(CaptureError::Unsupported);
    let mut cap = AllMonitorsCapturer::open(&list, None, &mut open).unwrap();
    // Cursor on monitor 1 (left of the primary): bounds origin is (-1280, -100).
    let c = cap.cursor().unwrap();
    assert!(c.visible);
    assert_eq!((c.x, c.y), (10, 20));
    assert_eq!(c.shape.map(|s| s.width), Some(2));
    // Same monitor, no new shape: none re-sent.
    let c = cap.cursor().unwrap();
    assert!(c.shape.is_none());

    // Move the cursor to the primary: its position shifts by the primary's offset and its
    // remembered shape is sent again.
    let mut parts: Vec<Box<dyn ScreenCapturer>> = Vec::new();
    for m in &list {
        parts.push(Box::new(Solid {
            info: m.clone(),
            size: (m.width, m.height),
            color: RED,
            pending: false,
            cursor: CursorState {
                x: 5,
                y: 6,
                visible: m.index == 0,
                shape: Some(shape(m.index + 1)),
            },
        }));
    }
    let mut parts = parts.into_iter();
    let mut open = |_: u32, _: CaptureOptions| parts.next().ok_or(CaptureError::Unsupported);
    let mut cap = AllMonitorsCapturer::open(&list, None, &mut open).unwrap();
    let c = cap.cursor().unwrap();
    assert_eq!((c.x, c.y), (1280 + 5, 100 + 6));
    assert_eq!(c.shape.map(|s| s.width), Some(1));
}

#[test]
fn blit_scales_when_gpu_scaling_was_unavailable_and_clips_safely() {
    let mut dst = BgraFrame::new_tight(40, 20);
    let mut src = BgraFrame::new_tight(80, 40);
    for y in 0..40 {
        for (x, p) in src.row_mut(y).chunks_exact_mut(4).enumerate() {
            let v = if x < 40 { 100 } else { 200 };
            p.copy_from_slice(&[v, v, v, 255]);
        }
    }
    let place = Placement {
        index: 0,
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };
    blit(&mut dst, &src, place);
    assert_eq!(px(&dst, 0, 0), [100, 100, 100, 255]);
    assert_eq!(px(&dst, 39, 19), [200, 200, 200, 255]);
    // Off-canvas and oversized placements never panic.
    for place in [
        Placement {
            index: 0,
            x: 39,
            y: 19,
            width: 1000,
            height: 1000,
        },
        Placement {
            index: 0,
            x: 1000,
            y: 0,
            width: 10,
            height: 10,
        },
    ] {
        blit(&mut dst, &src, place);
    }
    let bad = BgraFrame {
        width: 10,
        height: 10,
        stride: 40,
        data: vec![0; 3],
        timestamp_us: 0,
    };
    blit(&mut dst, &bad, place);
}

#[test]
fn open_fails_cleanly() {
    let mut open = |i: u32, _: CaptureOptions| -> Result<Box<dyn ScreenCapturer>, CaptureError> {
        Err(CaptureError::NoSuchMonitor(i))
    };
    assert!(matches!(
        AllMonitorsCapturer::open(&[], None, &mut open),
        Err(CaptureError::NoSuchMonitor(ALL_MONITORS))
    ));
    assert!(matches!(
        AllMonitorsCapturer::open(&[mon(0, 0, 0, 10, 10)], None, &mut open),
        Err(CaptureError::NoSuchMonitor(0))
    ));
}
