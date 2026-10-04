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
//!
//! A re-sort because the data changed ([`Table::refresh`]) is gentler than one the
//! user asked for: the selected row keeps its place on screen, scrolling the rest
//! around it, and with animation on, rows slide from where they were to where they
//! now belong instead of jumping.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

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

    /// Whether rows carry an image (a program's icon) at the start of the first
    /// column. When true the column reserves the room on every row, so names line
    /// up whether or not a row has one.
    fn images(&self) -> bool {
        false
    }

    /// The path of the file whose icon stands for this row, when it has one.
    fn image(&self, _row: usize) -> Option<&str> {
        None
    }

    /// Whether a listed row is there for context rather than on its own merits (an
    /// ancestor of a search match, say). Painted in the dim text color.
    fn muted(&self, _row: usize) -> bool {
        false
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

    /// Whether a row with children starts out folded. A process's child processes
    /// are worth seeing by default; its threads are not, until asked. The table
    /// remembers the user's toggles relative to this default.
    fn collapsed_by_default(&self, _row: usize) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub title: &'static str,
    pub width: f32,
    /// Right-aligned with tabular figures.
    pub numeric: bool,
    /// First click sorts descending (natural for "most CPU first").
    pub default_desc: bool,
    /// Shown, as opposed to turned off in the column chooser. The width and the
    /// sort state of a hidden column are kept for when it comes back.
    pub visible: bool,
}

impl Column {
    #[must_use]
    pub const fn text(title: &'static str, width: f32) -> Self {
        Self {
            title,
            width,
            numeric: false,
            default_desc: false,
            visible: true,
        }
    }

    #[must_use]
    pub const fn number(title: &'static str, width: f32) -> Self {
        Self {
            title,
            width,
            numeric: true,
            default_desc: true,
            visible: true,
        }
    }

    /// The same column, off until chosen.
    #[must_use]
    pub const fn hidden(self) -> Self {
        Self {
            visible: false,
            ..self
        }
    }
}

/// The side of a row's icon, in DIPs: the small icon size at 100 %.
pub(crate) const ROW_ICON: f32 = 16.0;

/// One column's place in a saved layout: what the shell persists, by title so a
/// layout survives columns being added between versions.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnLayout {
    pub title: String,
    pub width: f32,
    pub visible: bool,
}

/// What is under a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Header(usize),
    /// The right edge of column `i` in the header: dragging it resizes the column.
    Divider(usize),
    /// Position in the current display order.
    Row(usize),
    /// The expand/collapse box of a row that has children (tree mode only).
    Expander(usize),
    Nothing,
}

/// Narrowest a column can be dragged.
const MIN_COLUMN_W: f32 = 40.0;
/// How close to a column edge counts as grabbing it, in DIPs either side.
const DIVIDER_GRAB: f32 = 4.0;

/// A column-resize drag in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Resize {
    col: usize,
    /// Pointer x when the drag started, and the column's width then.
    start_x: f32,
    start_w: f32,
}

/// Pointer movement before a header press becomes a drag rather than a click.
const DRAG_START: f32 = 6.0;

/// The mouse is down on a column header: a click to sort until the pointer has
/// moved [`DRAG_START`], a drag to reorder after that.
#[derive(Debug, Clone, Copy, PartialEq)]
struct HeaderPress {
    col: usize,
    start: Point,
    /// Where the pointer is now, once dragging.
    at: Option<Point>,
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

/// How long rows take to slide to their new places after a re-sort.
const SLIDE: Duration = Duration::from_millis(150);

/// Rows in motion after a re-sort. Positions are in rows relative to the top of
/// the body ("screen rows"), so scrolling during a slide moves everything together.
#[derive(Debug, Default)]
struct Slide {
    /// When the slide began: the first frame painted after the re-sort.
    start: Option<Instant>,
    /// Rows on screen now that were somewhere else: how far above (negative) or
    /// below their place they start, in rows. A row that came from off screen
    /// starts just outside the edge it came from.
    offsets: HashMap<RowId, f32>,
    /// Rows that were on screen and no longer are: their new display position and
    /// the screen row they leave from. They slide out over the nearer edge.
    leaving: Vec<(usize, f32)>,
}

impl Slide {
    fn is_empty(&self) -> bool {
        self.offsets.is_empty() && self.leaving.is_empty()
    }

    fn clear(&mut self) {
        self.start = None;
        self.offsets.clear();
        self.leaving.clear();
    }
}

/// Ease-out cubic: quick to leave, gentle to land.
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

// Sort direction, dirtiness, refresh, animation and tree mode are independent
// switches; folding them into an enum or bit set would only obscure them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub struct Table {
    pub columns: Vec<Column>,
    /// Indices into `columns` of the visible columns, left to right.
    display: Vec<usize>,
    pub sort_col: usize,
    pub sort_desc: bool,
    /// A press on a header, see [`HeaderPress`].
    header_press: Option<HeaderPress>,
    /// Scroll offset in rows; fractional for smooth wheel scrolling.
    scroll: f32,
    /// Horizontal scroll offset in DIPs, when the columns are wider than the table.
    scroll_x: f32,
    resize: Option<Resize>,
    /// Source row indices in display order.
    order: Vec<usize>,
    /// The ids of `order`, as of when it was built, so the next rebuild can tell
    /// where each row was after the source has changed underneath.
    order_ids: Vec<RowId>,
    /// The previous `order_ids`, during a rebuild; kept for its allocation.
    prev_ids: Vec<RowId>,
    /// Row height at the last paint, for the refresh bookkeeping that happens
    /// between paints.
    row_h: f32,
    meta: Vec<RowMeta>,
    order_dirty: bool,
    /// The next rebuild is a re-sort because the data changed ([`Table::refresh`]).
    refreshing: bool,
    /// Slide rows to their new places on a refresh.
    animate: bool,
    slide: Slide,
    /// The frame time [`Table::tick`] last gave; `None` never animates.
    now: Option<Instant>,
    /// Scratch for a rebuild: where each row was, and where it is now.
    was_at: HashMap<RowId, usize>,
    is_at: HashMap<RowId, usize>,
    /// Source length when `order` was built; a silent length change forces a rebuild
    /// so stale indices can never reach the source.
    order_src_len: usize,
    /// Hovered position in `order`.
    pub hover: Option<usize>,
    pub selected: Option<RowId>,
    tree: bool,
    /// Rows whose fold state the user flipped away from the source's default.
    collapsed: HashSet<RowId>,
    scratch: TreeScratch,
    body: Rect,
    header: Rect,
}

impl Table {
    #[must_use]
    pub fn new(columns: Vec<Column>, sort_col: usize) -> Self {
        let sort_desc = columns.get(sort_col).is_some_and(|c| c.default_desc);
        let display = columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.visible)
            .map(|(i, _)| i)
            .collect();
        Self {
            columns,
            display,
            sort_col,
            sort_desc,
            header_press: None,
            scroll: 0.0,
            scroll_x: 0.0,
            resize: None,
            order: Vec::new(),
            order_ids: Vec::new(),
            prev_ids: Vec::new(),
            row_h: 0.0,
            meta: Vec::new(),
            order_dirty: true,
            refreshing: false,
            animate: false,
            slide: Slide::default(),
            now: None,
            was_at: HashMap::new(),
            is_at: HashMap::new(),
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

    /// The visible columns, left to right, with their indices into `columns`.
    pub fn shown(&self) -> impl Iterator<Item = (usize, &Column)> + '_ {
        self.display.iter().map(move |&i| (i, &self.columns[i]))
    }

    /// Show or hide column `col`. A column that comes back goes to the right end.
    /// The last visible column cannot be hidden. Returns whether anything changed.
    pub fn set_visible(&mut self, col: usize, on: bool) -> bool {
        if col >= self.columns.len() || self.columns[col].visible == on {
            return false;
        }
        if !on && self.display.len() <= 1 {
            return false;
        }
        self.columns[col].visible = on;
        if on {
            self.display.push(col);
        } else {
            self.display.retain(|&i| i != col);
        }
        true
    }

    /// Put every column back as it was made: `defaults` in their order, with their
    /// widths and visibility, keeping the sort if its column still exists.
    pub fn reset_columns(&mut self, defaults: Vec<Column>) {
        let sort_title = self.columns.get(self.sort_col).map(|c| c.title);
        self.sort_col = defaults
            .iter()
            .position(|c| Some(c.title) == sort_title)
            .unwrap_or(0);
        self.columns = defaults;
        self.display = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.visible)
            .map(|(i, _)| i)
            .collect();
        self.scroll_x = 0.0;
    }

    /// The columns as they are, for saving: the visible ones in display order,
    /// then the hidden ones.
    #[must_use]
    pub fn layout(&self) -> Vec<ColumnLayout> {
        let entry = |c: &Column| ColumnLayout {
            title: c.title.to_owned(),
            width: c.width,
            visible: c.visible,
        };
        self.shown()
            .map(|(_, c)| entry(c))
            .chain(self.columns.iter().filter(|c| !c.visible).map(entry))
            .collect()
    }

    /// Restore a saved layout. Columns it names get its width, visibility and
    /// order; columns it does not name keep their defaults and go after the named
    /// ones; names that no longer exist are ignored. A layout that would hide
    /// every column is ignored.
    pub fn apply_layout(&mut self, layout: &[ColumnLayout]) {
        let mut display = Vec::with_capacity(self.columns.len());
        let mut seen = vec![false; self.columns.len()];
        for l in layout {
            let Some(i) = self.columns.iter().position(|c| c.title == l.title) else {
                continue;
            };
            if seen[i] {
                continue;
            }
            seen[i] = true;
            let c = &mut self.columns[i];
            c.width = l.width.max(MIN_COLUMN_W);
            c.visible = l.visible;
            if l.visible {
                display.push(i);
            }
        }
        for (i, c) in self.columns.iter().enumerate() {
            if !seen[i] && c.visible {
                display.push(i);
            }
        }
        if display.is_empty() {
            for c in &mut self.columns {
                c.visible = true;
            }
            display = (0..self.columns.len()).collect();
        }
        self.display = display;
    }

    /// The column at display position `pos`, by its index into `columns`.
    #[must_use]
    pub fn column_at(&self, pos: usize) -> Option<usize> {
        self.display.get(pos).copied()
    }

    /// The mouse went down on column `col`'s header at `p`: a sort if it comes
    /// back up there, a reorder if it moves first.
    pub fn begin_header_press(&mut self, col: usize, p: Point) {
        if col < self.columns.len() {
            self.header_press = Some(HeaderPress {
                col,
                start: p,
                at: None,
            });
        }
    }

    /// The pointer moved with a header pressed. Returns whether a repaint is due
    /// (the drag started or moved).
    pub fn header_drag_to(&mut self, p: Point) -> bool {
        let Some(h) = self.header_press.as_mut() else {
            return false;
        };
        if h.at.is_none() && (p.x - h.start.x).abs() < DRAG_START {
            return false;
        }
        h.at = Some(p);
        true
    }

    /// The mouse came up. A press that never became a drag is a click on the
    /// column, returned for the caller to sort by; a drag drops the column where
    /// the pointer is and returns `None`.
    pub fn end_header_press(&mut self, p: Point) -> Option<usize> {
        let h = self.header_press.take()?;
        if h.at.is_none() {
            return Some(h.col);
        }
        let from = self.display.iter().position(|&i| i == h.col)?;
        let to = self.drop_slot(p.x).min(self.display.len());
        // Removing the dragged column shifts the slots after it left by one.
        let to = if to > from { to - 1 } else { to };
        let col = self.display.remove(from);
        self.display.insert(to, col);
        None
    }

    /// Whether a header is being dragged to a new place.
    #[must_use]
    pub fn header_dragging(&self) -> bool {
        self.header_press.is_some_and(|h| h.at.is_some())
    }

    /// The display slot a column dropped at `x` would take: before the first
    /// column whose middle is right of `x`.
    fn drop_slot(&self, x: f32) -> usize {
        let mut left = self.header.x - self.scroll_x;
        for (slot, (_, c)) in self.shown().enumerate() {
            if x < left + c.width * 0.5 {
                return slot;
            }
            left += c.width;
        }
        self.display.len()
    }

    /// The data changed (a new sample): re-sort, keeping the selected row where it
    /// is on screen if it is on screen, and sliding rows to their new places if
    /// animation is on.
    pub fn refresh(&mut self) {
        self.order_dirty = true;
        self.refreshing = true;
    }

    /// Whether a refresh slides rows rather than jumping them. Turning it off stops
    /// a slide in progress.
    pub fn set_animate(&mut self, on: bool) {
        self.animate = on;
        if !on {
            self.slide.clear();
        }
    }

    /// The time of the frame about to be painted, for the slide.
    pub fn tick(&mut self, now: Instant) {
        self.now = Some(now);
    }

    /// Whether rows are still sliding, so the shell should paint another frame.
    #[must_use]
    pub fn animating(&self) -> bool {
        !self.slide.is_empty()
    }

    /// Where the header was painted last.
    #[must_use]
    pub fn header_rect(&self) -> Rect {
        self.header
    }

    /// Where the header and body were painted last.
    #[must_use]
    pub fn rect(&self) -> Rect {
        Rect::new(
            self.header.x,
            self.header.y,
            self.header.w,
            self.body.bottom() - self.header.y,
        )
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
        // A different arrangement is a jump, not a re-sort: nothing to keep in
        // place and nothing to slide.
        self.refreshing = false;
        self.slide.clear();
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
                if self.is_collapsed(src, p) {
                    self.set_collapsed(src, p, false);
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
        let row = self.order[pos];
        if m.has_children && !self.is_collapsed(src, row) {
            self.set_collapsed(src, row, true);
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
        let row = self.order[pos];
        if self.is_collapsed(src, row) {
            self.set_collapsed(src, row, false);
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

    /// Effective fold state of a row: the source's default, flipped if the user
    /// toggled it.
    fn is_collapsed<S: RowSource>(&self, src: &S, row: usize) -> bool {
        src.collapsed_by_default(row) != self.collapsed.contains(&src.id(row))
    }

    fn set_collapsed<S: RowSource>(&mut self, src: &S, row: usize, on: bool) {
        if self.is_collapsed(src, row) != on {
            let id = src.id(row);
            if !self.collapsed.remove(&id) {
                self.collapsed.insert(id);
            }
            self.order_dirty = true;
        }
    }

    #[must_use]
    pub fn hit(&self, p: Point, theme: &Theme) -> Hit {
        if self.header.contains(p) {
            if let Some(i) = self.divider_at(p) {
                return Hit::Divider(i);
            }
            let mut x = self.header.x - self.scroll_x;
            for (i, c) in self.shown() {
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
                            let x0 = self.body.x - self.scroll_x
                                + theme.pad
                                + f32::from(m.depth) * theme.indent;
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

    /// The column whose right edge is under `p`, if `p` is in the header and within
    /// grabbing distance of one. The cursor changes here before any drag starts.
    #[must_use]
    pub fn divider_at(&self, p: Point) -> Option<usize> {
        if !self.header.contains(p) {
            return None;
        }
        let mut edge = self.header.x - self.scroll_x;
        for (i, c) in self.shown() {
            edge += c.width;
            if (p.x - edge).abs() <= DIVIDER_GRAB {
                return Some(i);
            }
        }
        None
    }

    /// Start dragging column `col`'s right edge from pointer position `p`.
    pub fn begin_resize(&mut self, col: usize, p: Point) {
        if let Some(c) = self.columns.get(col) {
            self.resize = Some(Resize {
                col,
                start_x: p.x,
                start_w: c.width,
            });
        }
    }

    /// Continue a drag. Returns true if a width changed.
    pub fn resize_to(&mut self, p: Point) -> bool {
        let Some(r) = self.resize else {
            return false;
        };
        let Some(c) = self.columns.get_mut(r.col) else {
            return false;
        };
        let w = (r.start_w + p.x - r.start_x).max(MIN_COLUMN_W);
        if (w - c.width).abs() < f32::EPSILON {
            return false;
        }
        c.width = w;
        true
    }

    pub fn end_resize(&mut self) {
        self.resize = None;
    }

    #[must_use]
    pub fn resizing(&self) -> bool {
        self.resize.is_some()
    }

    /// Sum of the visible columns' widths.
    #[must_use]
    pub fn total_width(&self) -> f32 {
        self.shown().map(|(_, c)| c.width).sum()
    }

    /// Positive scrolls the columns to the left. Clamped on next paint.
    pub fn scroll_x_by(&mut self, dx: f32) {
        self.scroll_x = (self.scroll_x + dx).max(0.0);
    }

    /// Where the selected row is drawn, if it is selected and in view.
    #[must_use]
    pub fn selected_rect<S: RowSource>(&self, src: &S, theme: &Theme) -> Option<Rect> {
        let pos = self.selected_pos(src)?;
        let y = self.body.y + (pos as f32 - self.scroll) * theme.row_h;
        let r = Rect::new(self.body.x, y, self.body.w, theme.row_h);
        (r.bottom() > self.body.y && y < self.body.bottom()).then_some(r)
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
        let refreshing = std::mem::take(&mut self.refreshing);
        // Before the old order goes: where the selection sat on screen, and where
        // every row was.
        let anchor = if refreshing {
            self.selected_screen_row()
        } else {
            None
        };
        let old_scroll = self.scroll;
        std::mem::swap(&mut self.order_ids, &mut self.prev_ids);
        let slide = refreshing && self.animate && self.row_h > 0.0 && !self.prev_ids.is_empty();

        self.order.clear();
        self.meta.clear();
        self.order_src_len = src.len();
        if self.tree {
            self.build_tree_order(src);
        } else {
            self.build_flat_order(src);
        }
        self.order_dirty = false;
        self.order_ids.clear();
        self.order_ids.extend(self.order.iter().map(|&r| src.id(r)));

        if let Some(screen) = anchor {
            let id = self.selected;
            if let Some(pos) = self.order_ids.iter().position(|&x| Some(x) == id) {
                self.scroll = (pos as f32 - screen).max(0.0);
            }
        }
        if slide {
            self.record_slide(old_scroll);
        }
    }

    /// The selected row's position relative to the top of the body, in rows, if it
    /// is on screen in the order as last built.
    fn selected_screen_row(&self) -> Option<f32> {
        let id = self.selected?;
        let pos = self.order_ids.iter().position(|&x| x == id)?;
        let screen = pos as f32 - self.scroll;
        let rows = if self.row_h > 0.0 {
            self.body.h / self.row_h
        } else {
            0.0
        };
        (screen > -1.0 && screen < rows).then_some(screen)
    }

    /// After a refresh rebuilt the order: start sliding every row on screen from
    /// where it was (`prev_ids`, scrolled by `old_scroll`) to where it is now, and
    /// every row that was on screen and no longer is off over the nearer edge.
    fn record_slide(&mut self, old_scroll: f32) {
        self.slide.clear();
        let view = self.body.h / self.row_h;
        let span = view.ceil() as usize + 1;
        self.was_at.clear();
        self.was_at
            .extend(self.prev_ids.iter().enumerate().map(|(i, &id)| (id, i)));
        self.is_at.clear();
        self.is_at
            .extend(self.order_ids.iter().enumerate().map(|(i, &id)| (id, i)));

        let first = self.scroll.max(0.0).floor() as usize;
        let now_shown = first..(first + span).min(self.order_ids.len());
        for pos in now_shown.clone() {
            let id = self.order_ids[pos];
            // A row that is new appears in place.
            let Some(&was) = self.was_at.get(&id) else {
                continue;
            };
            let from = (was as f32 - old_scroll).clamp(-1.0, view);
            let to = pos as f32 - self.scroll;
            if (from - to).abs() > 0.01 {
                self.slide.offsets.insert(id, from - to);
            }
        }
        let first_old = old_scroll.max(0.0).floor() as usize;
        for was in first_old..(first_old + span).min(self.prev_ids.len()) {
            // A row that is gone just goes.
            if let Some(&pos) = self.is_at.get(&self.prev_ids[was]) {
                if !now_shown.contains(&pos) {
                    self.slide.leaving.push((pos, was as f32 - old_scroll));
                }
            }
        }
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

        let toggled = &self.collapsed;
        let is_collapsed = |r: usize| src.collapsed_by_default(r) != toggled.contains(&src.id(r));
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
                let hide = hidden || is_collapsed(ri);
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
        let max_x = (self.total_width() - self.body.w).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
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

    /// How far the slide has come, `0.0..=1.0` (eased); `1.0` when nothing is
    /// sliding. The first frame painted after a refresh starts the clock. Without a
    /// frame time (no [`Table::tick`]) a slide finishes at once.
    fn slide_progress(&mut self) -> f32 {
        if self.slide.is_empty() {
            return 1.0;
        }
        let Some(now) = self.now else {
            return 1.0;
        };
        let start = *self.slide.start.get_or_insert(now);
        ease(now.saturating_duration_since(start).as_secs_f32() / SLIDE.as_secs_f32())
    }

    /// One row at `y`, display position `pos`: background, heat, cells, and in
    /// the tree its guides and chevron.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn paint_row<S: RowSource>(
        &self,
        dl: &mut DisplayList,
        src: &S,
        theme: &Theme,
        buf: &mut String,
        pos: usize,
        y: f32,
        active: Option<&(u16, std::ops::Range<usize>)>,
        lifted: bool,
    ) {
        let body = self.body;
        let row_h = theme.row_h;
        let ox = -self.scroll_x;
        let row = self.order[pos];
        let rr = Rect::new(body.x, y, body.w, row_h);
        let id = src.id(row);
        let meta = if self.tree {
            self.meta.get(pos).copied().unwrap_or_default()
        } else {
            RowMeta::default()
        };
        let collapsed =
            meta.has_children && (src.collapsed_by_default(row) != self.collapsed.contains(&id));

        if lifted {
            // A row in motion slides over the rows it crosses: an opaque backing in
            // the window's own color so their text does not show through it, and a
            // hairline along each edge so it reads as a row lifted off the table.
            dl.fill_rect(rr, theme.bg_solid);
            dl.fill_rect(rr, theme.surface);
            dl.fill_rect(Rect::new(rr.x, rr.y, rr.w, 1.0), theme.grid);
            dl.fill_rect(Rect::new(rr.x, rr.bottom() - 1.0, rr.w, 1.0), theme.grid);
        }
        if self.selected == Some(id) {
            dl.fill_rect(rr, theme.row_selected);
        } else if self.hover == Some(pos) {
            dl.fill_rect(rr, theme.row_hover);
        } else if pos % 2 == 1 {
            dl.fill_rect(rr, theme.row_alt);
        }
        let text_color = if src.muted(row) {
            theme.text_dim
        } else {
            theme.text
        };

        let mut x = body.x + ox;
        for (slot, (ci, col)) in self.shown().enumerate() {
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
            if self.tree && slot == 0 {
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
                    let center = Point::new(x0 + indent + theme.expander_w * 0.5, y + row_h * 0.5);
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
            if slot == 0 && src.images() {
                // The icon's room is reserved on every row; the icon itself only
                // where the source has one.
                if let Some(path) = src.image(row) {
                    let icon = Rect::new(
                        text_rect.x,
                        y + ((row_h - ROW_ICON) * 0.5).floor(),
                        ROW_ICON,
                        ROW_ICON,
                    );
                    dl.image(path, icon);
                }
                let shift = ROW_ICON + theme.pad * 0.5;
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
                text_color,
                align,
                VAlign::Middle,
                true,
            );
            x += col.width;
        }
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
        self.row_h = theme.row_h;
        self.ensure_order(src);
        self.clamp_scroll(theme);
        // Columns slide left by the horizontal scroll; rows and their backgrounds
        // span the whole width regardless.
        let ox = -self.scroll_x;

        // Header.
        dl.fill_rect(header, theme.surface);
        dl.push_clip(header);
        let drag = self.header_press.and_then(|h| h.at.map(|at| (h.col, at)));
        let mut x = header.x + ox;
        for (ci, col) in self.shown() {
            let cr = Rect::new(x, header.y, col.width, header.h);
            let align = if col.numeric {
                HAlign::Right
            } else {
                HAlign::Left
            };
            let color = if drag.is_some_and(|(c, _)| c == ci) {
                theme.text_dim.with_alpha(0.4)
            } else {
                theme.text_dim
            };
            dl.text(
                col.title,
                cr.inset(theme.pad, 0.0),
                theme.header,
                color,
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
        // A column being dragged: its title follows the pointer, and a bar marks
        // where it would land.
        if let Some((ci, at)) = drag {
            let col = &self.columns[ci];
            let slot = self.drop_slot(at.x);
            let mark_x = header.x + ox + self.shown().take(slot).map(|(_, c)| c.width).sum::<f32>();
            dl.fill_rect(
                Rect::new(mark_x - 1.0, header.y + 4.0, 2.0, header.h - 8.0),
                theme.accent,
            );
            let ghost = Rect::new(at.x - col.width * 0.5, header.y, col.width, header.h);
            dl.fill_round_rect(
                ghost.inset(0.0, 3.0),
                theme.card_radius,
                theme.button_active,
            );
            dl.text(
                col.title,
                ghost.inset(theme.pad, 0.0),
                theme.header,
                theme.text,
                HAlign::Center,
                VAlign::Middle,
                true,
            );
        }
        dl.pop_clip();
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
        let settle = self.slide_progress();
        if settle >= 1.0 {
            // Landed: this frame draws everything at rest.
            self.slide.clear();
        }

        dl.push_clip(body);
        // Rows at rest first, rows in motion over them.
        for moving in [false, true] {
            for vi in 0..visible {
                let pos = first + vi;
                let Some(&row) = self.order.get(pos) else {
                    break;
                };
                let offset = self.slide.offsets.get(&src.id(row)).copied();
                if offset.is_some() != moving {
                    continue;
                }
                let screen = vi as f32 - frac + offset.unwrap_or(0.0) * (1.0 - settle);
                let y = body.y + screen * row_h;
                self.paint_row(dl, src, theme, buf, pos, y, active.as_ref(), moving);
            }
        }
        let view = body.h / row_h;
        for &(pos, from) in &self.slide.leaving {
            if pos >= self.order.len() {
                continue;
            }
            let to = (pos as f32 - self.scroll).clamp(-1.0, view);
            let y = body.y + (from + (to - from) * settle) * row_h;
            self.paint_row(dl, src, theme, buf, pos, y, active.as_ref(), true);
        }
        dl.pop_clip();

        // Scrollbar thumbs, drawn only when there is something to scroll.
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
        let total_w = self.total_width();
        if total_w > body.w && body.w > 12.0 {
            let track = Rect::new(body.x + 2.0, body.bottom() - 6.0, body.w - 12.0, 4.0);
            let thumb_w = (track.w * body.w / total_w).max(20.0);
            let thumb_x = track.x + (track.w - thumb_w) * (self.scroll_x / (total_w - body.w));
            dl.fill_round_rect(
                Rect::new(thumb_x, track.y, thumb_w, track.h),
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

    /// Rows with a program icon on the even numbers.
    struct Iconed(Vec<u32>);

    impl RowSource for Iconed {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn id(&self, row: usize) -> RowId {
            RowId(u64::from(self.0[row]))
        }
        fn cell(&self, _row: usize, _col: usize, out: &mut String) {
            out.clear();
            out.push('n');
        }
        fn compare(&self, a: usize, b: usize, _col: usize) -> Ordering {
            self.0[a].cmp(&self.0[b])
        }
        fn images(&self) -> bool {
            true
        }
        fn image(&self, row: usize) -> Option<&str> {
            self.0[row].is_multiple_of(2).then_some(r"C:\x\a.exe")
        }
    }

    #[test]
    fn rows_with_images_draw_them_and_every_row_leaves_the_room() {
        use ot_paint::DrawCmd;
        let src = Iconed(vec![2, 1, 4]);
        let mut t = Table::new(vec![Column::text("name", 200.0)], 0);
        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        t.paint(
            &mut dl,
            Rect::new(0.0, 0.0, 300.0, 300.0),
            &src,
            &theme,
            &mut buf,
        );
        let images: Vec<(String, Rect)> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Image { path, rect } => Some((dl.str(*path).to_owned(), *rect)),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 2, "the two even rows: {images:?}");
        assert!(images
            .iter()
            .all(|(p, r)| p == r"C:\x\a.exe" && r.w == ROW_ICON));
        // Every row's text starts past the icon's room, icon or not, so names
        // line up.
        let texts: Vec<Rect> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) if dl.str(t.text) == "n" => Some(t.rect),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 3);
        let x0 = theme.pad + ROW_ICON + theme.pad * 0.5;
        assert!(texts.iter().all(|r| (r.x - x0).abs() < 0.01), "{texts:?}");
        assert!(images.iter().all(|(_, r)| (r.x - theme.pad).abs() < 0.01));
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
    fn dragging_a_divider_resizes_and_scrolling_shifts_hits() {
        let src = Nums((0..50).collect());
        let mut t = Table::new(
            vec![Column::number("a", 100.0), Column::number("b", 100.0)],
            0,
        );
        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        let rect = Rect::new(0.0, 0.0, 150.0, 300.0);
        t.paint(&mut dl, rect, &src, &theme, &mut buf);
        assert_eq!(dl.clip_depth(), 0);

        // The first column's right edge is at x = 100; nearby is the divider.
        assert_eq!(t.hit(Point::new(98.0, 5.0), &theme), Hit::Divider(0));
        assert_eq!(t.hit(Point::new(103.0, 5.0), &theme), Hit::Divider(0));
        assert_eq!(t.hit(Point::new(110.0, 5.0), &theme), Hit::Header(1));
        assert_eq!(
            t.divider_at(Point::new(100.0, 50.0)),
            None,
            "not in the header"
        );

        t.begin_resize(0, Point::new(100.0, 5.0));
        assert!(t.resizing());
        assert!(t.resize_to(Point::new(130.0, 5.0)));
        assert!(
            !t.resize_to(Point::new(130.0, 9.0)),
            "same width: no change"
        );
        t.end_resize();
        assert!((t.columns[0].width - 130.0).abs() < f32::EPSILON);
        assert!(!t.resize_to(Point::new(0.0, 0.0)), "no drag: no change");
        t.begin_resize(0, Point::new(130.0, 5.0));
        t.resize_to(Point::new(-500.0, 5.0));
        t.end_resize();
        assert!((t.columns[0].width - MIN_COLUMN_W).abs() < f32::EPSILON);

        // Columns total 140 in a 150-wide table: nothing to scroll.
        t.scroll_x_by(50.0);
        t.paint(&mut dl, rect, &src, &theme, &mut buf);
        assert!(t.scroll_x.abs() < f32::EPSILON);
        // Widen: 240 total, 90 of overflow. Scroll clamps to it, and hits shift.
        t.columns[1].width = 200.0;
        t.scroll_x_by(500.0);
        t.paint(&mut dl, rect, &src, &theme, &mut buf);
        assert!((t.scroll_x - 90.0).abs() < f32::EPSILON);
        assert_eq!(t.hit(Point::new(5.0, 5.0), &theme), Hit::Header(1));
        // Column 1's right edge now sits at the table's right edge (exclusive).
        assert_eq!(t.divider_at(Point::new(149.0, 5.0)), Some(1));
    }

    #[test]
    fn columns_hide_reorder_and_round_trip_through_a_layout() {
        let src = Nums((0..5).collect());
        let mut t = Table::new(
            vec![
                Column::number("a", 100.0),
                Column::number("b", 100.0).hidden(),
                Column::text("c", 100.0),
            ],
            0,
        );
        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        let rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        t.paint(&mut dl, rect, &src, &theme, &mut buf);
        // b is hidden: a then c, 200 wide, and the header hit skips b.
        assert_eq!(t.shown().map(|(i, _)| i).collect::<Vec<_>>(), [0, 2]);
        assert!((t.total_width() - 200.0).abs() < f32::EPSILON);
        assert_eq!(t.hit(Point::new(150.0, 5.0), &theme), Hit::Header(2));

        // Show b: it goes to the right end. Hide the last visible one: refused.
        assert!(t.set_visible(1, true));
        assert!(!t.set_visible(1, true), "already shown");
        assert_eq!(t.shown().map(|(i, _)| i).collect::<Vec<_>>(), [0, 2, 1]);
        assert!(t.set_visible(0, false));
        assert!(t.set_visible(2, false));
        assert!(!t.set_visible(1, false), "the last column stays");
        assert!(t.set_visible(0, true));
        assert!(t.set_visible(2, true));
        assert_eq!(t.shown().map(|(i, _)| i).collect::<Vec<_>>(), [1, 0, 2]);

        // Drag a (second slot, x 100..200) to the far left: a click would have
        // sorted; a drag reorders and reports no click.
        t.paint(&mut dl, rect, &src, &theme, &mut buf);
        t.begin_header_press(0, Point::new(150.0, 5.0));
        assert!(!t.header_drag_to(Point::new(152.0, 5.0)), "within the slop");
        assert!(!t.header_dragging());
        assert!(t.header_drag_to(Point::new(20.0, 5.0)));
        assert!(t.header_dragging());
        t.paint(&mut dl, rect, &src, &theme, &mut buf);
        assert_eq!(t.end_header_press(Point::new(20.0, 5.0)), None);
        assert_eq!(t.shown().map(|(i, _)| i).collect::<Vec<_>>(), [0, 1, 2]);
        // Drag a to the far right.
        t.begin_header_press(0, Point::new(50.0, 5.0));
        t.header_drag_to(Point::new(390.0, 5.0));
        assert_eq!(t.end_header_press(Point::new(390.0, 5.0)), None);
        assert_eq!(t.shown().map(|(i, _)| i).collect::<Vec<_>>(), [1, 2, 0]);
        // A press released in place is a click.
        t.begin_header_press(2, Point::new(150.0, 5.0));
        assert_eq!(t.end_header_press(Point::new(151.0, 5.0)), Some(2));

        // The layout lists visible columns in order, then hidden ones, and a
        // fresh table restored from it matches; an unknown title is ignored and
        // a column it does not name keeps its default place after the named ones.
        t.columns[2].width = 123.0;
        t.set_visible(1, false);
        let layout = t.layout();
        assert_eq!(
            layout
                .iter()
                .map(|l| (l.title.as_str(), l.visible))
                .collect::<Vec<_>>(),
            [("c", true), ("a", true), ("b", false)]
        );
        let mut fresh = Table::new(
            vec![
                Column::number("a", 100.0),
                Column::number("b", 100.0),
                Column::text("c", 100.0),
                Column::text("d", 100.0),
            ],
            0,
        );
        let mut saved = layout.clone();
        saved.push(ColumnLayout {
            title: "gone".to_owned(),
            width: 1.0,
            visible: true,
        });
        fresh.apply_layout(&saved);
        assert_eq!(fresh.shown().map(|(i, _)| i).collect::<Vec<_>>(), [2, 0, 3]);
        assert!((fresh.columns[2].width - 123.0).abs() < f32::EPSILON);
        assert!(!fresh.columns[1].visible);
        // A layout hiding everything is ignored.
        let all_hidden: Vec<ColumnLayout> = ["a", "b", "c", "d"]
            .iter()
            .map(|t| ColumnLayout {
                title: (*t).to_owned(),
                width: 50.0,
                visible: false,
            })
            .collect();
        fresh.apply_layout(&all_hidden);
        assert_eq!(fresh.shown().count(), 4);
        // Reset puts the defaults back and keeps the sort by title.
        fresh.sort_col = 2;
        fresh.reset_columns(vec![Column::text("c", 10.0), Column::number("a", 20.0)]);
        assert_eq!(fresh.sort_col, 0);
        assert_eq!(fresh.shown().count(), 2);
    }

    #[test]
    fn selected_rect_follows_scroll_and_hides_off_screen() {
        let src = Nums((0..50).collect());
        let mut t = table();
        let theme = Theme::dark();
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        let rect = Rect::new(0.0, 0.0, 100.0, theme.header_h + theme.row_h * 5.0);
        t.paint(&mut dl, rect, &src, &theme, &mut buf);
        assert_eq!(t.selected_rect(&src, &theme), None);
        t.selected = Some(src.id(49)); // value 49 sorts first (descending)
        let r = t.selected_rect(&src, &theme).expect("in view");
        assert!((r.y - theme.header_h).abs() < f32::EPSILON);
        t.selected = Some(src.id(0)); // last row, well below the five visible
        assert_eq!(t.selected_rect(&src, &theme), None);
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

    /// Rows whose values change under stable ids (the row index).
    struct Live(Vec<u32>);

    impl RowSource for Live {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn id(&self, row: usize) -> RowId {
            RowId(row as u64)
        }
        fn cell(&self, row: usize, _col: usize, out: &mut String) {
            out.clear();
            out.push_str(&self.0[row].to_string());
        }
        fn compare(&self, a: usize, b: usize, _col: usize) -> Ordering {
            self.0[a].cmp(&self.0[b])
        }
    }

    /// Ten rows high; descending, so row `r` of `0..50` starts at position 49 - r.
    fn live_view(t: &mut Table, src: &Live, theme: &Theme) -> DisplayList {
        let mut dl = DisplayList::new();
        let rect = Rect::new(0.0, 0.0, 100.0, theme.header_h + theme.row_h * 10.0);
        t.paint(&mut dl, rect, src, theme, &mut String::new());
        dl
    }

    /// Where the text `s` was painted, as a screen row of the body.
    fn row_of_text(dl: &DisplayList, s: &str, theme: &Theme) -> Option<f32> {
        dl.cmds().iter().find_map(|c| match c {
            ot_paint::DrawCmd::Text(t) if dl.str(t.text) == s => {
                Some((t.rect.y - theme.header_h) / theme.row_h)
            }
            _ => None,
        })
    }

    #[test]
    fn a_refresh_keeps_the_selection_where_it_is_on_screen() {
        let theme = Theme::dark();
        let mut src = Live((0..50).collect());
        let mut t = table();
        live_view(&mut t, &src, &theme);
        t.selected = Some(RowId(45)); // position 4
                                      // It drops to 20: behind the 28 larger values and row 20 (same value,
                                      // lower id), so position 29.
        src.0[45] = 20;
        t.refresh();
        live_view(&mut t, &src, &theme);
        let r = t.selected_rect(&src, &theme).expect("still on screen");
        assert!((r.y - (theme.header_h + 4.0 * theme.row_h)).abs() < 1e-3);
        assert!(
            (t.scroll - 25.0).abs() < 1e-3,
            "the rest scrolled around it"
        );

        // A re-sort the user asked for does not chase it.
        t.set_sort(0);
        live_view(&mut t, &src, &theme);
        assert!((t.scroll - 25.0).abs() < 1e-3);
        // Off screen, a refresh leaves the scroll alone too.
        t.scroll = 0.0;
        t.set_sort(0);
        live_view(&mut t, &src, &theme);
        src.0[45] = 21;
        t.refresh();
        live_view(&mut t, &src, &theme);
        assert!(t.scroll.abs() < 1e-3);
    }

    #[test]
    fn with_animation_rows_slide_to_their_new_places() {
        let theme = Theme::dark();
        let mut src = Live((0..50).collect());
        let mut t = table();
        t.set_animate(true);
        let t0 = Instant::now();
        t.tick(t0);
        live_view(&mut t, &src, &theme);
        assert!(!t.animating());

        // Row 40 (position 9, the last on screen) jumps to the top.
        src.0[40] = 100;
        t.refresh();
        let dl = live_view(&mut t, &src, &theme);
        assert!(t.animating());
        let at = |dl: &DisplayList, s| row_of_text(dl, s, &theme).unwrap();
        assert!((at(&dl, "100") - 9.0).abs() < 1e-3, "starts where it was");
        assert!((at(&dl, "49") - 0.0).abs() < 1e-3, "and the top row too");

        t.tick(t0 + SLIDE / 2);
        let dl = live_view(&mut t, &src, &theme);
        let mid = at(&dl, "100");
        let backing = |dl: &DisplayList| {
            dl.cmds()
                .iter()
                .filter_map(|c| match *c {
                    ot_paint::DrawCmd::FillRect { rect, color } if color == theme.bg_solid => {
                        Some((rect.y - theme.header_h) / theme.row_h)
                    }
                    _ => None,
                })
                .collect::<Vec<f32>>()
        };
        assert!(
            backing(&dl).iter().any(|&y| (y - mid).abs() < 1e-3),
            "the moving row is backed so rows it crosses do not show through"
        );
        assert!(
            mid > 0.0 && mid < 9.0 * 0.5,
            "past halfway (ease-out): {mid}"
        );
        let one_down = at(&dl, "49");
        assert!(one_down > 0.5 && one_down < 1.0, "{one_down}");

        t.tick(t0 + SLIDE);
        let dl = live_view(&mut t, &src, &theme);
        assert!(at(&dl, "100").abs() < 1e-3, "landed");
        assert!(backing(&dl).is_empty(), "rows at rest are not lifted");
        assert!((at(&dl, "49") - 1.0).abs() < 1e-3);
        assert!(!t.animating(), "done");

        // Without animation nothing is in motion.
        t.set_animate(false);
        src.0[40] = 0;
        t.refresh();
        live_view(&mut t, &src, &theme);
        assert!(!t.animating());
    }

    #[test]
    fn a_row_leaving_the_view_slides_out_over_the_edge() {
        let theme = Theme::dark();
        let mut src = Live((0..50).collect());
        let mut t = table();
        t.set_animate(true);
        let t0 = Instant::now();
        t.tick(t0);
        live_view(&mut t, &src, &theme);
        // Row 49 (position 0) sinks to the bottom of the list, far below the view.
        src.0[49] = 0;
        t.refresh();
        let dl = live_view(&mut t, &src, &theme);
        assert_eq!(t.slide.leaving.len(), 1);
        assert!(
            row_of_text(&dl, "0", &theme).is_some_and(|y| y.abs() < 1e-3),
            "drawn where it was"
        );
        t.tick(t0 + SLIDE);
        let dl = live_view(&mut t, &src, &theme);
        assert!(!t.animating());
        // At rest it is back to being an ordinary off-screen row.
        assert!(row_of_text(&dl, "0", &theme).is_none());
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
