//! Virtualized, sortable table with an optional tree mode.
//!
//! The table knows nothing about processes. A [`RowSource`] supplies row count,
//! stable ids, cell text, optional heat, ordering and (for tree mode) parentage; the
//! table owns sort state, scroll position, hover, selection and collapse state, and
//! paints only the rows in view.
//!
//! Selection is by [`RowId`], not position, so it survives re-sorting and rows
//! appearing or vanishing between samples. Collapse state is keyed the same way.
//!
//! In tree mode the display order is a pre-order walk of the hierarchy, with the
//! children of every node ordered by the active sort column. Collapsed subtrees are
//! skipped when the order is built, so they cost nothing per frame.

use std::cmp::Ordering;
use std::collections::HashSet;

use ot_paint::{Color, DisplayList, HAlign, Point, Rect, VAlign};

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

    /// Parent row, for tree mode. A row is a root when this is `None`, out of range,
    /// the row itself, or a hidden row. Parent links must not form cycles; the table
    /// tolerates one by listing the unreachable rows flat at the end, but that is a
    /// bug in the source, not a feature.
    fn parent(&self, _row: usize) -> Option<usize> {
        None
    }

    /// Text for a cell of a row whose children are hidden because it is collapsed.
    /// A source that can aggregate shows subtree totals here, so collapsing a branch
    /// folds its usage into the visible row instead of making it disappear.
    fn cell_collapsed(&self, row: usize, col: usize, out: &mut String) {
        self.cell(row, col, out);
    }

    /// Heat for a collapsed row; see [`RowSource::cell_collapsed`].
    fn heat_collapsed(&self, row: usize, col: usize) -> Option<f32> {
        self.heat(row, col)
    }

    /// Ordering among siblings in tree mode. Sources that aggregate should order by
    /// subtree totals, so that collapsing or expanding a node never reorders its
    /// siblings and a branch whose children are busy rises even when its root is
    /// idle.
    fn compare_subtree(&self, a: usize, b: usize, col: usize) -> Ordering {
        self.compare(a, b, col)
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
    /// Position in the current display order.
    Row(usize),
    /// The expand/collapse box of a row that has children (tree mode only).
    Expander(usize),
    Nothing,
}

/// Per-position tree facts, parallel to the display order. Empty in list mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RowMeta {
    depth: u16,
    has_children: bool,
}

/// Buffers for building the tree order, kept so steady state does not allocate.
#[derive(Debug, Default)]
struct TreeScratch {
    parent: Vec<u32>,
    child_start: Vec<u32>,
    fill: Vec<u32>,
    child_list: Vec<u32>,
    stack: Vec<(u32, u16, bool)>,
    visited: Vec<bool>,
}

/// Above this many remembered collapsed ids, ids of rows that no longer exist are
/// dropped on the next rebuild. Collapsing is rare, so this almost never runs.
const COLLAPSED_PRUNE_AT: usize = 4096;

#[derive(Debug)]
pub struct Table {
    pub columns: Vec<Column>,
    pub sort_col: usize,
    pub sort_desc: bool,
    /// Scroll offset in rows; fractional for smooth wheel scrolling.
    scroll: f32,
    /// Source row indices in display order.
    order: Vec<usize>,
    meta: Vec<RowMeta>,
    order_dirty: bool,
    /// Source length when `order` was built; a silent length change forces a rebuild
    /// so stale indices can never reach the source.
    order_src_len: usize,
    /// Hovered position in `order`.
    pub hover: Option<usize>,
    pub selected: Option<RowId>,
    tree: bool,
    collapsed: HashSet<RowId>,
    scratch: TreeScratch,
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
            meta: Vec::new(),
            order_dirty: true,
            order_src_len: 0,
            hover: None,
            selected: None,
            tree: false,
            collapsed: HashSet::new(),
            scratch: TreeScratch::default(),
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

    /// Whether rows are shown as a hierarchy.
    #[must_use]
    pub fn tree(&self) -> bool {
        self.tree
    }

    /// Switch between list and tree. The selection is kept and revealed in the new
    /// order (ancestors expanded, row centered); with nothing selected the view
    /// starts at the top, since the two orders have nothing in common.
    pub fn set_tree<S: RowSource>(&mut self, on: bool, src: &S, theme: &Theme) {
        self.tree = on;
        self.order_dirty = true;
        self.hover = None;
        if !self.reveal_selected(src, theme) {
            self.scroll = 0.0;
        }
    }

    /// Make the selected row visible: expand its ancestors in tree mode and scroll
    /// it to the middle of the view. Returns false if nothing is selected or the
    /// selected row is not in the source.
    pub fn reveal_selected<S: RowSource>(&mut self, src: &S, theme: &Theme) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        let n = src.len();
        let Some(row) = (0..n).find(|&r| src.id(r) == id) else {
            return false;
        };
        if self.tree {
            let mut r = row;
            for _ in 0..n {
                let Some(p) = src.parent(r) else {
                    break;
                };
                if p >= n || p == r || !src.visible(p) {
                    break;
                }
                if self.collapsed.remove(&src.id(p)) {
                    self.order_dirty = true;
                }
                r = p;
            }
        }
        self.ensure_order(src);
        let Some(pos) = self.order.iter().position(|&r| r == row) else {
            return false;
        };
        self.scroll_to_center(pos, theme);
        true
    }

    /// Collapse or expand the row at a display position. Returns false if the row
    /// has no children.
    pub fn toggle_expanded<S: RowSource>(&mut self, pos: usize, src: &S) -> bool {
        let Some(&row) = self.order.get(pos) else {
            return false;
        };
        if !self.meta.get(pos).is_some_and(|m| m.has_children) {
            return false;
        }
        let id = src.id(row);
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        self.order_dirty = true;
        true
    }

    /// Left arrow in tree mode: collapse the selected row, or if it is already
    /// collapsed or a leaf, move to its parent.
    pub fn collapse_or_parent<S: RowSource>(&mut self, src: &S, theme: &Theme) -> bool {
        if !self.tree {
            return false;
        }
        self.ensure_order(src);
        let Some(pos) = self.selected_pos(src) else {
            return false;
        };
        let m = self.meta[pos];
        let id = src.id(self.order[pos]);
        if m.has_children && !self.collapsed.contains(&id) {
            self.collapsed.insert(id);
            self.order_dirty = true;
            return true;
        }
        if m.depth == 0 {
            return false;
        }
        // The nearest preceding row one level up is the parent.
        let Some(parent) = (0..pos).rev().find(|&i| self.meta[i].depth < m.depth) else {
            return false;
        };
        self.selected = Some(src.id(self.order[parent]));
        self.scroll_into_view(parent, theme);
        true
    }

    /// Right arrow in tree mode: expand the selected row, or if it is already
    /// expanded, move to its first child.
    pub fn expand_or_child<S: RowSource>(&mut self, src: &S, theme: &Theme) -> bool {
        if !self.tree {
            return false;
        }
        self.ensure_order(src);
        let Some(pos) = self.selected_pos(src) else {
            return false;
        };
        if !self.meta[pos].has_children {
            return false;
        }
        let id = src.id(self.order[pos]);
        if self.collapsed.remove(&id) {
            self.order_dirty = true;
            return true;
        }
        // Expanded, so the first child is the next row.
        let next = pos + 1;
        if next >= self.order.len() {
            return false;
        }
        self.selected = Some(src.id(self.order[next]));
        self.scroll_into_view(next, theme);
        true
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
                let pos = pos as usize;
                if self.tree {
                    if let Some(m) = self.meta.get(pos) {
                        if m.has_children {
                            let x0 = self.body.x + theme.pad + f32::from(m.depth) * theme.indent;
                            if p.x >= x0 && p.x < x0 + theme.expander_w {
                                return Hit::Expander(pos);
                            }
                        }
                    }
                }
                return Hit::Row(pos);
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
        let current = self.selected_pos(src);
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

    fn selected_pos<S: RowSource>(&self, src: &S) -> Option<usize> {
        self.selected
            .and_then(|id| self.order.iter().position(|&r| src.id(r) == id))
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

    /// For jumps, as opposed to stepping: put the row in the middle so its
    /// neighbours on both sides are in view. Clamped at the next paint.
    fn scroll_to_center(&mut self, pos: usize, theme: &Theme) {
        let visible = self.rows_visible(theme).max(1) as f32;
        self.scroll = (pos as f32 - (visible - 1.0) * 0.5).floor().max(0.0);
    }

    fn ensure_order<S: RowSource>(&mut self, src: &S) {
        if !self.order_dirty && self.order_src_len == src.len() {
            return;
        }
        self.order.clear();
        self.meta.clear();
        self.order_src_len = src.len();
        if self.tree {
            self.build_tree_order(src);
        } else {
            self.build_flat_order(src);
        }
        self.order_dirty = false;
    }

    fn build_flat_order<S: RowSource>(&mut self, src: &S) {
        self.order
            .extend((0..src.len()).filter(|&r| src.visible(r)));
        let col = self.sort_col;
        let desc = self.sort_desc;
        // Tie-break on id so equal keys keep a stable order between samples instead
        // of shuffling every second.
        self.order.sort_unstable_by(|&a, &b| {
            let o = src.compare(a, b, col);
            let o = if desc { o.reverse() } else { o };
            o.then_with(|| src.id(a).0.cmp(&src.id(b).0))
        });
    }

    fn build_tree_order<S: RowSource>(&mut self, src: &S) {
        const NONE: u32 = u32::MAX;
        let n = src.len();
        let root = n as u32;

        if self.collapsed.len() > COLLAPSED_PRUNE_AT {
            let live: HashSet<RowId> = (0..n).map(|r| src.id(r)).collect();
            self.collapsed.retain(|id| live.contains(id));
        }

        let TreeScratch {
            parent,
            child_start,
            fill,
            child_list,
            stack,
            visited,
        } = &mut self.scratch;

        // Parent slot per row: `root` for roots, `NONE` for hidden rows.
        parent.clear();
        parent.extend((0..n).map(|r| {
            if !src.visible(r) {
                return NONE;
            }
            match src.parent(r) {
                Some(p) if p < n && p != r && src.visible(p) => p as u32,
                _ => root,
            }
        }));

        // Children in CSR form: node p's children are
        // `child_list[child_start[p]..child_start[p + 1]]`; slot `n` is the root.
        child_start.clear();
        child_start.resize(n + 2, 0);
        for &p in parent.iter() {
            if p != NONE {
                child_start[p as usize + 1] += 1;
            }
        }
        for i in 1..n + 2 {
            child_start[i] += child_start[i - 1];
        }
        fill.clear();
        fill.extend_from_slice(&child_start[..=n]);
        child_list.clear();
        child_list.resize(child_start[n + 1] as usize, 0);
        for (r, &p) in parent.iter().enumerate() {
            if p != NONE {
                let slot = &mut fill[p as usize];
                child_list[*slot as usize] = r as u32;
                *slot += 1;
            }
        }

        // Siblings sort by the subtree comparison; same tie-break as the flat order.
        let (col, desc) = (self.sort_col, self.sort_desc);
        for p in 0..=n {
            let range = child_start[p] as usize..child_start[p + 1] as usize;
            child_list[range].sort_unstable_by(|&a, &b| {
                let (a, b) = (a as usize, b as usize);
                let o = src.compare_subtree(a, b, col);
                let o = if desc { o.reverse() } else { o };
                o.then_with(|| src.id(a).0.cmp(&src.id(b).0))
            });
        }

        // Pre-order walk. Children are pushed in reverse so the first pops first. A
        // collapsed node's subtree is still walked, flagged hidden, so that every
        // reachable row is marked visited without being listed.
        visited.clear();
        visited.resize(n, false);
        stack.clear();
        let roots = child_start[n] as usize..child_start[n + 1] as usize;
        stack.extend(child_list[roots].iter().rev().map(|&r| (r, 0u16, false)));
        while let Some((r, depth, hidden)) = stack.pop() {
            let ri = r as usize;
            if std::mem::replace(&mut visited[ri], true) {
                continue;
            }
            let kids = child_start[ri] as usize..child_start[ri + 1] as usize;
            let has_children = !kids.is_empty();
            if !hidden {
                self.order.push(ri);
                self.meta.push(RowMeta {
                    depth,
                    has_children,
                });
            }
            if has_children {
                let hide = hidden || self.collapsed.contains(&src.id(ri));
                let d = depth.saturating_add(1);
                stack.extend(child_list[kids].iter().rev().map(|&c| (c, d, hide)));
            }
        }

        // Rows the walk could not reach (a cycle in the source) are listed flat at
        // the end rather than silently dropped.
        for (r, &p) in parent.iter().enumerate() {
            if p != NONE && !visited[r] {
                self.order.push(r);
                self.meta.push(RowMeta::default());
            }
        }
    }

    fn clamp_scroll(&mut self, theme: &Theme) {
        let visible = self.rows_visible(theme) as f32;
        let max = (self.order.len() as f32 - visible).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// The selected row's sibling block in tree mode: the guide level to highlight
    /// and the display positions it spans.
    fn active_block<S: RowSource>(&self, src: &S) -> Option<(u16, std::ops::Range<usize>)> {
        if !self.tree {
            return None;
        }
        let sel = self.selected_pos(src)?;
        let depth = self.meta.get(sel)?.depth;
        if depth == 0 {
            return None;
        }
        let parent = (0..sel).rev().find(|&i| self.meta[i].depth < depth)?;
        let end = (sel + 1..self.order.len())
            .find(|&i| self.meta[i].depth < depth)
            .unwrap_or(self.order.len());
        Some((depth - 1, parent + 1..end))
    }

    #[allow(clippy::too_many_lines)]
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
        let active = self.active_block(src);

        dl.push_clip(body);
        for vi in 0..visible {
            let pos = first + vi;
            let Some(&row) = self.order.get(pos) else {
                break;
            };
            let y = body.y + (vi as f32 - frac) * row_h;
            let rr = Rect::new(body.x, y, body.w, row_h);
            let id = src.id(row);
            let meta = if self.tree {
                self.meta.get(pos).copied().unwrap_or_default()
            } else {
                RowMeta::default()
            };
            let collapsed = meta.has_children && self.collapsed.contains(&id);

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
                let heat = if collapsed {
                    src.heat_collapsed(row, ci)
                } else {
                    src.heat(row, ci)
                };
                if let Some(h) = heat {
                    dl.fill_rect(cr, theme.heat.with_alpha(0.04 + 0.45 * h.clamp(0.0, 1.0)));
                }
                if collapsed {
                    src.cell_collapsed(row, ci, buf);
                } else {
                    src.cell(row, ci, buf);
                }
                let mut text_rect = cr.inset(theme.pad, 0.0);
                if self.tree && ci == 0 {
                    let x0 = cr.x + theme.pad;
                    for level in 0..meta.depth {
                        let gx = x0 + f32::from(level) * theme.indent + theme.expander_w * 0.5;
                        let is_active = active
                            .as_ref()
                            .is_some_and(|(l, range)| *l == level && range.contains(&pos));
                        let color = if is_active {
                            theme.tree_guide_active
                        } else {
                            theme.tree_guide
                        };
                        dl.fill_rect(Rect::new(gx.round() - 0.5, y, 1.0, row_h), color);
                    }
                    let indent = f32::from(meta.depth) * theme.indent;
                    if meta.has_children {
                        let center =
                            Point::new(x0 + indent + theme.expander_w * 0.5, y + row_h * 0.5);
                        chevron(dl, center, collapsed, theme.text_dim);
                    }
                    let shift = indent + theme.expander_w;
                    text_rect = Rect::new(
                        text_rect.x + shift,
                        text_rect.y,
                        (text_rect.w - shift).max(0.0),
                        text_rect.h,
                    );
                }
                let (style, align) = if col.numeric {
                    (theme.cell_num, HAlign::Right)
                } else {
                    (theme.cell, HAlign::Left)
                };
                dl.text(
                    buf,
                    text_rect,
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

/// A small solid triangle: pointing right when collapsed, down when expanded. Drawn
/// as geometry rather than a glyph so it is crisp at every DPI and needs no font.
fn chevron(dl: &mut DisplayList, center: Point, collapsed: bool, color: Color) {
    const S: f32 = 4.0;
    let (cx, cy) = (center.x, center.y);
    let pts = if collapsed {
        [
            Point::new(cx - S * 0.5, cy - S),
            Point::new(cx + S * 0.5, cy),
            Point::new(cx - S * 0.5, cy + S),
        ]
    } else {
        [
            Point::new(cx - S, cy - S * 0.5),
            Point::new(cx + S, cy - S * 0.5),
            Point::new(cx, cy + S * 0.5),
        ]
    };
    dl.fill_polygon(pts, color);
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

    /// A small forest. Row: (value, parent). Ids are the value times 1000.
    struct Forest {
        val: Vec<u32>,
        parent: Vec<Option<usize>>,
        hidden: Vec<bool>,
        /// When set, siblings order by this instead of `val`.
        subtree: Option<Vec<u32>>,
    }

    impl Forest {
        fn new(rows: &[(u32, Option<usize>)]) -> Self {
            Self {
                val: rows.iter().map(|r| r.0).collect(),
                parent: rows.iter().map(|r| r.1).collect(),
                hidden: vec![false; rows.len()],
                subtree: None,
            }
        }
    }

    impl RowSource for Forest {
        fn len(&self) -> usize {
            self.val.len()
        }
        fn id(&self, row: usize) -> RowId {
            RowId(u64::from(self.val[row]) * 1000)
        }
        fn cell(&self, row: usize, _col: usize, out: &mut String) {
            out.clear();
            out.push_str(&self.val[row].to_string());
        }
        fn compare(&self, a: usize, b: usize, _col: usize) -> Ordering {
            self.val[a].cmp(&self.val[b])
        }
        fn visible(&self, row: usize) -> bool {
            !self.hidden[row]
        }
        fn parent(&self, row: usize) -> Option<usize> {
            self.parent[row]
        }
        fn compare_subtree(&self, a: usize, b: usize, col: usize) -> Ordering {
            match &self.subtree {
                Some(s) => s[a].cmp(&s[b]),
                None => self.compare(a, b, col),
            }
        }
    }

    // 4(7)      <- root
    // 0(5)      <- root
    // ├─ 2(9)
    // │  └─ 3(2)
    // └─ 1(1)
    fn forest() -> Forest {
        Forest::new(&[
            (5, None),
            (1, Some(0)),
            (9, Some(0)),
            (2, Some(2)),
            (7, None),
        ])
    }

    fn tree_table() -> Table {
        let mut t = table();
        t.set_tree(true, &forest(), &Theme::dark());
        t
    }

    fn depths(t: &Table) -> Vec<u16> {
        t.meta.iter().map(|m| m.depth).collect()
    }

    #[test]
    fn tree_order_is_preorder_with_siblings_sorted() {
        let src = forest();
        let mut t = tree_table();
        t.ensure_order(&src);
        // Descending by value: roots 4(7) then 0(5); 0's children 2(9) then 1(1).
        assert_eq!(t.order, vec![4, 0, 2, 3, 1]);
        assert_eq!(depths(&t), vec![0, 0, 1, 2, 1]);
        let kids: Vec<bool> = t.meta.iter().map(|m| m.has_children).collect();
        assert_eq!(kids, vec![false, true, true, false, false]);

        // Ascending flips siblings at every level but keeps the hierarchy.
        t.set_sort(0);
        t.ensure_order(&src);
        assert_eq!(t.order, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn siblings_order_by_subtree_comparison() {
        let mut src = forest();
        // Own values put 4(7) above 0(5); subtree totals (0's branch sums to 17) put
        // 0 first. Children of 0 keep their own order here since we gave them the
        // same subtree weights as values.
        src.subtree = Some(vec![17, 1, 11, 2, 7]);
        let mut t = tree_table();
        t.ensure_order(&src);
        assert_eq!(t.order, vec![0, 2, 3, 1, 4]);
        // The list order is unaffected by the subtree comparison.
        t.set_tree(false, &src, &Theme::dark());
        t.ensure_order(&src);
        assert_eq!(t.order, vec![2, 4, 0, 3, 1]);
    }

    #[test]
    fn collapsing_hides_descendants_and_expanding_restores_them() {
        let src = forest();
        let mut t = tree_table();
        t.ensure_order(&src);
        assert!(t.toggle_expanded(1, &src), "row 0 has children");
        t.ensure_order(&src);
        assert_eq!(t.order, vec![4, 0]);
        assert!(!t.toggle_expanded(0, &src), "row 4 is a leaf");
        assert!(t.toggle_expanded(1, &src));
        t.ensure_order(&src);
        assert_eq!(t.order, vec![4, 0, 2, 3, 1]);
    }

    #[test]
    fn missing_self_and_hidden_parents_make_roots() {
        let mut src = Forest::new(&[(1, Some(99)), (2, Some(1)), (3, Some(0)), (4, Some(3))]);
        src.hidden[0] = true;
        let mut t = tree_table();
        t.ensure_order(&src);
        // Row 0 is hidden, so row 3 (its child) is a root; row 1 points at itself;
        // row 2's parent is out of range. Everyone is a root, sorted descending.
        assert_eq!(t.order, vec![3, 2, 1]);
        assert_eq!(depths(&t), vec![0, 0, 0]);
    }

    #[test]
    fn a_parent_cycle_loses_no_rows() {
        let src = Forest::new(&[(1, Some(1)), (2, Some(0)), (3, None)]);
        let mut t = tree_table();
        t.ensure_order(&src);
        assert_eq!(t.order.len(), 3);
        assert_eq!(t.order[0], 2, "the real root comes first");
    }

    #[test]
    fn reveal_expands_every_ancestor_of_the_selection() {
        let src = forest();
        let theme = Theme::dark();
        let mut t = tree_table();
        t.ensure_order(&src);
        t.toggle_expanded(2, &src); // collapse 2
        t.toggle_expanded(1, &src); // collapse 0
        t.ensure_order(&src);
        assert_eq!(t.order, vec![4, 0]);
        t.selected = Some(src.id(3));
        assert!(t.reveal_selected(&src, &theme));
        assert_eq!(t.order, vec![4, 0, 2, 3, 1]);
        assert!(t.collapsed.is_empty());
    }

    #[test]
    fn switching_modes_keeps_the_selection() {
        let src = forest();
        let theme = Theme::dark();
        let mut t = table();
        t.ensure_order(&src);
        t.selected = Some(src.id(3));
        t.set_tree(true, &src, &theme);
        assert_eq!(t.selected, Some(src.id(3)));
        assert_eq!(t.selected_pos(&src), Some(3));
        t.set_tree(false, &src, &theme);
        assert_eq!(t.selected, Some(src.id(3)));
        assert_eq!(t.order, vec![2, 4, 0, 3, 1]);
        assert_eq!(t.selected_pos(&src), Some(3));
    }

    #[test]
    fn left_and_right_walk_the_hierarchy() {
        let src = forest();
        let theme = Theme::dark();
        let mut t = tree_table();
        t.ensure_order(&src);
        t.selected = Some(src.id(2));

        // Left on an expanded parent collapses it.
        assert!(t.collapse_or_parent(&src, &theme));
        t.ensure_order(&src);
        assert_eq!(t.order, vec![4, 0, 2, 1]);
        assert_eq!(t.selected, Some(src.id(2)));
        // Left again moves to the parent.
        assert!(t.collapse_or_parent(&src, &theme));
        assert_eq!(t.selected, Some(src.id(0)));
        // Right on an expanded parent moves to the first child.
        assert!(t.expand_or_child(&src, &theme));
        assert_eq!(t.selected, Some(src.id(2)));
        // Right on a collapsed parent expands it.
        assert!(t.expand_or_child(&src, &theme));
        t.ensure_order(&src);
        assert_eq!(t.order, vec![4, 0, 2, 3, 1]);
        // Right on a leaf does nothing.
        t.selected = Some(src.id(3));
        assert!(!t.expand_or_child(&src, &theme));
        // Left on a root leaf does nothing.
        t.selected = Some(src.id(4));
        assert!(!t.collapse_or_parent(&src, &theme));
        // Neither does anything in list mode.
        t.set_tree(false, &src, &theme);
        assert!(!t.collapse_or_parent(&src, &theme));
        assert!(!t.expand_or_child(&src, &theme));
    }

    #[test]
    fn expander_hit_test_respects_depth() {
        let src = forest();
        let theme = Theme::dark();
        let mut t = tree_table();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        t.paint(
            &mut dl,
            Rect::new(0.0, 0.0, 300.0, 300.0),
            &src,
            &theme,
            &mut buf,
        );
        let row_y = |pos: usize| theme.header_h + theme.row_h * (pos as f32 + 0.5);
        // Position 1 is row 0, a depth-0 parent: its box starts at the padding.
        let x = theme.pad + theme.expander_w * 0.5;
        assert_eq!(t.hit(Point::new(x, row_y(1)), &theme), Hit::Expander(1));
        // Position 2 is row 2 at depth 1: the same x is now a guide, not the box.
        assert_eq!(t.hit(Point::new(x, row_y(2)), &theme), Hit::Row(2));
        assert_eq!(
            t.hit(Point::new(x + theme.indent, row_y(2)), &theme),
            Hit::Expander(2)
        );
        // Position 0 is a leaf: no expander anywhere.
        assert_eq!(t.hit(Point::new(x, row_y(0)), &theme), Hit::Row(0));
        // Right of the name there is only the row.
        assert_eq!(t.hit(Point::new(200.0, row_y(1)), &theme), Hit::Row(1));
        assert_eq!(dl.clip_depth(), 0);
    }

    #[test]
    fn active_block_is_the_selected_rows_siblings() {
        let src = forest();
        let mut t = tree_table();
        t.ensure_order(&src);
        t.selected = Some(src.id(3));
        // Row 3 is the only child of 2, at depth 2: guide level 1, positions 3..4.
        assert_eq!(t.active_block(&src), Some((1, 3..4)));
        t.selected = Some(src.id(1));
        // Row 1 is a child of 0 at depth 1: level 0, spanning 0's whole subtree.
        assert_eq!(t.active_block(&src), Some((0, 2..5)));
        t.selected = Some(src.id(4));
        assert_eq!(t.active_block(&src), None);
    }
}
