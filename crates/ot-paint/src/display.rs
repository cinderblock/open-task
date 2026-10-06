//! The display list: one frame's worth of draw commands.

use crate::color::Color;
use crate::geom::{Point, Rect};
use crate::icon::Icon;
use crate::text::{HAlign, TextStyle, VAlign};

/// A range into one of the list's arenas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    /// Editable-field semantics: never trimmed; when wider than `rect`, shifted
    /// left so the end of the text stays visible. Backends measure the text, which
    /// is why this is a flag on the command rather than a computation here.
    pub field: bool,
    /// Draw an insertion caret after the last character, in the text color.
    pub caret: bool,
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
    /// A rounded outline, inside `rect` like [`DrawCmd::StrokeRect`].
    StrokeRoundRect {
        rect: Rect,
        radius: f32,
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
    /// A symbolic icon, `size` DIPs tall, centered in `rect`.
    Icon {
        icon: Icon,
        rect: Rect,
        size: f32,
        color: Color,
    },
    /// A raster image the backend finds by its path (a program's icon, say),
    /// scaled into `rect`. The backend owns the lookup and the loading; a path it
    /// has not loaded yet draws nothing this frame.
    Image {
        /// Indexes [`DisplayList::str`].
        path: Span,
        rect: Rect,
    },
    /// Everything until the matching [`DrawCmd::PopClip`] is clipped to `rect`.
    PushClip(Rect),
    PopClip,
}

/// A run of commands painted as one piece that a frame can repaint on its own,
/// such as a chart; see [`DisplayList::begin_layer`] and [`DisplayList::splice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layer {
    /// Chosen by the painter, to know what to repaint.
    pub id: u32,
    /// The commands, as indexes into [`DisplayList::cmds`].
    pub start: u32,
    pub end: u32,
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
    layers: Vec<Layer>,
    /// The layer being painted: its id and first command.
    open: Option<(u32, u32)>,
}

impl DisplayList {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make this list a copy of `other`, reusing this one's allocations: how a
    /// backend keeps the last frame to compare the next against.
    pub fn copy_from(&mut self, other: &Self) {
        self.cmds.clone_from(&other.cmds);
        self.points.clone_from(&other.points);
        self.strings.clone_from(&other.strings);
        self.clip_depth = other.clip_depth;
        self.layers.clone_from(&other.layers);
        self.open = other.open;
    }

    /// Start a layer: the commands until [`DisplayList::end_layer`] are one piece,
    /// known by `id`, that [`DisplayList::splice`] can replace. Layers do not nest;
    /// starting one while another is open ends that one first.
    pub fn begin_layer(&mut self, id: u32) {
        self.end_layer();
        self.open = Some((id, self.cmds.len() as u32));
    }

    /// End the open layer, if any.
    pub fn end_layer(&mut self) {
        if let Some((id, start)) = self.open.take() {
            self.layers.push(Layer {
                id,
                start,
                end: self.cmds.len() as u32,
            });
        }
    }

    /// The layers painted, in order.
    #[must_use]
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Make this list `kept` with each of its layers painted afresh: everything
    /// between the layers is copied, and `paint` is called with each layer's id to
    /// paint it again, in its place. How a frame where only some layers can have
    /// changed is made without painting the rest again.
    pub fn splice(&mut self, kept: &DisplayList, mut paint: impl FnMut(u32, &mut DisplayList)) {
        self.clear();
        let mut at = 0;
        for layer in &kept.layers {
            self.copy_cmds(kept, at, layer.start as usize);
            self.begin_layer(layer.id);
            paint(layer.id, self);
            self.end_layer();
            at = layer.end as usize;
        }
        self.copy_cmds(kept, at, kept.cmds.len());
    }

    /// Append `from`'s commands `start..end`, with what they index in its arenas.
    fn copy_cmds(&mut self, from: &DisplayList, start: usize, end: usize) {
        for cmd in &from.cmds[start..end] {
            let cmd = match *cmd {
                DrawCmd::Polyline {
                    points,
                    color,
                    width,
                } => DrawCmd::Polyline {
                    points: self.push_points(from.points(points).iter().copied()),
                    color,
                    width,
                },
                DrawCmd::FillPolygon { points, color } => DrawCmd::FillPolygon {
                    points: self.push_points(from.points(points).iter().copied()),
                    color,
                },
                DrawCmd::Text(t) => DrawCmd::Text(TextCmd {
                    text: self.push_str(from.str(t.text)),
                    ..t
                }),
                DrawCmd::Image { path, rect } => DrawCmd::Image {
                    path: self.push_str(from.str(path)),
                    rect,
                },
                DrawCmd::PushClip(_) => {
                    self.clip_depth += 1;
                    *cmd
                }
                DrawCmd::PopClip => {
                    self.clip_depth = self.clip_depth.saturating_sub(1);
                    *cmd
                }
                other => other,
            };
            self.cmds.push(cmd);
        }
    }

    fn push_str(&mut self, s: &str) -> Span {
        let start = self.strings.len() as u32;
        self.strings.push_str(s);
        Span {
            start,
            len: s.len() as u32,
        }
    }

    /// Forget this frame's commands, keeping allocations.
    pub fn clear(&mut self) {
        self.cmds.clear();
        self.points.clear();
        self.strings.clear();
        self.clip_depth = 0;
        self.layers.clear();
        self.open = None;
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

    pub fn stroke_round_rect(&mut self, rect: Rect, radius: f32, color: Color, width: f32) {
        if !rect.is_empty() && color.a > 0.0 {
            self.cmds.push(DrawCmd::StrokeRoundRect {
                rect,
                radius,
                color,
                width,
            });
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
        self.push_text(
            text,
            TextCmd {
                text: Span::default(),
                rect,
                style,
                color,
                halign,
                valign,
                ellipsis,
                field: false,
                caret: false,
            },
        );
    }

    /// Convenience: left-aligned, vertically centered, ellipsized. The common
    /// table-cell case.
    pub fn label(&mut self, text: &str, rect: Rect, style: TextStyle, color: Color) {
        self.text(text, rect, style, color, HAlign::Left, VAlign::Middle, true);
    }

    /// A single-line editable field's text: left-aligned, never trimmed, scrolled
    /// so the end stays visible, with a caret after it when `caret` is set. Empty
    /// text draws nothing, caret included; callers paint that caret themselves.
    pub fn field(&mut self, text: &str, rect: Rect, style: TextStyle, color: Color, caret: bool) {
        self.push_text(
            text,
            TextCmd {
                text: Span::default(),
                rect,
                style,
                color,
                halign: HAlign::Left,
                valign: VAlign::Middle,
                ellipsis: false,
                field: true,
                caret,
            },
        );
    }

    fn push_text(&mut self, text: &str, mut cmd: TextCmd) {
        if text.is_empty() || cmd.rect.is_empty() || cmd.color.a <= 0.0 {
            return;
        }
        cmd.text = self.push_str(text);
        self.cmds.push(DrawCmd::Text(cmd));
    }

    /// Draw `icon` `size` DIPs tall, centered in `rect`.
    /// The image at `path` (a file whose icon the backend extracts), scaled into
    /// `rect`.
    pub fn image(&mut self, path: &str, rect: Rect) {
        if rect.is_empty() || path.is_empty() {
            return;
        }
        let path = self.push_str(path);
        self.cmds.push(DrawCmd::Image { path, rect });
    }

    pub fn icon(&mut self, icon: Icon, rect: Rect, size: f32, color: Color) {
        if !rect.is_empty() && size > 0.0 && color.a > 0.0 {
            self.cmds.push(DrawCmd::Icon {
                icon,
                rect,
                size,
                color,
            });
        }
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
    fn a_splice_repaints_the_layers_and_keeps_the_rest() {
        let style = TextStyle::default();
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut kept = DisplayList::new();
        kept.label("before", r, style, Color::WHITE);
        kept.push_clip(r);
        kept.begin_layer(7);
        kept.polyline(
            [Point::new(0.0, 0.0), Point::new(1.0, 1.0)],
            Color::WHITE,
            1.0,
        );
        kept.end_layer();
        kept.pop_clip();
        kept.label("between", r, style, Color::WHITE);
        kept.begin_layer(9);
        kept.label("old", r, style, Color::WHITE);
        kept.end_layer();
        kept.image("icon.exe", r);
        assert_eq!(kept.layers().len(), 2);

        let mut dl = DisplayList::new();
        dl.label("stale", r, style, Color::WHITE);
        let mut asked = Vec::new();
        dl.splice(&kept, |id, dl| {
            asked.push(id);
            if id == 7 {
                dl.polyline(
                    [
                        Point::new(0.0, 5.0),
                        Point::new(1.0, 6.0),
                        Point::new(2.0, 7.0),
                    ],
                    Color::WHITE,
                    1.0,
                );
            } else {
                dl.label("new", r, style, Color::WHITE);
                dl.label("newer", r, style, Color::WHITE);
            }
        });
        assert_eq!(asked, [7, 9]);
        let texts: Vec<&str> = dl
            .cmds()
            .iter()
            .filter_map(|c| match *c {
                DrawCmd::Text(t) => Some(dl.str(t.text)),
                DrawCmd::Image { path, .. } => Some(dl.str(path)),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["before", "between", "new", "newer", "icon.exe"]);
        let line = dl.cmds().iter().find_map(|c| match *c {
            DrawCmd::Polyline { points, .. } => Some(dl.points(points).to_vec()),
            _ => None,
        });
        assert_eq!(line.map(|p| p.len()), Some(3));
        assert_eq!(dl.clip_depth(), 0);
        // The layers are where the fresh paint went, so the result splices too.
        let l: Vec<(u32, u32)> = dl
            .layers()
            .iter()
            .map(|l| (l.id, l.end - l.start))
            .collect();
        assert_eq!(l, [(7, 1), (9, 2)]);
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
