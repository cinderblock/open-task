//! Virtualized, sortable table.
//!
//! The table knows nothing about processes. A [`RowSource`] supplies row count,
//! stable ids, cell text, optional heat, and ordering; the table owns sort state,
//! scroll position, hover and selection, and paints only the rows in view.
//!
//! Selection is by [`RowId`], not position, so it survives re-sorting and rows
//! appearing or vanishing between samples.

use std::cmp::Ordering;

use ot_paint::{DisplayList, HAlign, Point, Rect, VAlign};

use crate::theme::Theme;

/// Stable identity of a row across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowId(pub u64);

/// Where a table gets its rows.
pub trait RowSource {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn id(&self, row: usize) -> RowId;

    /// Write the text for one cell into `out` (already cleared).
    fn cell(&self, row: usize, col: usize, out: &mut String);

    /// Background intensity `0..=1` for a cell, or `None` for no tint.
    fn heat(&self, _row: usize, _col: usize) -> Option<f32> {
        None
    }

    /// Ascending order for `col`. The table reverses for descending.
    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering;

    /// Whether a row is listed at all. Hidden rows are skipped when the display
    /// order is built, so a filter costs nothing per frame.
    fn visible(&self, _row: usize) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    pub title: &'static str,
    pub width: f32,
    /// Right-aligned with tabular figures.
    pub numeric: bool,
    /// First click sorts descending (natural for "most CPU first").
    pub default_desc: bool,
}

impl Column {
    #[must_use]
    pub const fn text(title: &'static str, width: f32) -> Self {
        Self {
            title,
            width,
            numeric: false,
            default_desc: false,
        }
    }

    #[must_use]
    pub const fn number(title: &'static str, width: f32) -> Self {
        Self {
            title,
            width,
            numeric: true,
            default_desc: true,
        }
    }
}

/// What is under a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Header(usize),
    /// Position in the current sorted order.
    Row(usize),
    Nothing,
}

#[derive(Debug)]
pub struct Table {
    pub columns: Vec<Column>,
    pub sort_col: usize,
    pub sort_desc: bool,
    /// Scroll offset in rows; fractional for smooth wheel scrolling.
    scroll: f32,
    /// Source row indices in display order.
    order: Vec<usize>,
    order_dirty: bool,
    /// Source length when `order` was built; a silent length change forces a rebuild
    /// so stale indices can never reach the source.
    order_src_len: usize,
    /// Hovered position in `order`.
    pub hover: Option<usize>,
    pub selected: Option<RowId>,
    body: Rect,
    header: Rect,
}

impl Table {
    #[must_use]
    pub fn new(columns: Vec<Column>, sort_col: usize) -> Self {
        let sort_desc = columns.get(sort_col).is_some_and(|c| c.default_desc);
        Self {
            columns,
            sort_col,
            sort_desc,
            scroll: 0.0,
            order: Vec::new(),
            order_dirty: true,
            order_src_len: 0,
            hover: None,
            selected: None,
            body: Rect::ZERO,
            header: Rect::ZERO,
        }
    }

    /// Call whenever the source's rows may have changed.
    pub fn invalidate_order(&mut self) {
        self.order_dirty = true;
    }

    /// Sort by `col`; clicking the active column flips direction.
    pub fn set_sort(&mut self, col: usize) {
        if col >= self.columns.len() {
            return;
        }
        if col == self.sort_col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_col = col;
            self.sort_desc = self.columns[col].default_desc;
        }
        self.order_dirty = true;
    }

    #[must_use]
    pub fn hit(&self, p: Point, theme: &Theme) -> Hit {
        if self.header.contains(p) {
            let mut x = self.header.x;
            for (i, c) in self.columns.iter().enumerate() {
                if p.x < x + c.width {
                    return Hit::Header(i);
                }
                x += c.width;
            }
            return Hit::Nothing;
        }
        if self.body.contains(p) {
            let pos = ((p.y - self.body.y) / theme.row_h + self.scroll).floor();
            if pos >= 0.0 && (pos as usize) < self.order.len() {
                return Hit::Row(pos as usize);
            }
        }
        Hit::Nothing
    }

    /// Source row at a display position.
    #[must_use]
    pub fn row_at(&self, pos: usize) -> Option<usize> {
        self.order.get(pos).copied()
    }

    /// Positive scrolls down. Clamped on next paint.
    pub fn scroll_lines(&mut self, delta: f32) {
        self.scroll = (self.scroll + delta).max(0.0);
    }

    #[must_use]
    pub fn rows_visible(&self, theme: &Theme) -> usize {
        (self.body.h / theme.row_h).floor().max(0.0) as usize
    }

    /// Move selection by `delta` rows, or select the first row if nothing is
    /// selected. Scrolls to keep the selection in view.
    pub fn move_selection<S: RowSource>(&mut self, src: &S, delta: isize, theme: &Theme) {
        self.ensure_order(src);
        if self.order.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| self.order.iter().position(|&r| src.id(r) == id));
        let last = self.order.len() - 1;
        let next = match current {
            None => {
                if delta >= 0 {
                    0
                } else {
                    last
                }
            }
            Some(c) => c.saturating_add_signed(delta).min(last),
        };
        self.selected = Some(src.id(self.order[next]));
        self.scroll_into_view(next, theme);
    }

    /// Select the first or last row.
    pub fn select_end<S: RowSource>(&mut self, src: &S, first: bool, theme: &Theme) {
        self.ensure_order(src);
        if self.order.is_empty() {
            return;
        }
        let pos = if first { 0 } else { self.order.len() - 1 };
        self.selected = Some(src.id(self.order[pos]));
        self.scroll_into_view(pos, theme);
    }

    fn scroll_into_view(&mut self, pos: usize, theme: &Theme) {
        let visible = self.rows_visible(theme).max(1) as f32;
        let pos = pos as f32;
        if pos < self.scroll {
            self.scroll = pos;
        } else if pos + 1.0 > self.scroll + visible {
            self.scroll = pos + 1.0 - visible;
        }
    }

    fn ensure_order<S: RowSource>(&mut self, src: &S) {
        if !self.order_dirty && self.order_src_len == src.len() {
            return;
        }
        self.order.clear();
        self.order
            .extend((0..src.len()).filter(|&r| src.visible(r)));
        self.order_src_len = src.len();
        let col = self.sort_col;
        let desc = self.sort_desc;
        // Tie-break on id so equal keys keep a stable order between samples instead
        // of shuffling every second.
        self.order.sort_unstable_by(|&a, &b| {
            let o = src.compare(a, b, col);
            let o = if desc { o.reverse() } else { o };
            o.then_with(|| src.id(a).0.cmp(&src.id(b).0))
        });
        self.order_dirty = false;
    }

    fn clamp_scroll(&mut self, theme: &Theme) {
        let visible = self.rows_visible(theme) as f32;
        let max = (self.order.len() as f32 - visible).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    pub fn paint<S: RowSource>(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        src: &S,
        theme: &Theme,
        buf: &mut String,
    ) {
        let (header, body) = rect.split_top(theme.header_h);
        self.header = header;
        self.body = body;
        self.ensure_order(src);
        self.clamp_scroll(theme);

        // Header.
        dl.fill_rect(header, theme.surface);
        let mut x = header.x;
        for (ci, col) in self.columns.iter().enumerate() {
            let cr = Rect::new(x, header.y, col.width, header.h);
            let align = if col.numeric {
                HAlign::Right
            } else {
                HAlign::Left
            };
            dl.text(
                col.title,
                cr.inset(theme.pad, 0.0),
                theme.header,
                theme.text_dim,
                align,
                VAlign::Middle,
                true,
            );
            if ci == self.sort_col {
                let bar = Rect::new(
                    cr.x + theme.pad,
                    cr.bottom() - 2.0,
                    cr.w - 2.0 * theme.pad,
                    2.0,
                );
                dl.fill_rect(bar, theme.accent);
            }
            dl.fill_rect(
                Rect::new(cr.right() - 0.5, cr.y + 7.0, 1.0, cr.h - 14.0),
                theme.grid,
            );
            x += col.width;
        }
        dl.fill_rect(
            Rect::new(header.x, header.bottom() - 1.0, header.w, 1.0),
            theme.grid,
        );

        // Body.
        let row_h = theme.row_h;
        let first = self.scroll.floor();
        let frac = self.scroll - first;
        let first = first as usize;
        let visible = (body.h / row_h).ceil() as usize + 1;

        dl.push_clip(body);
        for vi in 0..visible {
            let pos = first + vi;
            let Some(&row) = self.order.get(pos) else {
                break;
            };
            let y = body.y + (vi as f32 - frac) * row_h;
            let rr = Rect::new(body.x, y, body.w, row_h);
            let id = src.id(row);

            if self.selected == Some(id) {
                dl.fill_rect(rr, theme.row_selected);
            } else if self.hover == Some(pos) {
                dl.fill_rect(rr, theme.row_hover);
            } else if pos % 2 == 1 {
                dl.fill_rect(rr, theme.row_alt);
            }

            let mut x = body.x;
            for (ci, col) in self.columns.iter().enumerate() {
                let cr = Rect::new(x, y, col.width, row_h);
                if let Some(h) = src.heat(row, ci) {
                    dl.fill_rect(cr, theme.heat.with_alpha(0.04 + 0.45 * h.clamp(0.0, 1.0)));
                }
                src.cell(row, ci, buf);
                let (style, align) = if col.numeric {
                    (theme.cell_num, HAlign::Right)
                } else {
                    (theme.cell, HAlign::Left)
                };
                dl.text(
                    buf,
                    cr.inset(theme.pad, 0.0),
                    style,
                    theme.text,
                    align,
                    VAlign::Middle,
                    true,
                );
                x += col.width;
            }
        }
        dl.pop_clip();

        // Scrollbar thumb.
        let total = self.order.len() as f32;
        let vis = self.rows_visible(theme) as f32;
        if total > vis && vis > 0.0 {
            let track = Rect::new(body.right() - 6.0, body.y + 2.0, 4.0, body.h - 4.0);
            let thumb_h = (track.h * vis / total).max(20.0);
            let thumb_y = track.y + (track.h - thumb_h) * (self.scroll / (total - vis));
            dl.fill_round_rect(
                Rect::new(track.x, thumb_y, track.w, thumb_h),
                2.0,
                theme.scrollbar,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nums(Vec<u32>);

    impl RowSource for Nums {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn id(&self, row: usize) -> RowId {
            RowId(u64::from(self.0[row]) * 1000)
        }
        fn cell(&self, row: usize, _col: usize, out: &mut String) {
            out.clear();
            out.push_str(&self.0[row].to_string());
        }
        fn compare(&self, a: usize, b: usize, _col: usize) -> Ordering {
            self.0[a].cmp(&self.0[b])
        }
    }

    fn table() -> Table {
        Table::new(vec![Column::number("n", 50.0)], 0)
    }

    #[test]
    fn sorts_descending_by_default_for_numeric_and_flips_on_reclick() {
        let src = Nums(vec![3, 1, 2]);
        let mut t = table();
        t.ensure_order(&src);
        assert_eq!(t.order, vec![0, 2, 1]);
        t.set_sort(0);
        t.ensure_order(&src);
        assert_eq!(t.order, vec![1, 2, 0]);
    }

    #[test]
    fn selection_follows_id_not_position() {
        let mut src = Nums(vec![3, 1, 2]);
        let mut t = table();
        let theme = Theme::dark();
        t.move_selection(&src, 1, &theme);
        assert_eq!(t.selected, Some(RowId(3000)));
        // Data changes: 3 is now the smallest. Selection must still be "3".
        src.0 = vec![3, 10, 20];
        t.invalidate_order();
        t.ensure_order(&src);
        assert_eq!(t.selected, Some(RowId(3000)));
        assert_eq!(t.order, vec![2, 1, 0]);
    }

    struct HideOdd(Vec<u32>);

    impl RowSource for HideOdd {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn id(&self, row: usize) -> RowId {
            RowId(u64::from(self.0[row]))
        }
        fn cell(&self, _row: usize, _col: usize, out: &mut String) {
            out.clear();
        }
        fn compare(&self, a: usize, b: usize, _col: usize) -> Ordering {
            self.0[a].cmp(&self.0[b])
        }
        fn visible(&self, row: usize) -> bool {
            self.0[row].is_multiple_of(2)
        }
    }

    #[test]
    fn hidden_rows_are_excluded_from_order() {
        let src = HideOdd(vec![1, 2, 3, 4, 5, 6]);
        let mut t = table();
        t.ensure_order(&src);
        assert_eq!(t.order, vec![5, 3, 1]);
        // Same length, different contents, no invalidate: order is reused (documented
        // contract: callers invalidate on data change). A length change rebuilds.
        let src = HideOdd(vec![2, 4]);
        t.ensure_order(&src);
        assert_eq!(t.order, vec![1, 0]);
    }

    #[test]
    fn paint_only_emits_visible_rows() {
        let src = Nums((0..1000).collect());
        let mut t = table();
        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        t.paint(
            &mut dl,
            Rect::new(0.0, 0.0, 100.0, theme.header_h + theme.row_h * 10.0),
            &src,
            &theme,
            &mut buf,
        );
        let texts = dl
            .cmds()
            .iter()
            .filter(|c| matches!(c, ot_paint::DrawCmd::Text(_)))
            .count();
        // 1 header label + at most 11 visible rows (10 plus one partial).
        assert!(texts <= 12, "emitted {texts} text runs");
        assert_eq!(dl.clip_depth(), 0);
    }

    #[test]
    fn hit_maps_header_and_rows() {
        let src = Nums((0..50).collect());
        let mut t = table();
        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        t.paint(
            &mut dl,
            Rect::new(0.0, 0.0, 100.0, 300.0),
            &src,
            &theme,
            &mut buf,
        );
        assert_eq!(t.hit(Point::new(10.0, 5.0), &theme), Hit::Header(0));
        assert_eq!(
            t.hit(Point::new(10.0, theme.header_h + 1.0), &theme),
            Hit::Row(0)
        );
        assert_eq!(
            t.hit(Point::new(10.0, theme.header_h + theme.row_h * 2.5), &theme),
            Hit::Row(2)
        );
        assert_eq!(t.hit(Point::new(500.0, 5.0), &theme), Hit::Nothing);
    }
}
