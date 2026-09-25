//! The display list: one frame's worth of draw commands.

use crate::color::Color;
use crate::geom::{Point, Rect};
use crate::text::{HAlign, TextStyle, VAlign};

/// A range into one of the list's arenas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

impl Span {
    fn range(self) -> std::ops::Range<usize> {
        let s = self.start as usize;
        s..s + self.len as usize
    }
}

/// A run of text laid out inside a rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextCmd {
    /// Indexes [`DisplayList::str`].
    pub text: Span,
    pub rect: Rect,
    pub style: TextStyle,
    pub color: Color,
    pub halign: HAlign,
    pub valign: VAlign,
    /// Trim with an ellipsis when the text is wider than `rect`. Text is always
    /// clipped to `rect` regardless.
    pub ellipsis: bool,
}

/// One drawing primitive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawCmd {
    /// Fill the whole surface. Backends composited over a system backdrop use a
    /// transparent color here to let the backdrop show through.
    Clear(Color),
    FillRect {
        rect: Rect,
        color: Color,
    },
    FillRoundRect {
        rect: Rect,
        radius: f32,
        color: Color,
    },
    StrokeRect {
        rect: Rect,
        color: Color,
        width: f32,
    },
    Line {
        from: Point,
        to: Point,
        color: Color,
        width: f32,
    },
    /// An open path through `points` (indexes [`DisplayList::points`]).
    Polyline {
        points: Span,
        color: Color,
        width: f32,
    },
    /// A closed filled path through `points`.
    FillPolygon {
        points: Span,
        color: Color,
    },
    Text(TextCmd),
    /// Everything until the matching [`DrawCmd::PopClip`] is clipped to `rect`.
    PushClip(Rect),
    PopClip,
}

/// A frame's draw commands plus the arenas they index.
///
/// Cleared and refilled each frame; capacity is retained.
#[derive(Debug, Default, Clone)]
pub struct DisplayList {
    cmds: Vec<DrawCmd>,
    points: Vec<Point>,
    strings: String,
    clip_depth: u32,
}

impl DisplayList {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget this frame's commands, keeping allocations.
    pub fn clear(&mut self) {
        self.cmds.clear();
        self.points.clear();
        self.strings.clear();
        self.clip_depth = 0;
    }

    #[must_use]
    pub fn cmds(&self) -> &[DrawCmd] {
        &self.cmds
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    /// Resolve a point span.
    #[must_use]
    pub fn points(&self, span: Span) -> &[Point] {
        &self.points[span.range()]
    }

    /// Resolve a text span.
    #[must_use]
    pub fn str(&self, span: Span) -> &str {
        &self.strings[span.range()]
    }

    /// Unbalanced clip pushes are a bug in the view code; this makes it visible in
    /// tests without making the render path panic.
    #[must_use]
    pub fn clip_depth(&self) -> u32 {
        self.clip_depth
    }

    pub fn clear_to(&mut self, color: Color) {
        self.cmds.push(DrawCmd::Clear(color));
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if !rect.is_empty() && color.a > 0.0 {
            self.cmds.push(DrawCmd::FillRect { rect, color });
        }
    }

    pub fn fill_round_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        if !rect.is_empty() && color.a > 0.0 {
            self.cmds.push(DrawCmd::FillRoundRect {
                rect,
                radius,
                color,
            });
        }
    }

    pub fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32) {
        if !rect.is_empty() && color.a > 0.0 {
            self.cmds.push(DrawCmd::StrokeRect { rect, color, width });
        }
    }

    pub fn line(&mut self, from: Point, to: Point, color: Color, width: f32) {
        if color.a > 0.0 {
            self.cmds.push(DrawCmd::Line {
                from,
                to,
                color,
                width,
            });
        }
    }

    /// Add an open polyline. Fewer than two points draws nothing.
    pub fn polyline<I: IntoIterator<Item = Point>>(&mut self, pts: I, color: Color, width: f32) {
        let span = self.push_points(pts);
        if span.len >= 2 && color.a > 0.0 {
            self.cmds.push(DrawCmd::Polyline {
                points: span,
                color,
                width,
            });
        }
    }

    /// Add a closed filled polygon. Fewer than three points draws nothing.
    pub fn fill_polygon<I: IntoIterator<Item = Point>>(&mut self, pts: I, color: Color) {
        let span = self.push_points(pts);
        if span.len >= 3 && color.a > 0.0 {
            self.cmds.push(DrawCmd::FillPolygon {
                points: span,
                color,
            });
        }
    }

    /// Lay out `text` in `rect`.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        text: &str,
        rect: Rect,
        style: TextStyle,
        color: Color,
        halign: HAlign,
        valign: VAlign,
        ellipsis: bool,
    ) {
        if text.is_empty() || rect.is_empty() || color.a <= 0.0 {
            return;
        }
        let start = self.strings.len() as u32;
        self.strings.push_str(text);
        self.cmds.push(DrawCmd::Text(TextCmd {
            text: Span {
                start,
                len: text.len() as u32,
            },
            rect,
            style,
            color,
            halign,
            valign,
            ellipsis,
        }));
    }

    /// Convenience: left-aligned, vertically centered, ellipsized. The common
    /// table-cell case.
    pub fn label(&mut self, text: &str, rect: Rect, style: TextStyle, color: Color) {
        self.text(text, rect, style, color, HAlign::Left, VAlign::Middle, true);
    }

    pub fn push_clip(&mut self, rect: Rect) {
        self.clip_depth += 1;
        self.cmds.push(DrawCmd::PushClip(rect));
    }

    pub fn pop_clip(&mut self) {
        self.clip_depth = self.clip_depth.saturating_sub(1);
        self.cmds.push(DrawCmd::PopClip);
    }

    fn push_points<I: IntoIterator<Item = Point>>(&mut self, pts: I) -> Span {
        let start = self.points.len() as u32;
        self.points.extend(pts);
        Span {
            start,
            len: self.points.len() as u32 - start,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arenas_round_trip() {
        let mut dl = DisplayList::new();
        dl.label(
            "hello",
            Rect::new(0.0, 0.0, 10.0, 10.0),
            TextStyle::default(),
            Color::WHITE,
        );
        dl.polyline(
            [
                Point::new(0.0, 0.0),
                Point::new(1.0, 1.0),
                Point::new(2.0, 0.0),
            ],
            Color::WHITE,
            1.0,
        );
        let mut texts = 0;
        let mut lines = 0;
        for c in dl.cmds() {
            match *c {
                DrawCmd::Text(t) => {
                    assert_eq!(dl.str(t.text), "hello");
                    texts += 1;
                }
                DrawCmd::Polyline { points, .. } => {
                    assert_eq!(dl.points(points).len(), 3);
                    lines += 1;
                }
                _ => {}
            }
        }
        assert_eq!((texts, lines), (1, 1));
    }

    #[test]
    fn degenerate_primitives_are_dropped() {
        let mut dl = DisplayList::new();
        dl.fill_rect(Rect::new(0.0, 0.0, 0.0, 10.0), Color::WHITE);
        dl.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::TRANSPARENT);
        dl.polyline([Point::default()], Color::WHITE, 1.0);
        dl.fill_polygon([Point::default(), Point::default()], Color::WHITE);
        dl.text(
            "",
            Rect::new(0.0, 0.0, 10.0, 10.0),
            TextStyle::default(),
            Color::WHITE,
            HAlign::Left,
            VAlign::Top,
            false,
        );
        assert!(dl.is_empty());
    }

    #[test]
    fn clear_retains_nothing_but_capacity() {
        let mut dl = DisplayList::new();
        dl.push_clip(Rect::new(0.0, 0.0, 1.0, 1.0));
        dl.label(
            "x",
            Rect::new(0.0, 0.0, 10.0, 10.0),
            TextStyle::default(),
            Color::WHITE,
        );
        dl.pop_clip();
        assert_eq!(dl.clip_depth(), 0);
        dl.clear();
        assert!(dl.is_empty());
        assert_eq!(dl.clip_depth(), 0);
    }
}
