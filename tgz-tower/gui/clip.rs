//! Rectangles and CPU-side clipping.
//!
//! The renderer has no scissor support, so every quad Tower emits is clipped
//! here first. Quads are axis-aligned, so clipping is exact: shrink the
//! destination and shrink the texture window by the same proportions.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        !(self.w > 0.0 && self.h > 0.0)
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if r > x && b > y {
            Some(Rect::new(x, y, r - x, b - y))
        } else {
            None
        }
    }

    pub fn inset(&self, d: f32) -> Rect {
        self.inset_xy(d, d)
    }

    pub fn inset_xy(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Split off the leftmost `w` pixels: `(left, rest)`.
    pub fn take_left(&self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w).max(0.0);
        (
            Rect::new(self.x, self.y, w, self.h),
            Rect::new(self.x + w, self.y, self.w - w, self.h),
        )
    }

    /// Split off the top `h` pixels: `(top, rest)`.
    pub fn take_top(&self, h: f32) -> (Rect, Rect) {
        let h = h.min(self.h).max(0.0);
        (
            Rect::new(self.x, self.y, self.w, h),
            Rect::new(self.x, self.y + h, self.w, self.h - h),
        )
    }

    /// Split off the rightmost `w` pixels: `(rest, right)`.
    pub fn take_right(&self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w).max(0.0);
        (
            Rect::new(self.x, self.y, self.w - w, self.h),
            Rect::new(self.right() - w, self.y, w, self.h),
        )
    }
}

/// Texture window `(x1, x2, y1, y2)` in atlas coordinates.
pub type Tex = (f32, f32, f32, f32);

/// Clip a textured quad to `clip`. Returns the visible destination and the
/// matching texture window, or `None` if nothing is visible.
pub fn clip_textured(dest: Rect, tex: Tex, clip: &Rect) -> Option<(Rect, Tex)> {
    if dest.is_empty() {
        return None;
    }
    let vis = dest.intersect(clip)?;
    if vis == dest {
        return Some((dest, tex));
    }
    let (x1, x2, y1, y2) = tex;
    let fx0 = (vis.x - dest.x) / dest.w;
    let fx1 = (vis.right() - dest.x) / dest.w;
    let fy0 = (vis.y - dest.y) / dest.h;
    let fy1 = (vis.bottom() - dest.y) / dest.h;
    let tw = x2 - x1;
    let th = y2 - y1;
    Some((
        vis,
        (x1 + tw * fx0, x1 + tw * fx1, y1 + th * fy0, y1 + th * fy1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_and_contains() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersect(&b), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
        assert_eq!(a.intersect(&Rect::new(10.0, 0.0, 5.0, 5.0)), None);
        assert!(a.contains(0.0, 9.99));
        assert!(!a.contains(10.0, 5.0));
    }

    #[test]
    fn unclipped_quad_is_unchanged() {
        let d = Rect::new(2.0, 2.0, 4.0, 4.0);
        let t = (0.1, 0.2, 0.3, 0.4);
        assert_eq!(
            clip_textured(d, t, &Rect::new(0.0, 0.0, 10.0, 10.0)),
            Some((d, t))
        );
    }

    #[test]
    fn clipping_shrinks_texture_proportionally() {
        let d = Rect::new(0.0, 0.0, 10.0, 20.0);
        let (v, (x1, x2, y1, y2)) =
            clip_textured(d, (0.0, 1.0, 0.0, 1.0), &Rect::new(5.0, 15.0, 100.0, 100.0)).unwrap();
        assert_eq!(v, Rect::new(5.0, 15.0, 5.0, 5.0));
        assert!((x1 - 0.5).abs() < 1e-6 && (x2 - 1.0).abs() < 1e-6);
        assert!((y1 - 0.75).abs() < 1e-6 && (y2 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn fully_hidden_quad_is_dropped() {
        let d = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(clip_textured(d, (0.0, 1.0, 0.0, 1.0), &Rect::new(20.0, 0.0, 5.0, 5.0)).is_none());
        assert!(clip_textured(Rect::new(0.0, 0.0, 0.0, 5.0), (0.0, 1.0, 0.0, 1.0), &d).is_none());
    }

    #[test]
    fn splits_partition_the_rect() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        let (l, rest) = r.take_left(30.0);
        assert_eq!(l.w + rest.w, 100.0);
        let (t, rest) = r.take_top(80.0);
        assert_eq!(t.h, 50.0);
        assert_eq!(rest.h, 0.0);
        let (rest, rt) = r.take_right(20.0);
        assert_eq!(rt.x, 80.0);
        assert_eq!(rest.w, 80.0);
    }
}
