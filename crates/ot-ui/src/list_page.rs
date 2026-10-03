//! What the pages that list things have in common: a title line with the count
//! and a search field, a table under it, the keys and the mouse on both, and the
//! context menu's anchor. Each page supplies its rows (a [`RowSource`]) and makes
//! its own menu from the row the page says is selected.
//!
//! The search works as on the Processes page: printable keys land in the field
//! whether or not it has focus; the page recomputes which rows match and hands
//! that back to its row source as a visibility mask.

use ot_paint::{DisplayList, HAlign, Point, Rect, VAlign};

use crate::search::SearchBox;
use crate::table::{Column, Hit, RowId, RowSource, Table};
use crate::theme::Theme;
use crate::view::{Command, Key, MouseButton, UiEvent};

/// Where a list page's line of buttons goes, if it has any.
const BUTTON_H: f32 = 28.0;

/// What an event on a list page led to, for the page to act on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ListOutcome {
    Nothing,
    Repaint,
    /// The search text changed: recompute the matches, then repaint.
    SearchChanged,
    /// Show the context menu for the selected row at this point.
    Menu(Point),
    /// A button of the page's own (by index) was clicked.
    Button(usize),
}

/// A button on the page's title line: "Open Services", "Copy all".
#[derive(Debug, Clone)]
pub(crate) struct PageButton {
    pub label: &'static str,
    pub width: f32,
    pub enabled: bool,
}

#[derive(Debug)]
pub(crate) struct ListPage {
    pub table: Table,
    pub search: SearchBox,
    title: &'static str,
    /// Buttons on the title line, and where they were painted.
    pub buttons: Vec<PageButton>,
    button_rects: Vec<Rect>,
    hover_button: Option<usize>,
    /// Last known pointer position, for the cursor.
    mouse: Option<Point>,
}

impl ListPage {
    pub fn new(title: &'static str, columns: Vec<Column>, sort_col: usize) -> Self {
        Self {
            table: Table::new(columns, sort_col),
            search: SearchBox::default(),
            title,
            buttons: Vec::new(),
            button_rects: Vec::new(),
            hover_button: None,
            mouse: None,
        }
    }

    /// Whether the pointer is over the search field, for the shell's cursor.
    #[must_use]
    pub fn over_text_field(&self) -> bool {
        self.mouse
            .is_some_and(|p| self.search.rect.contains(p) && !self.search.clear_rect.contains(p))
    }

    #[must_use]
    pub fn over_divider(&self) -> bool {
        self.table.resizing()
            || self
                .mouse
                .is_some_and(|p| self.table.divider_at(p).is_some())
    }

    /// The selected row's index into the source, if it is still there.
    #[must_use]
    pub fn selected<S: RowSource>(&self, src: &S) -> Option<usize> {
        let id = self.table.selected?;
        (0..src.len()).find(|&r| src.id(r) == id)
    }

    /// Handle input. The page recomputes its matches on `SearchChanged`.
    #[allow(clippy::too_many_lines)]
    pub fn handle<S: RowSource>(&mut self, ev: UiEvent, src: &S, theme: &Theme) -> ListOutcome {
        match ev {
            UiEvent::Resize(_) => ListOutcome::Repaint,
            UiEvent::MouseMove(p) => {
                self.mouse = Some(p);
                if self.table.resizing() {
                    return if self.table.resize_to(p) {
                        ListOutcome::Repaint
                    } else {
                        ListOutcome::Nothing
                    };
                }
                if self.table.header_drag_to(p) {
                    return ListOutcome::Repaint;
                }
                let hover = match self.table.hit(p, theme) {
                    Hit::Row(i) | Hit::Expander(i) => Some(i),
                    _ => None,
                };
                let clear = self.search.clear_rect.contains(p);
                let button = self.button_rects.iter().position(|r| r.contains(p));
                let changed = hover != self.table.hover
                    || clear != self.search.hover_clear
                    || button != self.hover_button;
                self.table.hover = hover;
                self.search.hover_clear = clear;
                self.hover_button = button;
                if changed {
                    ListOutcome::Repaint
                } else {
                    ListOutcome::Nothing
                }
            }
            UiEvent::MouseLeave => {
                self.mouse = None;
                let changed = self.table.hover.take().is_some()
                    | std::mem::take(&mut self.search.hover_clear)
                    | self.hover_button.take().is_some();
                if changed {
                    ListOutcome::Repaint
                } else {
                    ListOutcome::Nothing
                }
            }
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => self.left_down(at, src, theme),
            UiEvent::MouseDown {
                at,
                button: MouseButton::Right,
            } => {
                self.select_at(at, src, theme);
                ListOutcome::Repaint
            }
            UiEvent::MouseUp {
                at,
                button: MouseButton::Left,
            } => {
                if self.table.resizing() {
                    self.table.resize_to(at);
                    self.table.end_resize();
                    return ListOutcome::Repaint;
                }
                if let Some(col) = self.table.end_header_press(at) {
                    self.table.set_sort(col);
                }
                ListOutcome::Repaint
            }
            UiEvent::Wheel {
                lines, horizontal, ..
            } => {
                if horizontal {
                    self.table.scroll_x_by(lines * 40.0);
                } else {
                    self.table.scroll_lines(lines * 3.0);
                }
                ListOutcome::Repaint
            }
            UiEvent::Char(c) => {
                self.search.focused = true;
                self.search.push(c);
                ListOutcome::SearchChanged
            }
            UiEvent::Key(k) => self.key(k, src, theme),
            UiEvent::ContextMenu { at } => {
                if let Some(p) = at {
                    if !self.select_at(p, src, theme)
                        && !matches!(self.table.hit(p, theme), Hit::Row(_) | Hit::Expander(_))
                    {
                        return ListOutcome::Nothing;
                    }
                    return ListOutcome::Menu(p);
                }
                // From the keyboard: just under the selected row's first cell.
                match self.table.selected_rect(src, theme) {
                    Some(r) => ListOutcome::Menu(Point::new(r.x + theme.pad, r.bottom())),
                    None => ListOutcome::Nothing,
                }
            }
            UiEvent::Command(Command::Find) => {
                self.search.focused = true;
                ListOutcome::Repaint
            }
            UiEvent::MouseUp { .. } | UiEvent::Command(_) => ListOutcome::Nothing,
        }
    }

    fn left_down<S: RowSource>(&mut self, at: Point, src: &S, theme: &Theme) -> ListOutcome {
        if let Some(i) = self.button_rects.iter().position(|r| r.contains(at)) {
            if self.buttons.get(i).is_some_and(|b| b.enabled) {
                return ListOutcome::Button(i);
            }
            return ListOutcome::Nothing;
        }
        if self.search.clear_rect.contains(at) {
            self.search.clear();
            self.search.focused = false;
            return ListOutcome::SearchChanged;
        }
        if self.search.rect.contains(at) {
            self.search.focused = true;
            return ListOutcome::Repaint;
        }
        self.search.focused = false;
        match self.table.hit(at, theme) {
            Hit::Divider(c) => {
                self.table.begin_resize(c, at);
                ListOutcome::Nothing
            }
            Hit::Header(c) => {
                self.table.begin_header_press(c, at);
                ListOutcome::Nothing
            }
            Hit::Expander(pos) => {
                self.table.toggle_expanded(pos, src);
                ListOutcome::Repaint
            }
            Hit::Row(pos) => {
                self.table.selected = self.table.row_at(pos).map(|r| src.id(r));
                ListOutcome::Repaint
            }
            Hit::Nothing => ListOutcome::Repaint,
        }
    }

    /// Select the row under `at`. Returns whether a row is there.
    fn select_at<S: RowSource>(&mut self, at: Point, src: &S, theme: &Theme) -> bool {
        match self.table.hit(at, theme) {
            Hit::Row(pos) | Hit::Expander(pos) => {
                self.table.selected = self.table.row_at(pos).map(|r| src.id(r));
                true
            }
            _ => false,
        }
    }

    fn key<S: RowSource>(&mut self, k: Key, src: &S, theme: &Theme) -> ListOutcome {
        let page = isize::try_from(self.table.rows_visible(theme).max(1)).unwrap_or(isize::MAX);
        match k {
            Key::Up => self.table.move_selection(src, -1, theme),
            Key::Down => self.table.move_selection(src, 1, theme),
            Key::PageUp => self.table.move_selection(src, -page, theme),
            Key::PageDown => self.table.move_selection(src, page, theme),
            Key::Home => self.table.select_end(src, true, theme),
            Key::End => self.table.select_end(src, false, theme),
            Key::Left => {
                return if self.table.collapse_or_parent(src, theme) {
                    ListOutcome::Repaint
                } else {
                    ListOutcome::Nothing
                };
            }
            Key::Right => {
                return if self.table.expand_or_child(src, theme) {
                    ListOutcome::Repaint
                } else {
                    ListOutcome::Nothing
                };
            }
            Key::Backspace | Key::WordBackspace => {
                if !self.search.backspace(k == Key::WordBackspace) {
                    return ListOutcome::Nothing;
                }
                self.search.focused = true;
                return ListOutcome::SearchChanged;
            }
            Key::Escape => {
                if self.search.text.is_empty() && !self.search.focused {
                    return ListOutcome::Nothing;
                }
                self.search.clear();
                self.search.focused = false;
                return ListOutcome::SearchChanged;
            }
            Key::Enter => {
                return if std::mem::take(&mut self.search.focused) {
                    ListOutcome::Repaint
                } else {
                    ListOutcome::Nothing
                };
            }
        }
        ListOutcome::Repaint
    }

    /// Paint the title line, the search field and the table. `count` is the
    /// title line's right-hand text ("12 of 340 services").
    pub fn paint<S: RowSource>(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        src: &S,
        count: &str,
        theme: &Theme,
        buf: &mut String,
    ) {
        let (title_line, body) = rect.split_top(theme.toolbar_h);
        let (_, body) = body.split_top(theme.gap * 0.5);
        // Title at the left; the count, the search and the buttons from the right.
        let (title, right) = title_line.split_left(160.0_f32.min(title_line.w));
        dl.label(self.title, title, theme.title, theme.text);
        let (right, count_rect) = right.split_left((right.w - 150.0).max(0.0));
        dl.text(
            count,
            count_rect,
            theme.small,
            theme.text_dim,
            HAlign::Right,
            VAlign::Middle,
            true,
        );
        let search_w = theme.search_w.min(right.w);
        let (right, search) = right.split_left((right.w - search_w).max(0.0));
        self.search.paint(dl, search, theme);
        // Buttons, right to left before the search.
        self.button_rects.clear();
        let mut edge = right.right() - theme.pad;
        for (i, b) in self.buttons.iter().enumerate() {
            let r = Rect::new(
                (edge - b.width).max(right.x),
                right.y + (right.h - BUTTON_H) * 0.5,
                b.width,
                BUTTON_H,
            );
            edge = r.x - theme.pad;
            self.button_rects.push(r);
            let fill = if self.hover_button == Some(i) && b.enabled {
                theme.button_hover
            } else {
                theme.surface
            };
            dl.fill_round_rect(r, theme.card_radius, fill);
            dl.stroke_rect(r, theme.surface_border, 1.0);
            let color = if b.enabled {
                theme.text
            } else {
                theme.text_dim
            };
            dl.text(
                b.label,
                r,
                theme.header,
                color,
                HAlign::Center,
                VAlign::Middle,
                true,
            );
        }
        self.table.paint(dl, body, src, theme, buf);
    }
}

/// Case-insensitive substring match of `needle` (already lower-cased) against any
/// of `fields`, for a page's search.
pub(crate) fn any_matches<'a>(needle: &str, fields: impl IntoIterator<Item = &'a str>) -> bool {
    needle.is_empty()
        || fields
            .into_iter()
            .any(|f| crate::process_rows::contains_ci(f, needle))
}

/// A stable row id for a page's own rows: a tag per page and a key.
pub(crate) fn page_row_id(tag: u64, key: u64) -> RowId {
    // Keep the top bits distinct from the process table's ids.
    RowId((tag << 56) ^ key.wrapping_mul(0x9E37_79B9_7F4A_7C15) | (1 << 63))
}
