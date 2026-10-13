//! Pure coordinate mapping between the viewer's displayed image, the normalized wire
//! coordinates and the host's physical virtual desktop.

use crate::denormalize;

/// A rectangle in physical desktop pixels, in virtual-desktop coordinates.
///
/// The primary monitor's top-left is `(0, 0)`; monitors left of or above it have negative
/// origins. Used both for one monitor and for the whole virtual desktop bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl MonitorRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Maps normalized wire coordinates to the physical desktop pixel inside this monitor.
    /// `0` is the first pixel column/row and `u16::MAX` the last one.
    pub fn normalized_to_pixel(&self, x: u16, y: u16) -> (i32, i32) {
        (
            offset(self.x, denormalize(x, self.width)),
            offset(self.y, denormalize(y, self.height)),
        )
    }

    /// True when the physical desktop pixel `(px, py)` lies inside this rectangle.
    pub fn contains(&self, px: i32, py: i32) -> bool {
        let (px, py) = (i64::from(px), i64::from(py));
        let (x, y) = (i64::from(self.x), i64::from(self.y));
        px >= x && py >= y && px < x + i64::from(self.width) && py < y + i64::from(self.height)
    }
}

fn offset(origin: i32, d: u32) -> i32 {
    (i64::from(origin) + i64::from(d)).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Maps a physical desktop pixel to `SendInput` absolute coordinates for
/// `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`, where `0..=65535` spans the whole
/// virtual desktop `virtual_desk`.
///
/// Windows converts an absolute coordinate `a` back to the pixel `origin + a * extent / 65536`
/// (rounded down). Choosing `a = ceil(rel * 65536 / extent)` makes that conversion land on
/// exactly the requested pixel for every desktop up to 65536 pixels wide. Pixels outside the
/// virtual desktop are clamped to its edge.
pub fn pixel_to_virtual_absolute(px: i32, py: i32, virtual_desk: MonitorRect) -> (i32, i32) {
    (
        axis_to_absolute(px, virtual_desk.x, virtual_desk.width),
        axis_to_absolute(py, virtual_desk.y, virtual_desk.height),
    )
}

fn axis_to_absolute(p: i32, origin: i32, extent: u32) -> i32 {
    if extent == 0 {
        return 0;
    }
    let extent = i64::from(extent);
    let rel = (i64::from(p) - i64::from(origin)).clamp(0, extent - 1);
    ((rel * 65536 + extent - 1) / extent).min(65535) as i32
}

/// Smallest normalized value that [`denormalize`] maps back to pixel `index` of `extent`.
/// Indices past the end are clamped to the last pixel. Exact for `extent <= 65536`.
pub fn normalize_pixel(index: u32, extent: u32) -> u16 {
    if extent <= 1 {
        return 0;
    }
    let last = u64::from(extent - 1);
    let i = u64::from(index).min(last);
    let v = (i * u64::from(u16::MAX)).div_ceil(last);
    v.min(u64::from(u16::MAX)) as u16
}

/// Where the remote image is drawn in the viewer window, in the same units as the pointer
/// positions the GUI reports (egui points, physical pixels, ... any consistent unit, so DPI
/// scaling of the viewer does not matter).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ViewRect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The largest rectangle with the image's aspect ratio centered inside `container`
    /// (letterbox / pillarbox "fit" mode).
    pub fn fit(container: ViewRect, image_w: u32, image_h: u32) -> ViewRect {
        if image_w == 0 || image_h == 0 || container.width <= 0.0 || container.height <= 0.0 {
            return ViewRect::new(container.x, container.y, 0.0, 0.0);
        }
        let scale = (f64::from(container.width) / f64::from(image_w))
            .min(f64::from(container.height) / f64::from(image_h));
        let w = f64::from(image_w) * scale;
        let h = f64::from(image_h) * scale;
        ViewRect::new(
            (f64::from(container.x) + (f64::from(container.width) - w) / 2.0) as f32,
            (f64::from(container.y) + (f64::from(container.height) - h) / 2.0) as f32,
            w as f32,
            h as f32,
        )
    }

    /// True when the pointer `(px, py)` is over the image (edges inclusive at the top-left).
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.width && py < self.y + self.height
    }

    /// Maps a pointer position over the displayed image to normalized wire coordinates.
    ///
    /// `image_w`/`image_h` are the decoded frame size. The pointer is resolved to the image
    /// pixel under it and encoded so the host's [`denormalize`] returns exactly that pixel
    /// when the host monitor has the same size. Points outside the rectangle (letterbox bars,
    /// outside the window while dragging) are clamped to the nearest edge.
    pub fn point_to_normalized(&self, px: f32, py: f32, image_w: u32, image_h: u32) -> (u16, u16) {
        (
            view_axis(px, self.x, self.width, image_w),
            view_axis(py, self.y, self.height, image_h),
        )
    }
}

fn view_axis(p: f32, origin: f32, len: f32, extent: u32) -> u16 {
    if extent == 0 || len.is_nan() || len <= 0.0 || p.is_nan() {
        return 0;
    }
    let t = ((f64::from(p) - f64::from(origin)) / f64::from(len)).clamp(0.0, 1.0);
    let index = (t * f64::from(extent)).floor().min(f64::from(extent - 1)) as u32;
    normalize_pixel(index, extent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How Windows turns a `MOUSEEVENTF_VIRTUALDESK` absolute coordinate back into a pixel.
    fn windows_pixel(abs: i32, origin: i32, extent: u32) -> i32 {
        origin + ((i64::from(abs) * i64::from(extent)) >> 16) as i32
    }

    const EXTENTS: &[u32] = &[
        1, 2, 3, 600, 640, 720, 768, 800, 900, 1024, 1050, 1080, 1200, 1280, 1366, 1440, 1600,
        1920, 2160, 2560, 2880, 3000, 3200, 3440, 3840, 4320, 5120, 5760, 6016, 7680, 11520, 15360,
        65535, 65536,
    ];

    #[test]
    fn normalize_pixel_roundtrips_every_pixel() {
        for &extent in EXTENTS {
            for i in 0..extent {
                let v = normalize_pixel(i, extent);
                assert_eq!(denormalize(v, extent), i, "extent {extent} index {i}");
            }
            assert_eq!(normalize_pixel(0, extent), 0);
            assert_eq!(
                normalize_pixel(extent + 10, extent),
                normalize_pixel(extent - 1, extent)
            );
        }
        assert_eq!(normalize_pixel(1919, 1920), u16::MAX);
        assert_eq!(normalize_pixel(5, 0), 0);
    }

    #[test]
    fn absolute_roundtrips_every_pixel_of_many_desktops() {
        for &extent in EXTENTS {
            for origin in [0, -1920, -2560, -3840, 1080, -1440] {
                for rel in 0..extent {
                    let p = origin + rel as i32;
                    let desk = MonitorRect::new(origin, origin, extent, extent);
                    let (ax, ay) = pixel_to_virtual_absolute(p, p, desk);
                    assert!((0..=65535).contains(&ax), "abs {ax} out of range");
                    assert_eq!(
                        windows_pixel(ax, origin, extent),
                        p,
                        "extent {extent} p {p}"
                    );
                    assert_eq!(windows_pixel(ay, origin, extent), p);
                }
            }
        }
    }

    #[test]
    fn absolute_clamps_outside_points_and_corners() {
        let desk = MonitorRect::new(-1920, -300, 1920 + 2560, 1440);
        assert_eq!(pixel_to_virtual_absolute(-1920, -300, desk), (0, 0));
        assert_eq!(pixel_to_virtual_absolute(-5000, -5000, desk), (0, 0));
        let (ax, ay) = pixel_to_virtual_absolute(i32::MAX, i32::MAX, desk);
        assert_eq!(windows_pixel(ax, -1920, 4480), 2559);
        assert_eq!(windows_pixel(ay, -300, 1440), 1139);
        assert_eq!(
            pixel_to_virtual_absolute(0, 0, MonitorRect::new(0, 0, 0, 0)),
            (0, 0)
        );
    }

    /// Full host path: normalized -> monitor pixel -> virtual absolute -> pixel Windows lands on.
    #[test]
    fn host_path_multi_monitor_layouts() {
        struct Layout {
            desk: MonitorRect,
            monitors: Vec<MonitorRect>,
        }
        let layouts = [
            // Single 1080p.
            Layout {
                desk: MonitorRect::new(0, 0, 1920, 1080),
                monitors: vec![MonitorRect::new(0, 0, 1920, 1080)],
            },
            // 4K primary, 1080p secondary to the left and slightly lower.
            Layout {
                desk: MonitorRect::new(-1920, 0, 1920 + 3840, 2160),
                monitors: vec![
                    MonitorRect::new(0, 0, 3840, 2160),
                    MonitorRect::new(-1920, 600, 1920, 1080),
                ],
            },
            // Laptop 2880x1800 (200%) with 1440p above and a portrait 1200x1920 left.
            Layout {
                desk: MonitorRect::new(-1200, -1440, 1200 + 2880, 1440 + 1800),
                monitors: vec![
                    MonitorRect::new(0, 0, 2880, 1800),
                    MonitorRect::new(0, -1440, 2560, 1440),
                    MonitorRect::new(-1200, -1440, 1200, 1920),
                ],
            },
            // Three 1366x768 in a row, primary in the middle.
            Layout {
                desk: MonitorRect::new(-1366, 0, 3 * 1366, 768),
                monitors: vec![
                    MonitorRect::new(-1366, 0, 1366, 768),
                    MonitorRect::new(0, 0, 1366, 768),
                    MonitorRect::new(1366, 0, 1366, 768),
                ],
            },
        ];
        for layout in &layouts {
            for mon in &layout.monitors {
                let samples = [0u16, 1, 100, 12345, 32767, 32768, 50000, 65534, 65535];
                for &nx in &samples {
                    for &ny in &samples {
                        let (px, py) = mon.normalized_to_pixel(nx, ny);
                        assert!(mon.contains(px, py), "{mon:?} ({nx},{ny}) -> ({px},{py})");
                        let (ax, ay) = pixel_to_virtual_absolute(px, py, layout.desk);
                        assert_eq!(windows_pixel(ax, layout.desk.x, layout.desk.width), px);
                        assert_eq!(windows_pixel(ay, layout.desk.y, layout.desk.height), py);
                    }
                }
                let (lx, ty) = mon.normalized_to_pixel(0, 0);
                assert_eq!((lx, ty), (mon.x, mon.y));
                let (rx, by) = mon.normalized_to_pixel(u16::MAX, u16::MAX);
                assert_eq!(
                    (rx, by),
                    (mon.x + mon.width as i32 - 1, mon.y + mon.height as i32 - 1)
                );
            }
        }
    }

    /// Viewer pixel -> wire -> host pixel is exact when the viewer shows the image 1:1,
    /// at any viewer DPI scale (pointer and rect expressed in the same logical units).
    #[test]
    fn viewer_to_host_is_exact_at_native_scale_for_any_dpi() {
        for (w, h) in [
            (1920u32, 1080u32),
            (1366, 768),
            (2560, 1440),
            (3840, 2160),
            (1280, 1024),
        ] {
            for dpi in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
                // A 1:1 image is w/dpi logical units wide.
                let rect = ViewRect::new(37.0 / dpi, 81.0 / dpi, w as f32 / dpi, h as f32 / dpi);
                let mon = MonitorRect::new(-(w as i32), 0, w, h);
                for ix in [0u32, 1, 2, w / 3, w / 2, w - 2, w - 1] {
                    let iy = (ix * h / w).min(h - 1);
                    // Center of the image pixel, in logical units.
                    let px = rect.x + (ix as f32 + 0.5) / dpi;
                    let py = rect.y + (iy as f32 + 0.5) / dpi;
                    let (nx, ny) = rect.point_to_normalized(px, py, w, h);
                    assert_eq!(
                        mon.normalized_to_pixel(nx, ny),
                        (mon.x + ix as i32, iy as i32),
                        "{w}x{h} dpi {dpi} pixel ({ix},{iy})"
                    );
                }
            }
        }
    }

    #[test]
    fn viewer_scaled_and_letterboxed() {
        // 1920x1080 remote in a 1000x1000 window: pillarboxed to 1000x562.5, centered.
        let container = ViewRect::new(0.0, 0.0, 1000.0, 1000.0);
        let r = ViewRect::fit(container, 1920, 1080);
        assert!((r.width - 1000.0).abs() < 1e-3);
        assert!((r.height - 562.5).abs() < 1e-3);
        assert!((r.y - 218.75).abs() < 1e-3);
        assert!(r.x.abs() < 1e-3);

        // Top-left / bottom-right corners of the image.
        assert_eq!(r.point_to_normalized(r.x, r.y, 1920, 1080), (0, 0));
        assert_eq!(
            r.point_to_normalized(r.x + r.width - 0.001, r.y + r.height - 0.001, 1920, 1080),
            (u16::MAX, u16::MAX)
        );
        // Middle of the image maps to the middle pixel.
        let (nx, ny) = r.point_to_normalized(500.0, 500.0, 1920, 1080);
        assert_eq!(denormalize(nx, 1920), 960);
        assert_eq!(denormalize(ny, 1080), 540);

        // Letterbox bars and points outside the window are clamped to the edge.
        assert_eq!(r.point_to_normalized(500.0, 10.0, 1920, 1080).1, 0);
        assert_eq!(r.point_to_normalized(500.0, 990.0, 1920, 1080).1, u16::MAX);
        assert_eq!(r.point_to_normalized(-50.0, -50.0, 1920, 1080), (0, 0));
        assert_eq!(
            r.point_to_normalized(5000.0, 5000.0, 1920, 1080),
            (u16::MAX, u16::MAX)
        );
        assert!(!r.contains(500.0, 10.0));
        assert!(r.contains(500.0, 500.0));

        // Letterboxed (wide window).
        let r = ViewRect::fit(ViewRect::new(10.0, 20.0, 3000.0, 1000.0), 1600, 1200);
        assert!((r.width - 1333.333).abs() < 1e-2);
        assert!((r.x - (10.0 + (3000.0 - 1333.333) / 2.0)).abs() < 1e-2);
        assert_eq!(r.y, 20.0);
    }

    #[test]
    fn viewer_downscaled_and_upscaled_hit_proportional_pixel() {
        for (w, h) in [(3840u32, 2160u32), (1920, 1080), (1024, 768), (800, 600)] {
            for view_w in [320.0f32, 640.0, 1280.0, 1919.0, 2560.0, 5000.0] {
                let view_h = view_w * h as f32 / w as f32;
                let r = ViewRect::new(-100.0, 50.0, view_w, view_h);
                for frac in [0.0f64, 0.1, 0.25, 0.5, 0.75, 0.9, 0.999] {
                    let px = r.x + (f64::from(view_w) * frac) as f32;
                    let py = r.y + (f64::from(view_h) * frac) as f32;
                    let (nx, ny) = r.point_to_normalized(px, py, w, h);
                    let hx = f64::from(denormalize(nx, w));
                    let hy = f64::from(denormalize(ny, h));
                    // Within one source pixel of the proportional position.
                    assert!(
                        (hx - frac * f64::from(w)).abs() <= 1.5,
                        "{w} {view_w} {frac}"
                    );
                    assert!(
                        (hy - frac * f64::from(h)).abs() <= 1.5,
                        "{h} {view_h} {frac}"
                    );
                }
            }
        }
    }

    #[test]
    fn viewer_degenerate_inputs_do_not_panic() {
        let r = ViewRect::new(0.0, 0.0, 0.0, 0.0);
        assert_eq!(r.point_to_normalized(5.0, 5.0, 1920, 1080), (0, 0));
        let r = ViewRect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(r.point_to_normalized(f32::NAN, 50.0, 0, 1080).0, 0);
        assert_eq!(
            r.point_to_normalized(f32::INFINITY, 50.0, 10, 10).0,
            u16::MAX
        );
        let f = ViewRect::fit(ViewRect::new(0.0, 0.0, 100.0, 100.0), 0, 10);
        assert_eq!(f.width, 0.0);
    }
}
