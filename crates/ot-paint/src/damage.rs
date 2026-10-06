//! What changed between two frames, so a backend that keeps the last frame it drew
//! can redraw only that.
//!
//! The view rebuilds its whole display list every frame, which is cheap; drawing
//! it is not, above all its text. When only a chart moved, the rest of the list is
//! the same as last frame's, command for command. [`DisplayList::damage_since`]
//! walks the two lists side by side and collects the area of every command that
//! differs, old and new, so a backend can clip to that area and replay just the
//! commands that touch it, on top of the frame it kept.
//!
//! Anything that changes the structure of the frame (a different number of
//! commands, a clip or clear that differs) makes the answer "all of it": the
//! lists can no longer be compared pairwise.

use crate::display::{DisplayList, DrawCmd};
use crate::geom::{Point, Rect};

/// Room around a command's geometry for antialiased edges, in DIPs.
const FRINGE: f32 = 1.0;
/// Damaged rectangles nearer than this are merged: one clip and one pass over the
/// commands is cheaper than two that nearly touch.
const MERGE_GAP: f32 = 8.0;
/// More separate rectangles than this and they are merged into one.
const MAX_RECTS: usize = 16;

impl DisplayList {
    /// The areas, in DIPs, where this frame differs from `prev`, merged into a few
    /// rectangles in `out`; empty when the frames are the same. Returns `false` when
    /// the frames differ in structure, so the whole frame has to be drawn.
    pub fn damage_since(&self, prev: &DisplayList, out: &mut Vec<Rect>) -> bool {
        out.clear();
        if self.cmds().len() != prev.cmds().len() {
            return false;
        }
        // The clips are the same in both lists (a differing one is structural), so
        // one stack serves both.
        let mut clips: Vec<Rect> = Vec::new();
        for (old, new) in prev.cmds().iter().zip(self.cmds()) {
            let same = same_cmd(prev, old, self, new);
            match (old, new) {
                (DrawCmd::PushClip(_), DrawCmd::PushClip(r)) if same => {
                    let r = clips.last().map_or(*r, |c| c.intersect(r));
                    clips.push(r);
                    continue;
                }
                (DrawCmd::PopClip, DrawCmd::PopClip) => {
                    clips.pop();
                    continue;
                }
                _ if same => continue,
                (DrawCmd::Clear(_) | DrawCmd::PushClip(_) | DrawCmd::PopClip, _)
                | (_, DrawCmd::Clear(_) | DrawCmd::PushClip(_) | DrawCmd::PopClip) => {
                    return false;
                }
                _ => {}
            }
            for r in [prev.bounds(old), self.bounds(new)].into_iter().flatten() {
                let r = clips.last().map_or(r, |c| c.intersect(&r));
                if !r.is_empty() {
                    out.push(r);
                }
            }
        }
        merge(out);
        true
    }

    /// The area `cmd` (one of this list's) can paint, in DIPs, fringe included;
    /// `None` for the whole surface. Clips are not applied.
    #[must_use]
    pub fn bounds(&self, cmd: &DrawCmd) -> Option<Rect> {
        let r = match *cmd {
            DrawCmd::Clear(_) | DrawCmd::PushClip(_) | DrawCmd::PopClip => return None,
            DrawCmd::FillRect { rect, .. }
            | DrawCmd::FillRoundRect { rect, .. }
            | DrawCmd::Image { rect, .. } => rect.inset(-FRINGE, -FRINGE),
            DrawCmd::StrokeRect { rect, width, .. }
            | DrawCmd::StrokeRoundRect { rect, width, .. } => {
                let m = width * 0.5 + FRINGE;
                rect.inset(-m, -m)
            }
            DrawCmd::Line {
                from, to, width, ..
            } => spread(&[from, to], width * 0.5 + FRINGE),
            DrawCmd::Polyline { points, width, .. } => {
                spread(self.points(points), width * 0.5 + FRINGE)
            }
            DrawCmd::FillPolygon { points, .. } => spread(self.points(points), FRINGE),
            // Drawn clipped to its rectangle; the caret of a field stays inside it.
            DrawCmd::Text(t) => t.rect.inset(-FRINGE, -FRINGE),
            DrawCmd::Icon { rect, size, .. } => {
                let c = rect.center();
                let glyph = Rect::new(c.x - size * 0.5, c.y - size * 0.5, size, size);
                union(rect, glyph).inset(-FRINGE, -FRINGE)
            }
        };
        Some(r)
    }
}

/// Whether two commands, each resolved against its own list, draw the same thing.
fn same_cmd(a: &DisplayList, ca: &DrawCmd, b: &DisplayList, cb: &DrawCmd) -> bool {
    match (*ca, *cb) {
        (
            DrawCmd::Polyline {
                points: pa,
                color: ka,
                width: wa,
            },
            DrawCmd::Polyline {
                points: pb,
                color: kb,
                width: wb,
            },
        ) => ka == kb && wa.to_bits() == wb.to_bits() && a.points(pa) == b.points(pb),
        (
            DrawCmd::FillPolygon {
                points: pa,
                color: ka,
            },
            DrawCmd::FillPolygon {
                points: pb,
                color: kb,
            },
        ) => ka == kb && a.points(pa) == b.points(pb),
        (DrawCmd::Text(ta), DrawCmd::Text(tb)) => {
            // Everything but where the string sits in its arena.
            let tb_at_a = crate::display::TextCmd {
                text: ta.text,
                ..tb
            };
            ta == tb_at_a && a.str(ta.text) == b.str(tb.text)
        }
        (DrawCmd::Image { path: pa, rect: ra }, DrawCmd::Image { path: pb, rect: rb }) => {
            ra == rb && a.str(pa) == b.str(pb)
        }
        (x, y) => x == y,
    }
}

/// The rectangle around `points`, grown by `margin`.
fn spread(points: &[Point], margin: f32) -> Rect {
    let mut lo = Point::new(f32::INFINITY, f32::INFINITY);
    let mut hi = Point::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in points {
        lo = Point::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Point::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    if lo.x > hi.x {
        return Rect::ZERO;
    }
    Rect::new(
        lo.x - margin,
        lo.y - margin,
        hi.x - lo.x + 2.0 * margin,
        hi.y - lo.y + 2.0 * margin,
    )
}

/// The smallest rectangle holding both.
fn union(a: Rect, b: Rect) -> Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    Rect::new(
        x,
        y,
        a.right().max(b.right()) - x,
        a.bottom().max(b.bottom()) - y,
    )
}

/// Whether `a` and `b` overlap or come within [`MERGE_GAP`] of each other.
fn near(a: &Rect, b: &Rect) -> bool {
    a.x <= b.right() + MERGE_GAP
        && b.x <= a.right() + MERGE_GAP
        && a.y <= b.bottom() + MERGE_GAP
        && b.y <= a.bottom() + MERGE_GAP
}

/// Merge rectangles that are near each other until none are, then everything
/// into one if there are still too many.
fn merge(rects: &mut Vec<Rect>) {
    let mut i = 0;
    while i < rects.len() {
        let mut grew = false;
        let mut j = i + 1;
        while j < rects.len() {
            if near(&rects[i], &rects[j]) {
                rects[i] = union(rects[i], rects.swap_remove(j));
                grew = true;
            } else {
                j += 1;
            }
        }
        // A grown rectangle may now reach ones it was checked against already.
        if !grew {
            i += 1;
        }
    }
    if rects.len() > MAX_RECTS {
        let all = rects.iter().copied().reduce(union).unwrap_or(Rect::ZERO);
        rects.clear();
        rects.push(all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::text::TextStyle;

    /// A frame of a table and a chart, the chart's line rising to `top`.
    fn frame(dl: &mut DisplayList, top: f32, label: &str) {
        dl.clear();
        dl.clear_to(Color::TRANSPARENT);
        for row in 0..20 {
            let y = 200.0 + row as f32 * 20.0;
            dl.label(
                &format!("row {row}"),
                Rect::new(0.0, y, 300.0, 20.0),
                TextStyle::default(),
                Color::WHITE,
            );
        }
        dl.label(
            label,
            Rect::new(0.0, 0.0, 100.0, 20.0),
            TextStyle::default(),
            Color::WHITE,
        );
        dl.push_clip(Rect::new(100.0, 20.0, 200.0, 100.0));
        dl.polyline(
            [
                Point::new(90.0, 110.0),
                Point::new(200.0, top),
                Point::new(320.0, 110.0),
            ],
            Color::WHITE,
            1.5,
        );
        dl.pop_clip();
    }

    #[test]
    fn the_same_frame_has_no_damage() {
        let (mut a, mut b) = (DisplayList::new(), DisplayList::new());
        frame(&mut a, 50.0, "CPU 4%");
        frame(&mut b, 50.0, "CPU 4%");
        let mut out = vec![Rect::ZERO];
        assert!(b.damage_since(&a, &mut out));
        assert_eq!(out, []);
    }

    #[test]
    fn a_moved_line_damages_its_clip_and_nothing_else() {
        let (mut a, mut b) = (DisplayList::new(), DisplayList::new());
        frame(&mut a, 50.0, "CPU 4%");
        frame(&mut b, 60.0, "CPU 4%");
        let mut out = Vec::new();
        assert!(b.damage_since(&a, &mut out));
        // The old line and the new, stroke and fringe included, cut to the clip:
        // the line reaches past it either side, but nothing outside it can change.
        let m = 0.75 + FRINGE;
        assert_eq!(
            out,
            [Rect::new(100.0, 50.0 - m, 200.0, 110.0 - 50.0 + 2.0 * m)]
        );
    }

    #[test]
    fn changed_text_damages_its_box_and_far_damage_stays_apart() {
        let (mut a, mut b) = (DisplayList::new(), DisplayList::new());
        frame(&mut a, 50.0, "CPU 4%");
        frame(&mut b, 60.0, "CPU 5%");
        let mut out = Vec::new();
        assert!(b.damage_since(&a, &mut out));
        // The label, fringe included, and the line, too far below it to merge.
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.contains(&Rect::new(-1.0, -1.0, 102.0, 22.0)), "{out:?}");
        assert!(
            out.iter().all(|r| r.bottom() < 200.0),
            "the table is untouched"
        );
    }

    #[test]
    fn a_different_structure_is_drawn_whole() {
        let (mut a, mut b) = (DisplayList::new(), DisplayList::new());
        frame(&mut a, 50.0, "CPU 4%");
        frame(&mut b, 50.0, "CPU 4%");
        b.fill_rect(Rect::new(0.0, 0.0, 5.0, 5.0), Color::WHITE);
        let mut out = Vec::new();
        assert!(!b.damage_since(&a, &mut out), "one more command");
        let clipped = |at: f32| {
            let mut dl = DisplayList::new();
            dl.clear_to(Color::TRANSPARENT);
            dl.push_clip(Rect::new(at, 20.0, 200.0, 100.0));
            dl.fill_rect(Rect::new(0.0, 0.0, 500.0, 500.0), Color::WHITE);
            dl.pop_clip();
            dl
        };
        assert!(
            !clipped(101.0).damage_since(&clipped(100.0), &mut out),
            "the clip moved"
        );
        assert!(clipped(100.0).damage_since(&clipped(100.0), &mut out));
    }

    #[test]
    fn far_apart_damage_stays_apart_and_too_much_becomes_one() {
        let mut rects = vec![
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Rect::new(500.0, 500.0, 10.0, 10.0),
            Rect::new(15.0, 0.0, 10.0, 10.0),
        ];
        merge(&mut rects);
        assert_eq!(rects.len(), 2, "{rects:?}");
        assert!(rects.contains(&Rect::new(0.0, 0.0, 25.0, 10.0)));
        let mut many: Vec<Rect> = (0..40)
            .map(|i| Rect::new(i as f32 * 100.0, 0.0, 10.0, 10.0))
            .collect();
        merge(&mut many);
        assert_eq!(many, [Rect::new(0.0, 0.0, 3910.0, 10.0)]);
    }
}
