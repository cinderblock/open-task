//! The Map: the process table's area drawn as a treemap of who has been using the
//! CPU, the third arrangement beside List and Tree.
//!
//! Each process is a tile whose area is the CPU time it used over the last minute
//! ([`ot_core::Usage`]), so the map answers "who has been busy", which the table's
//! one-second CPU column cannot. Tiles nest by the process tree: a process with
//! children is a frame with its name on a header strip, its children inside, and one
//! more tile for the time it used itself. Color says whether a tile is still at it:
//! the table's heat orange, as strong as the process's CPU in the last interval. A
//! process that exited inside the window stays, dimmed, until the window passes it.
//!
//! Layout is squarified ([`crate::treemap`]). Siblings are laid out largest first,
//! but by sticky keys ([`Steady`]), so two siblings of about the same size do not
//! trade places every second. Tiles too small to see are not drawn, and labels go
//! only where they fit.
//!
//! The same accounting also draws the icicle strip above the table in List and
//! Tree: two rows, the top-level processes across the first, what runs under each
//! beneath it, every segment as wide as its share of the CPU used over the minute.
//! It is the Map folded flat, always in view; the strip and the Map are never shown
//! together, so they share this one struct.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use ot_core::{Snapshot, Usage};
use ot_model::process::ProcessStatic;
use ot_model::ProcessKey;
use ot_paint::{DisplayList, HAlign, Point, Rect, VAlign};

use crate::process_rows::{process_matches, row_id, statics_match};
use crate::steady::{Band, Steady};
use crate::table::RowId;
use crate::theme::Theme;
use crate::treemap::squarify;

/// The strip along the bottom that describes the map or the tile under the pointer.
const CAPTION_H: f32 = 22.0;
/// A frame's name strip.
const HEADER_H: f32 = 18.0;
/// A process with children gets a frame only if its tile is at least this big;
/// smaller, it is one tile for its whole subtree.
const FRAME_MIN: (f32, f32) = (64.0, 48.0);
/// Space between a frame's edge and its contents.
const FRAME_PAD: f32 = 3.0;
/// Smallest tile drawn.
const TILE_MIN: f32 = 2.0;
/// A tile gets its name from this size, and a second line (its average CPU) from
/// this height.
const LABEL_MIN: (f32, f32) = (34.0, 15.0);
const TWO_LINES_H: f32 = 32.0;
/// A frame's share of the header for its percentage, right-aligned.
const HEADER_VALUE_W: f32 = 44.0;
/// A process with one child and less than this share of its subtree's time to
/// itself is folded into its child's frame: `a.exe \u{203a} b.exe`. Chains of
/// launchers and wrappers (a shell, a runtime, an app) otherwise nest frame in frame.
const CHAIN_OWN: f64 = 0.02;
/// The icicle strip: its height, rows, and the gap between them.
pub(crate) const STRIP_H: f32 = 30.0;
const STRIP_ROWS: usize = 2;
const STRIP_ROW_GAP: f32 = 1.0;
/// A strip segment gets its name from this width.
const STRIP_LABEL_W: f32 = 48.0;
/// Siblings reorder only when one's time changes by more than this: 50 ms or 10 %.
const ORDER_BAND: Band = Band {
    abs: 0.05,
    rel: 0.10,
};

#[derive(Debug, Clone)]
struct Node {
    statics: Arc<ProcessStatic>,
    parent: Option<usize>,
    /// CPU seconds this process used itself, and with everything under it.
    own: f64,
    total: f64,
    alive: bool,
    /// CPU in the last interval, as a share of one core; zero once exited.
    cpu_now: f32,
    /// Not a search match, and not above one.
    dim: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TileKind {
    /// A process drawn as one block: no children, or too small for a frame.
    Leaf,
    /// A process with children: name strip, contents inside.
    Frame,
    /// Inside a frame: the time the framed process used itself.
    Own,
}

#[derive(Debug, Clone, Copy)]
struct Tile {
    rect: Rect,
    /// The process the tile is for. For a folded chain, the last of it.
    node: usize,
    /// The first of a folded chain; `node` otherwise.
    top: usize,
    kind: TileKind,
}

/// What a container lays out: a child process, or the parent's own time.
#[derive(Debug, Clone, Copy)]
enum Item {
    Child(usize),
    Own(usize),
}

#[derive(Debug, Default)]
pub(crate) struct UsageMap {
    nodes: Vec<Node>,
    index: HashMap<ProcessKey, usize>,
    /// Children of each node in CSR form: `child_list[child_start[i]..child_start[i + 1]]`.
    child_start: Vec<usize>,
    child_list: Vec<usize>,
    roots: Vec<usize>,
    /// Scratch for [`UsageMap::build`] and [`UsageMap::layout`], kept so steady-state
    /// painting does not allocate.
    live: HashMap<ProcessKey, usize>,
    fill: Vec<usize>,
    seen: Vec<bool>,
    preorder: Vec<usize>,
    stack: Vec<usize>,
    queue: Vec<(Option<usize>, Rect)>,
    /// The strip's work list: (first and last of a folded chain, x, width, row).
    strip_queue: Vec<(usize, usize, f32, f32, usize)>,
    items: Vec<Item>,
    values: Vec<f64>,
    rects: Vec<Rect>,
    /// Painted this frame, parents before children, for drawing and hit testing.
    tiles: Vec<Tile>,
    order: Steady,
    hover: Option<ProcessKey>,
    /// Seconds the window covers, and CPU seconds everything used in it.
    span: f64,
    used: f64,
    logical_processors: usize,
}

impl UsageMap {
    /// Rebuild the process tree from the usage accounting: every process that used
    /// CPU in the window, alive or exited.
    fn build(&mut self, usage: &Usage, snap: &Snapshot, needle: &str, buf: &mut String) {
        self.nodes.clear();
        self.index.clear();
        self.live.clear();
        self.live
            .extend(snap.processes.iter().enumerate().map(|(i, p)| (p.key(), i)));
        for (key, u) in usage.iter() {
            // PID 0 is the kernel's idle accounting, not a process.
            if key.pid == 0 {
                continue;
            }
            let sample = self.live.get(&key).map(|&i| &snap.processes[i]);
            let matched = match sample {
                Some(p) => process_matches(p, needle, buf),
                None => statics_match(u.statics, &[], needle, buf),
            };
            self.index.insert(key, self.nodes.len());
            self.nodes.push(Node {
                statics: Arc::clone(u.statics),
                parent: None,
                own: u.used.as_secs_f64(),
                total: 0.0,
                alive: u.alive,
                cpu_now: sample.map_or(0.0, |p| p.cpu.get()),
                dim: !matched,
            });
        }
        for i in 0..self.nodes.len() {
            let parent = self.nodes[i]
                .statics
                .parent
                .and_then(|k| self.index.get(&k).copied())
                .filter(|&p| p != i);
            self.nodes[i].parent = parent;
        }

        // Children in CSR form, then a walk from the roots: totals bottom up, a
        // match lighting up its ancestors, and anything a malformed parent cycle
        // left unreached made a root.
        let n = self.nodes.len();
        self.child_start.clear();
        self.child_start.resize(n + 1, 0);
        for node in &self.nodes {
            if let Some(p) = node.parent {
                self.child_start[p + 1] += 1;
            }
        }
        for i in 1..=n {
            self.child_start[i] += self.child_start[i - 1];
        }
        self.fill.clear();
        self.fill.extend_from_slice(&self.child_start);
        self.child_list.clear();
        self.child_list.resize(self.child_start[n], 0);
        self.roots.clear();
        for (i, node) in self.nodes.iter().enumerate() {
            match node.parent {
                Some(p) => {
                    self.child_list[self.fill[p]] = i;
                    self.fill[p] += 1;
                }
                None => self.roots.push(i),
            }
        }
        self.seen.clear();
        self.seen.resize(n, false);
        self.preorder.clear();
        self.stack.clear();
        self.stack.extend_from_slice(&self.roots);
        while let Some(i) = self.stack.pop() {
            if std::mem::replace(&mut self.seen[i], true) {
                continue;
            }
            self.preorder.push(i);
            self.stack
                .extend(&self.child_list[self.child_start[i]..self.child_start[i + 1]]);
        }
        for i in 0..n {
            if !self.seen[i] {
                self.nodes[i].parent = None;
                self.roots.push(i);
                self.preorder.push(i);
            }
        }
        for k in (0..self.preorder.len()).rev() {
            let i = self.preorder[k];
            let node = &mut self.nodes[i];
            node.total += node.own;
            let (total, lit, parent) = (node.total, !node.dim, node.parent);
            if let Some(p) = parent {
                self.nodes[p].total += total;
                if lit {
                    self.nodes[p].dim = false;
                }
            }
        }

        self.order.update(
            0,
            true,
            Some(ORDER_BAND),
            self.nodes.iter().map(|n| (row_id(n.statics.key), n.total)),
        );
        self.span = usage.span().as_secs_f64();
        self.used = self.roots.iter().map(|&r| self.nodes[r].total).sum();
        self.logical_processors = usage.logical_processors();
    }

    fn children(&self, i: usize) -> &[usize] {
        &self.child_list[self.child_start[i]..self.child_start[i + 1]]
    }

    fn sort_key(&self, i: usize) -> f64 {
        let n = &self.nodes[i];
        self.order.key(row_id(n.statics.key), 0, true, n.total)
    }

    /// Place every tile inside `area`.
    fn layout(&mut self, area: Rect) {
        self.tiles.clear();
        self.queue.clear();
        self.queue.push((None, area));
        let mut items = std::mem::take(&mut self.items);
        let mut rects = std::mem::take(&mut self.rects);
        while let Some((container, rect)) = self.queue.pop() {
            items.clear();
            match container {
                None => items.extend(self.roots.iter().map(|&r| Item::Child(r))),
                Some(i) => items.extend(self.children(i).iter().map(|&c| Item::Child(c))),
            }
            // Largest first, by sticky key; the parent's own time goes last so it
            // keeps its corner of the frame.
            self.sort_items(&mut items);
            if let Some(i) = container {
                items.push(Item::Own(i));
            }
            self.values.clear();
            self.values.extend(items.iter().map(|it| match *it {
                Item::Child(c) => self.nodes[c].total,
                Item::Own(i) => self.nodes[i].own,
            }));
            squarify(&self.values, rect, &mut rects);
            for (item, &r) in items.iter().zip(&rects) {
                if r.w < TILE_MIN || r.h < TILE_MIN {
                    continue;
                }
                let (top, node, kind) = match *item {
                    Item::Own(i) => (i, i, TileKind::Own),
                    Item::Child(c) if r.w < FRAME_MIN.0 || r.h < FRAME_MIN.1 => {
                        (c, c, TileKind::Leaf)
                    }
                    Item::Child(c) => {
                        let end = self.chain_end(c);
                        if self.busy_children(end).next().is_none() {
                            (c, end, TileKind::Leaf)
                        } else {
                            let (_, body) = r.split_top(HEADER_H);
                            self.queue
                                .push((Some(end), body.inset(FRAME_PAD, FRAME_PAD)));
                            (c, end, TileKind::Frame)
                        }
                    }
                };
                self.tiles.push(Tile {
                    rect: r,
                    node,
                    top,
                    kind,
                });
            }
        }
        items.clear();
        self.items = items;
        self.rects = rects;
    }

    /// Lay the icicle out across `area`: the top-level processes (chains folded)
    /// along the first row, as wide as their share of the CPU used; the busy
    /// children of each beneath it, within its span. A parent's own time is the
    /// part of its span its children leave empty.
    fn layout_strip(&mut self, area: Rect) {
        self.tiles.clear();
        self.strip_queue.clear();
        if self.used <= 0.0 || area.is_empty() {
            return;
        }
        let rows = STRIP_ROWS as f32;
        let row_h = (area.h - STRIP_ROW_GAP * (rows - 1.0)) / rows;
        let mut items = std::mem::take(&mut self.items);
        items.clear();
        items.extend(self.roots.iter().map(|&r| Item::Child(r)));
        self.sort_items(&mut items);
        let mut x = area.x;
        for it in &items {
            if let Item::Child(c) = *it {
                let w = area.w * (self.nodes[c].total / self.used) as f32;
                self.strip_queue.push((c, self.chain_end(c), x, w, 0));
                x += w;
            }
        }
        while let Some((top, end, x, w, row)) = self.strip_queue.pop() {
            if w < 1.5 {
                continue;
            }
            let y = area.y + row as f32 * (row_h + STRIP_ROW_GAP);
            self.tiles.push(Tile {
                rect: Rect::new(x, y, w, row_h),
                node: end,
                top,
                kind: TileKind::Leaf,
            });
            if row + 1 >= STRIP_ROWS {
                continue;
            }
            let span = self.nodes[top].total;
            items.clear();
            items.extend(self.busy_children(end).map(Item::Child));
            self.sort_items(&mut items);
            let mut cx = x;
            for it in &items {
                if let Item::Child(c) = *it {
                    let cw = w * (self.nodes[c].total / span) as f32;
                    self.strip_queue
                        .push((c, self.chain_end(c), cx, cw, row + 1));
                    cx += cw;
                }
            }
        }
        items.clear();
        self.items = items;
    }

    /// Children largest first, by sticky key, ties by PID.
    fn sort_items(&self, items: &mut [Item]) {
        items.sort_by(|a, b| match (*a, *b) {
            (Item::Child(x), Item::Child(y)) => {
                self.sort_key(y).total_cmp(&self.sort_key(x)).then_with(|| {
                    let pid = |i: usize| self.nodes[i].statics.key.pid;
                    pid(x).cmp(&pid(y))
                })
            }
            _ => std::cmp::Ordering::Equal,
        });
    }

    /// Draw the icicle strip in `rect`.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_strip(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        usage: &Usage,
        snap: &Snapshot,
        needle: &str,
        selected: Option<RowId>,
        theme: &Theme,
        buf: &mut String,
    ) {
        self.build(usage, snap, needle, buf);
        self.layout_strip(rect);
        dl.fill_round_rect(rect, 2.0, theme.surface);
        dl.push_clip(rect);
        for t in &self.tiles {
            let n = &self.nodes[t.node];
            let body = Rect::new(t.rect.x, t.rect.y, (t.rect.w - 1.0).max(0.5), t.rect.h);
            let muted = n.dim || !n.alive;
            dl.fill_rect(body, theme.surface);
            if !muted {
                let heat = (n.cpu_now / 100.0).clamp(0.0, 1.0);
                dl.fill_rect(body, theme.heat.with_alpha(0.10 + 0.55 * heat));
            }
            if self.hover == Some(n.statics.key) {
                dl.fill_rect(body, theme.button_hover);
            }
            if body.w >= STRIP_LABEL_W {
                self.chain_name(t.top, t.node, buf);
                let ink = if muted { theme.text_dim } else { theme.text };
                dl.label(buf, body.inset(4.0, 0.0), theme.small, ink);
            }
            let top_key = self.nodes[t.top].statics.key;
            if selected == Some(row_id(n.statics.key)) || selected == Some(row_id(top_key)) {
                dl.stroke_rect(body, theme.accent, 1.5);
            }
        }
        dl.pop_clip();
    }

    /// What the pointer is over, for a status line: the process, its average CPU
    /// and seconds used over the window. False, and `out` empty, when nothing is.
    pub fn describe_hover(&self, out: &mut String) -> bool {
        out.clear();
        let Some(&i) = self.hover.and_then(|k| self.index.get(&k)) else {
            return false;
        };
        let n = &self.nodes[i];
        let _ = write!(
            out,
            "{} \u{b7} PID {} \u{b7} ",
            n.statics.name, n.statics.key.pid
        );
        push_percent(out, self.average(n.total));
        out.push_str("% of a core on average over ");
        self.window(out);
        let _ = write!(out, ", {:.1} s of CPU", n.total);
        if !self.children(i).is_empty() {
            out.push_str(" with everything under it");
        }
        if !n.alive {
            out.push_str(" \u{b7} exited");
        }
        true
    }

    /// Follow a chain of single children down from `i` while each link has next to
    /// no time of its own: the process whose frame (or tile) the chain folds into.
    fn chain_end(&self, i: usize) -> usize {
        let mut at = i;
        loop {
            let n = &self.nodes[at];
            let mut busy = self.busy_children(at);
            match (busy.next(), busy.next()) {
                (Some(only), None) if n.own <= CHAIN_OWN * n.total => at = only,
                _ => return at,
            }
        }
    }

    /// The children of `i` that took a real share of its subtree's time. Only these
    /// make it a frame or a link in a chain: a parent whose children barely ran is
    /// better drawn as one tile than as a frame around its own time.
    fn busy_children(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        let floor = CHAIN_OWN * self.nodes[i].total;
        self.children(i)
            .iter()
            .copied()
            .filter(move |&c| self.nodes[c].total > floor)
    }

    /// `top.exe \u{203a} ... \u{203a} end.exe` for a folded chain, the name alone
    /// otherwise.
    fn chain_name(&self, top: usize, end: usize, out: &mut String) {
        out.clear();
        let mut at = end;
        let mut first = true;
        // Walk up from the end, then the pieces are in reverse; a chain is short.
        let mut names: [usize; 16] = [0; 16];
        let mut len = 0;
        loop {
            if len < names.len() {
                names[len] = at;
                len += 1;
            }
            if at == top {
                break;
            }
            match self.nodes[at].parent {
                Some(p) => at = p,
                None => break,
            }
        }
        for &i in names[..len].iter().rev() {
            if !first {
                out.push_str(" \u{203a} ");
            }
            first = false;
            out.push_str(&self.nodes[i].statics.name);
        }
    }

    /// The process whose tile is under `p` (the innermost one), and whether it is
    /// still running.
    #[must_use]
    pub fn key_at(&self, p: Point) -> Option<(ProcessKey, bool)> {
        self.tiles
            .iter()
            .rev()
            .find(|t| t.rect.contains(p))
            .map(|t| {
                let n = &self.nodes[t.node];
                (n.statics.key, n.alive)
            })
    }

    /// Where the process with table id `id` was drawn last, if it was.
    #[must_use]
    pub fn tile_rect(&self, id: RowId) -> Option<Rect> {
        self.tiles
            .iter()
            .rev()
            .find(|t| {
                row_id(self.nodes[t.node].statics.key) == id
                    || row_id(self.nodes[t.top].statics.key) == id
            })
            .map(|t| t.rect)
    }

    /// Follow the pointer. Returns whether the highlighted tile changed.
    pub fn set_hover(&mut self, p: Option<Point>) -> bool {
        let key = p.and_then(|p| self.key_at(p)).map(|(k, _)| k);
        std::mem::replace(&mut self.hover, key) != key
    }

    /// Average CPU over the window, as a share of one core (100 is one core busy
    /// throughout), for `seconds` used.
    fn average(&self, seconds: f64) -> f32 {
        if self.span > 0.0 {
            (seconds / self.span * 100.0) as f32
        } else {
            0.0
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        usage: &Usage,
        snap: &Snapshot,
        needle: &str,
        selected: Option<RowId>,
        theme: &Theme,
        buf: &mut String,
    ) {
        self.build(usage, snap, needle, buf);
        let (caption, area) = rect.split_bottom(CAPTION_H);
        self.layout(area);

        dl.push_clip(area);
        for t in &self.tiles {
            self.paint_tile(dl, t, selected, theme, buf);
        }
        dl.pop_clip();
        if self.tiles.is_empty() {
            dl.text(
                "Nothing has used the CPU yet",
                area,
                theme.cell,
                theme.text_dim,
                HAlign::Center,
                VAlign::Middle,
                true,
            );
        }
        self.caption(buf);
        dl.label(
            buf,
            caption.inset(theme.pad, 0.0),
            theme.small,
            theme.text_dim,
        );
    }

    fn paint_tile(
        &self,
        dl: &mut DisplayList,
        t: &Tile,
        selected: Option<RowId>,
        theme: &Theme,
        buf: &mut String,
    ) {
        let n = &self.nodes[t.node];
        let body = t.rect.inset(1.0, 1.0);
        let muted = n.dim || !n.alive;
        let ink = if muted { theme.text_dim } else { theme.text };
        dl.fill_rect(body, theme.surface);
        match t.kind {
            TileKind::Frame => {
                let (head, _) = body.split_top(HEADER_H - 1.0);
                dl.fill_rect(head, theme.surface);
                let top = &self.nodes[t.top];
                let (names, value) = head
                    .inset(4.0, 0.0)
                    .split_left((head.w - 8.0 - HEADER_VALUE_W).max(0.0));
                self.chain_name(t.top, t.node, buf);
                dl.label(buf, names, theme.small, ink);
                buf.clear();
                push_percent(buf, self.average(top.total));
                buf.push('%');
                dl.text(
                    buf,
                    value,
                    theme.small,
                    theme.text_dim,
                    HAlign::Right,
                    VAlign::Middle,
                    false,
                );
            }
            TileKind::Leaf | TileKind::Own => {
                if !muted {
                    let heat = (n.cpu_now / 100.0).clamp(0.0, 1.0);
                    dl.fill_rect(body, theme.heat.with_alpha(0.06 + 0.55 * heat));
                }
                if self.hover == Some(n.statics.key) {
                    dl.fill_rect(body, theme.button_hover);
                }
                if body.w >= LABEL_MIN.0 && body.h >= LABEL_MIN.1 {
                    let seconds = if t.kind == TileKind::Own {
                        n.own
                    } else {
                        n.total
                    };
                    let (line1, line2) = body.inset(4.0, 2.0).split_top(14.0);
                    self.chain_name(t.top, t.node, buf);
                    dl.label(buf, line1, theme.small, ink);
                    if body.h >= TWO_LINES_H {
                        buf.clear();
                        push_percent(buf, self.average(seconds));
                        buf.push('%');
                        if !n.alive {
                            buf.push_str("  exited");
                        }
                        dl.text(
                            buf,
                            line2,
                            theme.small,
                            theme.text_dim,
                            HAlign::Left,
                            VAlign::Top,
                            true,
                        );
                    }
                }
            }
        }
        let top_key = self.nodes[t.top].statics.key;
        if selected == Some(row_id(n.statics.key)) || selected == Some(row_id(top_key)) {
            dl.stroke_rect(body, theme.accent, 2.0);
        }
    }

    /// The line under the map: the tile under the pointer, or what the map shows.
    fn caption(&self, out: &mut String) {
        if self.describe_hover(out) {
            return;
        }
        let lp = self.logical_processors.max(1);
        out.push_str("CPU used over ");
        self.window(out);
        out.push_str(": ");
        push_percent(out, self.average(self.used) / lp as f32);
        let _ = write!(
            out,
            "% of the machine ({:.1} s over {lp} logical processors). \
             Area is CPU time, color is CPU now.",
            self.used
        );
    }

    /// "the last minute", or while the session is younger, "the last 23 s".
    fn window(&self, out: &mut String) {
        if self.span >= 59.5 {
            out.push_str("the last minute");
        } else {
            let _ = write!(out, "the last {:.0} s", self.span);
        }
    }
}

/// Append a percentage the way [`crate::format::percent`] writes one: a decimal
/// below ten, none above.
fn push_percent(out: &mut String, p: f32) {
    let _ = if p < 10.0 {
        write!(out, "{p:.1}")
    } else {
        write!(out, "{p:.0}")
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_rows::tests::proc;
    use ot_model::Tick;
    use ot_paint::DrawCmd;
    use std::time::{Duration, SystemTime};

    /// Two snapshots a minute apart; `cpu` gives each pid's CPU seconds used in
    /// between. 1 is the parent of 2 and 3; 4 stands alone.
    fn usage_of(cpu: &[(u32, u64)]) -> (Usage, Snapshot) {
        let mut u = Usage::new(Duration::from_secs(60));
        let tree = [(1, None), (2, Some(1)), (3, Some(1)), (4, None)];
        let snap_at = |secs: u64, used: bool| {
            let mut procs = Vec::new();
            for &(pid, parent) in &tree {
                let mut p = proc(pid, parent, 5.0);
                let s = cpu.iter().find(|c| c.0 == pid).map_or(0, |c| c.1);
                p.cpu_time = Duration::from_secs(if used { 100 + s } else { 100 });
                procs.push(p);
            }
            Snapshot {
                tick: Tick(secs),
                taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
                interval: Duration::from_secs(1),
                processes: procs,
                ..Default::default()
            }
        };
        u.observe(&snap_at(1000, false));
        let last = snap_at(1060, true);
        u.observe(&last);
        (u, last)
    }

    fn texts(dl: &DisplayList) -> Vec<String> {
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect()
    }

    fn area(r: Rect) -> f32 {
        r.w * r.h
    }

    const RECT: Rect = Rect::new(0.0, 0.0, 600.0, 422.0);

    fn painted(m: &mut UsageMap, u: &Usage, snap: &Snapshot, needle: &str) -> DisplayList {
        let mut dl = DisplayList::new();
        m.paint(
            &mut dl,
            RECT,
            u,
            snap,
            needle,
            None,
            &Theme::dark(),
            &mut String::new(),
        );
        assert_eq!(dl.clip_depth(), 0);
        dl
    }

    fn tile(m: &UsageMap, pid: u32, kind: TileKind) -> Option<Rect> {
        m.tiles
            .iter()
            .find(|t| m.nodes[t.node].statics.key.pid == pid && t.kind == kind)
            .map(|t| t.rect)
    }

    #[test]
    fn tiles_are_sized_by_cpu_time_and_nest_by_the_tree() {
        let (u, snap) = usage_of(&[(1, 6), (2, 30), (3, 12), (4, 12)]);
        let mut m = UsageMap::default();
        let dl = painted(&mut m, &u, &snap, "");
        // p1 frames its children and its own 6 s; p4 is a leaf.
        let frame = tile(&m, 1, TileKind::Frame).expect("p1 is a frame");
        let four = tile(&m, 4, TileKind::Leaf).expect("p4 is a leaf");
        // 48 s under p1 against 12 s for p4.
        let ratio = area(frame) / area(four);
        assert!((ratio - 4.0).abs() < 0.05, "{frame:?} {four:?}");
        let two = tile(&m, 2, TileKind::Leaf).unwrap();
        let own = tile(&m, 1, TileKind::Own).unwrap();
        assert!(frame.contains(two.center()) && frame.contains(own.center()));
        assert!(
            (area(two) / area(own) - 5.0).abs() < 0.1,
            "30 s against 6 s"
        );
        let strings = texts(&dl);
        assert!(strings.iter().any(|s| s == "p1.exe"), "{strings:?}");
        assert!(strings.iter().any(|s| s == "80%"), "{strings:?}");
        assert!(strings.iter().any(|s| s == "p2.exe"));
        assert!(
            strings
                .iter()
                .any(|s| s.starts_with("CPU used over the last minute: ")),
            "{strings:?}"
        );
    }

    #[test]
    fn hover_and_hit_find_the_innermost_tile() {
        let (u, snap) = usage_of(&[(1, 6), (2, 30), (3, 12), (4, 12)]);
        let mut m = UsageMap::default();
        painted(&mut m, &u, &snap, "");
        let two = tile(&m, 2, TileKind::Leaf).unwrap();
        assert_eq!(m.key_at(two.center()), Some((ProcessKey::new(2, 1), true)));
        assert!(m.set_hover(Some(two.center())));
        assert!(!m.set_hover(Some(two.center())), "same tile");
        let dl = painted(&mut m, &u, &snap, "");
        assert!(
            texts(&dl)
                .iter()
                .any(|s| s.starts_with("p2.exe \u{b7} PID 2 \u{b7} 50% of a core on average")),
            "{:?}",
            texts(&dl)
        );
        assert!(m.set_hover(None));
    }

    #[test]
    fn a_search_dims_what_does_not_match_but_keeps_its_ancestors() {
        let (u, snap) = usage_of(&[(1, 6), (2, 30), (3, 12), (4, 12)]);
        let mut m = UsageMap::default();
        painted(&mut m, &u, &snap, "p3");
        let dim = |pid: u32| m.nodes[m.index[&ProcessKey::new(pid, 1)]].dim;
        assert!(!dim(3), "the match");
        assert!(!dim(1), "its parent, for context");
        assert!(dim(2) && dim(4));
    }

    #[test]
    fn a_chain_of_single_children_folds_into_one_frame() {
        // 1 > 2 > 3, where 1 and 2 only launched the next; 3 has children 4 and 5.
        let mut u = Usage::new(Duration::from_secs(60));
        let tree = [
            (1, None),
            (2, Some(1)),
            (3, Some(2)),
            (4, Some(3)),
            (5, Some(3)),
        ];
        let used = [0u64, 0, 3, 20, 10];
        let snap_at = |secs: u64, busy: bool| {
            let procs = tree
                .iter()
                .zip(used)
                .map(|(&(pid, parent), s)| {
                    let mut p = proc(pid, parent, 1.0);
                    p.cpu_time = Duration::from_secs(if busy { 100 + s } else { 100 });
                    p
                })
                .collect();
            Snapshot {
                tick: Tick(secs),
                taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
                processes: procs,
                ..Default::default()
            }
        };
        u.observe(&snap_at(1000, false));
        let last = snap_at(1060, true);
        u.observe(&last);
        let mut m = UsageMap::default();
        let dl = painted(&mut m, &u, &last, "");
        let frames: Vec<&Tile> = m
            .tiles
            .iter()
            .filter(|t| t.kind == TileKind::Frame)
            .collect();
        assert_eq!(frames.len(), 1, "one frame, not three");
        assert_eq!(m.nodes[frames[0].top].statics.key.pid, 1);
        assert_eq!(m.nodes[frames[0].node].statics.key.pid, 3);
        let strings = texts(&dl);
        let chain = "p1.exe \u{203a} p2.exe \u{203a} p3.exe";
        assert!(strings.iter().any(|s| s == chain), "{strings:?}");
        // Selecting any link of the chain highlights the frame.
        assert!(m.tile_rect(row_id(ProcessKey::new(1, 1))).is_some());
    }

    #[test]
    fn idle_children_do_not_make_a_frame_or_break_a_chain() {
        // 1 > 2 and 1 > 3; 2 did all the work, 3 did nothing; 2 > 4 did a trace.
        let mut u = Usage::new(Duration::from_secs(60));
        let tree = [(1, None), (2, Some(1)), (3, Some(1)), (4, Some(2))];
        let used_ms = [0u64, 30_000, 0, 100];
        let snap_at = |secs: u64, busy: bool| {
            let procs = tree
                .iter()
                .zip(used_ms)
                .map(|(&(pid, parent), ms)| {
                    let mut p = proc(pid, parent, 1.0);
                    p.cpu_time = Duration::from_millis(100_000 + if busy { ms } else { 0 });
                    p
                })
                .collect();
            Snapshot {
                tick: Tick(secs),
                taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
                processes: procs,
                ..Default::default()
            }
        };
        u.observe(&snap_at(1000, false));
        let last = snap_at(1060, true);
        u.observe(&last);
        let mut m = UsageMap::default();
        let dl = painted(&mut m, &u, &last, "");
        // 1 folds into 2 despite its idle child 3; 2's child 4 is too idle for a
        // frame, so the whole thing is one tile.
        assert!(
            m.tiles.iter().all(|t| t.kind != TileKind::Frame),
            "no frames"
        );
        let strings = texts(&dl);
        assert!(
            strings.iter().any(|s| s == "p1.exe \u{203a} p2.exe"),
            "{strings:?}"
        );
    }

    #[test]
    fn the_strip_is_the_tree_folded_flat() {
        let (u, snap) = usage_of(&[(1, 6), (2, 30), (3, 12), (4, 12)]);
        let mut m = UsageMap::default();
        let mut dl = DisplayList::new();
        let rect = Rect::new(0.0, 0.0, 600.0, STRIP_H);
        m.paint_strip(
            &mut dl,
            rect,
            &u,
            &snap,
            "",
            None,
            &Theme::dark(),
            &mut String::new(),
        );
        assert_eq!(dl.clip_depth(), 0);
        let seg = |pid: u32| {
            m.tiles
                .iter()
                .find(|t| m.nodes[t.node].statics.key.pid == pid)
                .map(|t| t.rect)
                .unwrap()
        };
        let (one, four) = (seg(1), seg(4));
        // Top row: 48 s under p1 and 12 s for p4, across the whole width.
        assert!((one.y - four.y).abs() < 1e-3 && one.y.abs() < 1e-3);
        assert!((one.w - 480.0).abs() < 0.5 && (four.w - 120.0).abs() < 0.5);
        // Second row: p1's children within its span, in proportion to the whole
        // it spans (30 s and 12 s of 48); its own 6 s is the gap left at the end.
        let (two, three) = (seg(2), seg(3));
        assert!(two.y > one.y && (two.y - three.y).abs() < 1e-3);
        assert!(two.x >= one.x && three.right() <= one.right() + 1e-3);
        assert!((two.w - 300.0).abs() < 0.5 && (three.w - 120.0).abs() < 0.5);
        let strings = texts(&dl);
        assert!(strings.iter().any(|s| s == "p2.exe"), "{strings:?}");
        // Pointing at a segment describes it.
        assert!(m.set_hover(Some(two.center())));
        let mut out = String::new();
        assert!(m.describe_hover(&mut out));
        assert!(
            out.starts_with("p2.exe \u{b7} PID 2 \u{b7} 50% of a core"),
            "{out}"
        );
    }

    #[test]
    fn nothing_used_says_so() {
        let (u, snap) = usage_of(&[]);
        let mut m = UsageMap::default();
        let dl = painted(&mut m, &u, &snap, "");
        assert!(m.tiles.is_empty());
        assert!(texts(&dl)
            .iter()
            .any(|s| s == "Nothing has used the CPU yet"));
    }
}
