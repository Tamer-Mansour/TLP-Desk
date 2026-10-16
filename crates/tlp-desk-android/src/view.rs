//! Zoom and pan of the remote picture inside the phone screen (pure logic).

use tlp_desk_input::ViewRect;

/// Largest zoom factor (1.0 = the whole remote screen fits the viewport).
pub const MAX_ZOOM: f32 = 6.0;

/// A screen position in the same unit as the viewport (egui points).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
}

impl Point {
    /// Creates a point.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// How the remote picture is placed: fitted to the viewport, then zoomed about its centre and
/// shifted by `pan`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewTransform {
    /// 1.0..=[`MAX_ZOOM`].
    pub zoom: f32,
    /// Shift of the picture centre from the viewport centre, in screen units.
    pub pan: Point,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Point::default(),
        }
    }
}

fn sane(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

impl ViewTransform {
    /// Where the picture is drawn: `image` is the decoded frame size.
    pub fn image_rect(&self, viewport: ViewRect, image: (u32, u32)) -> ViewRect {
        let fit = ViewRect::fit(viewport, image.0, image.1);
        let cx = fit.x + fit.width / 2.0 + self.pan.x;
        let cy = fit.y + fit.height / 2.0 + self.pan.y;
        let (w, h) = (fit.width * self.zoom, fit.height * self.zoom);
        ViewRect::new(cx - w / 2.0, cy - h / 2.0, w, h)
    }

    /// Keeps the picture covering the viewport (no empty gaps while zoomed) and re-centres it
    /// on an axis where it is smaller than the viewport.
    pub fn clamp(&mut self, viewport: ViewRect, image: (u32, u32)) {
        self.zoom = sane(self.zoom).clamp(1.0, MAX_ZOOM);
        if self.zoom == 0.0 {
            self.zoom = 1.0;
        }
        let fit = ViewRect::fit(viewport, image.0, image.1);
        let limit = |scaled: f32, view: f32| ((scaled - view) / 2.0).max(0.0);
        let lx = limit(fit.width * self.zoom, viewport.width);
        let ly = limit(fit.height * self.zoom, viewport.height);
        self.pan.x = sane(self.pan.x).clamp(-lx, lx);
        self.pan.y = sane(self.pan.y).clamp(-ly, ly);
    }

    /// Zooms by `factor` keeping the picture point under `focal` fixed (pinch).
    pub fn zoom_about(&mut self, factor: f32, focal: Point, viewport: ViewRect, image: (u32, u32)) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old = self.zoom;
        let new = (old * factor).clamp(1.0, MAX_ZOOM);
        let fit = ViewRect::fit(viewport, image.0, image.1);
        let center = Point::new(
            fit.x + fit.width / 2.0 + self.pan.x,
            fit.y + fit.height / 2.0 + self.pan.y,
        );
        let k = new / old;
        let new_center = Point::new(
            focal.x - (focal.x - center.x) * k,
            focal.y - (focal.y - center.y) * k,
        );
        self.zoom = new;
        self.pan = Point::new(
            new_center.x - (fit.x + fit.width / 2.0),
            new_center.y - (fit.y + fit.height / 2.0),
        );
        self.clamp(viewport, image);
    }

    /// Moves the picture by a finger delta.
    pub fn pan_by(&mut self, dx: f32, dy: f32, viewport: ViewRect, image: (u32, u32)) {
        self.pan.x += dx;
        self.pan.y += dy;
        self.clamp(viewport, image);
    }

    /// Normalized wire position of a screen point, clamped to the picture edge.
    pub fn to_normalized(&self, p: Point, viewport: ViewRect, image: (u32, u32)) -> (u16, u16) {
        self.image_rect(viewport, image)
            .point_to_normalized(p.x, p.y, image.0, image.1)
    }

    /// True when the screen point is over the picture.
    pub fn hits_image(&self, p: Point, viewport: ViewRect, image: (u32, u32)) -> bool {
        self.image_rect(viewport, image).contains(p.x, p.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: ViewRect = ViewRect::new(0.0, 0.0, 400.0, 800.0);
    const IMG: (u32, u32) = (1920, 1080);

    #[test]
    fn unzoomed_picture_is_letterboxed_in_the_middle() {
        let r = ViewTransform::default().image_rect(VIEW, IMG);
        assert_eq!(r.width, 400.0);
        assert!((r.height - 225.0).abs() < 0.01);
        assert!((r.y - 287.5).abs() < 0.01);
    }

    #[test]
    fn zoom_is_clamped_and_keeps_the_focal_point() {
        let mut t = ViewTransform::default();
        let focal = Point::new(300.0, 400.0);
        let before = t.to_normalized(focal, VIEW, IMG);
        t.zoom_about(2.0, focal, VIEW, IMG);
        assert!((t.zoom - 2.0).abs() < 1e-6);
        let after = t.to_normalized(focal, VIEW, IMG);
        let d = |a: u16, b: u16| (i32::from(a) - i32::from(b)).abs();
        assert!(d(before.0, after.0) < 400 && d(before.1, after.1) < 400);
        t.zoom_about(100.0, focal, VIEW, IMG);
        assert_eq!(t.zoom, MAX_ZOOM);
        t.zoom_about(0.0001, focal, VIEW, IMG);
        assert_eq!(t.zoom, 1.0);
        assert_eq!(t.pan, Point::default());
    }

    #[test]
    fn pan_cannot_leave_the_picture() {
        let mut t = ViewTransform::default();
        t.zoom_about(3.0, Point::new(200.0, 400.0), VIEW, IMG);
        t.pan_by(10_000.0, -10_000.0, VIEW, IMG);
        let r = t.image_rect(VIEW, IMG);
        assert!(r.x <= 0.0 + 0.01 && r.x + r.width >= VIEW.width - 0.01);
        assert!(r.y <= 0.0 + 0.01 || r.height < VIEW.height);
    }

    #[test]
    fn hostile_values_do_not_poison_the_transform() {
        let mut t = ViewTransform {
            zoom: f32::NAN,
            pan: Point::new(f32::INFINITY, f32::NAN),
        };
        t.clamp(VIEW, IMG);
        assert!(t.zoom.is_finite() && t.pan.x.is_finite() && t.pan.y.is_finite());
        t.zoom_about(f32::NAN, Point::default(), VIEW, IMG);
        assert!(t.zoom >= 1.0);
        let empty = ViewTransform::default().to_normalized(Point::new(1.0, 1.0), VIEW, (0, 0));
        assert_eq!(empty, (0, 0));
    }
}
