//! What each frame changes, for a plugin that draws only that
//! ([`abi::feature::PARTIAL_PAINT`]), as the Windows shell does.
//!
//! The view rebuilds its whole display list every frame, but at 60 frames a
//! second most of it is the same as the last: only a chart moved. A plugin whose
//! window keeps its pixels (Qt's backing store does) can redraw just the
//! rectangles [`DisplayList::damage_since`] finds, and leave the table's text
//! alone.
//!
//! [`abi::feature::PARTIAL_PAINT`]: ot_shell_abi::feature::PARTIAL_PAINT

use ot_paint::{DisplayList, Rect, Size};

/// Damage over this share of the window is drawn as a whole frame instead. Higher
/// than the Windows shell's limit, which replays the commands once per rectangle:
/// a plugin here makes one pass over the union, and on the History view, where a
/// big chart is most of the window, a lower limit lost the partial frames just
/// where they pay (measured in `plans/linux-cpu-cost.md`).
const WHOLE_AT: f32 = 0.9;

#[derive(Debug, Default)]
pub struct Frames {
    /// The frame painted before the newest.
    prev: DisplayList,
    damage: Vec<Rect>,
    /// The scale and size the newest frame was painted at.
    surface: Option<(f32, Size)>,
}

impl Frames {
    /// Before the next frame is painted into `dl`: keep the one there as the
    /// last.
    pub fn keep(&mut self, dl: &mut DisplayList) {
        std::mem::swap(dl, &mut self.prev);
    }

    /// Where `dl`, just painted at `scale` device pixels a DIP for a window
    /// `size` DIPs across, differs from the frame [`Frames::keep`] kept: a few
    /// rectangles in whole DIPs, none when nothing changed. `None` when it is to
    /// be drawn whole.
    pub fn damage(&mut self, dl: &DisplayList, scale: f32, size: Size) -> Option<&[Rect]> {
        let same_surface = self.surface.replace((scale, size)) == Some((scale, size));
        if !same_surface || !dl.damage_since(&self.prev, &mut self.damage) {
            return None;
        }
        for r in &mut self.damage {
            *r = whole_dips(*r);
        }
        let area: f32 = self.damage.iter().map(|r| r.w * r.h).sum();
        (area < size.w * size.h * WHOLE_AT).then_some(&self.damage[..])
    }
}

/// `r` grown out to whole DIPs, which a plugin's clip and region (Qt's
/// `QRegion` is in integers) take without rounding.
fn whole_dips(r: Rect) -> Rect {
    let (x, y) = (r.x.floor(), r.y.floor());
    Rect::new(x, y, (r.x + r.w).ceil() - x, (r.y + r.h).ceil() - y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_paint::Color;

    const SIZE: Size = Size::new(400.0, 300.0);

    /// A background and two cells, the second `w` DIPs wide.
    fn frame(dl: &mut DisplayList, w: f32) {
        dl.clear();
        dl.fill_rect(Rect::new(0.0, 0.0, 400.0, 300.0), Color::BLACK);
        dl.fill_rect(Rect::new(10.0, 10.0, 50.0, 20.0), Color::WHITE);
        dl.fill_rect(Rect::new(100.5, 40.25, w, 20.0), Color::WHITE);
    }

    #[test]
    fn a_frame_is_drawn_in_part_where_it_changed() {
        let mut frames = Frames::default();
        let mut dl = DisplayList::new();
        frames.keep(&mut dl);
        frame(&mut dl, 30.0);
        assert_eq!(frames.damage(&dl, 1.0, SIZE), None, "the first is whole");

        frames.keep(&mut dl);
        frame(&mut dl, 30.0);
        assert_eq!(
            frames.damage(&dl, 1.0, SIZE),
            Some(&[][..]),
            "nothing changed"
        );

        frames.keep(&mut dl);
        frame(&mut dl, 40.0);
        let damage = frames.damage(&dl, 1.0, SIZE).expect("in part").to_vec();
        assert_eq!(damage.len(), 1);
        let r = damage[0];
        // The cell old and new, its antialiased fringe, out to whole DIPs.
        assert!(r.x <= 99.5 && r.y <= 39.25 && r.x + r.w >= 141.5, "{r:?}");
        for v in [r.x, r.y, r.w, r.h] {
            assert_eq!(v, v.round(), "{r:?}");
        }
    }

    #[test]
    fn a_new_scale_or_size_or_a_big_change_is_drawn_whole() {
        let mut frames = Frames::default();
        let mut dl = DisplayList::new();
        frame(&mut dl, 30.0);
        frames.damage(&dl, 1.0, SIZE);

        frames.keep(&mut dl);
        frame(&mut dl, 30.0);
        assert_eq!(frames.damage(&dl, 2.0, SIZE), None, "new scale");
        frames.keep(&mut dl);
        frame(&mut dl, 30.0);
        assert_eq!(
            frames.damage(&dl, 2.0, Size::new(500.0, 300.0)),
            None,
            "new size"
        );

        // Most of the window: the cell grows to cover it.
        frames.keep(&mut dl);
        frame(&mut dl, 290.0);
        assert!(frames.damage(&dl, 2.0, Size::new(500.0, 300.0)).is_some());
        frames.keep(&mut dl);
        dl.clear();
        dl.fill_rect(Rect::new(0.0, 0.0, 400.0, 300.0), Color::BLACK);
        dl.fill_rect(Rect::new(10.0, 10.0, 50.0, 20.0), Color::WHITE);
        dl.fill_rect(Rect::new(0.0, 0.0, 500.0, 290.0), Color::WHITE);
        assert_eq!(frames.damage(&dl, 2.0, Size::new(500.0, 300.0)), None);
    }
}
