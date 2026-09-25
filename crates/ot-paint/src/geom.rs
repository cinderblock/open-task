//! Geometry in device-independent pixels.

/// A point in DIPs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A size in DIPs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    #[must_use]
    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

/// An axis-aligned rectangle in DIPs, stored as origin plus size.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0);

    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[must_use]
    pub const fn from_size(size: Size) -> Self {
        Self::new(0.0, 0.0, size.w, size.h)
    }

    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    #[must_use]
    pub fn origin(&self) -> Point {
        Point::new(self.x, self.y)
    }

    #[must_use]
    pub fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }

    #[must_use]
    pub fn center(&self) -> Point {
        Point::new(self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// Shrink by `dx` on the left and right and `dy` on the top and bottom.
    /// Negative values grow. Never produces a negative size.
    #[must_use]
    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self::new(
            self.x + dx,
            self.y + dy,
            (self.w - 2.0 * dx).max(0.0),
            (self.h - 2.0 * dy).max(0.0),
        )
    }

    /// Intersection, or an empty rect at the origin of `self` if disjoint.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        if r <= x || b <= y {
            Self::new(self.x, self.y, 0.0, 0.0)
        } else {
            Self::new(x, y, r - x, b - y)
        }
    }

    /// Split off a strip of height `h` from the top. Returns `(top, remainder)`.
    #[must_use]
    pub fn split_top(&self, h: f32) -> (Self, Self) {
        let h = h.clamp(0.0, self.h);
        (
            Self::new(self.x, self.y, self.w, h),
            Self::new(self.x, self.y + h, self.w, self.h - h),
        )
    }

    /// Split off a strip of height `h` from the bottom. Returns `(bottom, remainder)`.
    #[must_use]
    pub fn split_bottom(&self, h: f32) -> (Self, Self) {
        let h = h.clamp(0.0, self.h);
        (
            Self::new(self.x, self.bottom() - h, self.w, h),
            Self::new(self.x, self.y, self.w, self.h - h),
        )
    }

    /// Split off a strip of width `w` from the left. Returns `(left, remainder)`.
    #[must_use]
    pub fn split_left(&self, w: f32) -> (Self, Self) {
        let w = w.clamp(0.0, self.w);
        (
            Self::new(self.x, self.y, w, self.h),
            Self::new(self.x + w, self.y, self.w - w, self.h),
        )
    }

    /// Translate by `(dx, dy)`.
    #[must_use]
    pub fn offset(&self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Snap edges to whole DIPs so 1-DIP strokes land on pixel centers at 100% DPI.
    #[must_use]
    pub fn rounded(&self) -> Self {
        let x = self.x.round();
        let y = self.y.round();
        Self::new(
            x,
            y,
            (self.right().round() - x).max(0.0),
            (self.bottom().round() - y).max(0.0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_preserve_area() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        let (top, rest) = r.split_top(20.0);
        assert_eq!(top, Rect::new(10.0, 20.0, 100.0, 20.0));
        assert_eq!(rest, Rect::new(10.0, 40.0, 100.0, 30.0));
        let (left, rest) = r.split_left(30.0);
        assert_eq!(left, Rect::new(10.0, 20.0, 30.0, 50.0));
        assert_eq!(rest, Rect::new(40.0, 20.0, 70.0, 50.0));
    }

    #[test]
    fn split_clamps_to_available_extent() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let (top, rest) = r.split_top(50.0);
        assert_eq!(top, Rect::new(0.0, 0.0, 10.0, 10.0));
        assert_eq!(rest, Rect::new(0.0, 10.0, 10.0, 0.0));
        assert!(rest.is_empty());
    }

    #[test]
    fn intersect_disjoint_is_empty() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 20.0, 10.0, 10.0);
        assert!(a.intersect(&b).is_empty());
        let c = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersect(&c), Rect::new(5.0, 5.0, 5.0, 5.0));
    }

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(9.99, 9.99)));
        assert!(!r.contains(Point::new(10.0, 5.0)));
    }
}
