//! The search field: type-to-filter, shared by every page that lists things.

use ot_paint::{DisplayList, Point, Rect};

use crate::theme::Theme;

/// The search field. Type-to-filter: printable keys land here whether or not it has
/// focus, so focus only decides where Enter and Escape go and whether a caret shows.
#[derive(Debug, Default)]
pub(crate) struct SearchBox {
    pub text: String,
    /// Lower-cased `text`, what the matcher uses.
    pub needle: String,
    pub focused: bool,
    pub rect: Rect,
    /// The clear button at the right end, when there is text.
    pub clear_rect: Rect,
    pub hover_clear: bool,
}

impl SearchBox {
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.needle.clear();
        self.needle
            .extend(text.chars().map(|c| c.to_ascii_lowercase()));
    }

    pub fn push(&mut self, c: char) {
        self.text.push(c);
        self.needle.push(c.to_ascii_lowercase());
    }

    /// Remove the last character, or the last word. Returns false if empty.
    pub fn backspace(&mut self, word: bool) -> bool {
        if self.text.is_empty() {
            return false;
        }
        if word {
            let trimmed = self.text.trim_end();
            let cut = trimmed
                .rfind(|c: char| c.is_whitespace() || c == '\\' || c == '/')
                .map_or(0, |i| i + 1);
            self.text.truncate(cut);
        } else {
            self.text.pop();
        }
        let t = std::mem::take(&mut self.text);
        self.set_text(&t);
        true
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.needle.clear();
    }

    pub fn paint(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme) {
        self.rect = rect;
        let r = Rect::new(rect.x, rect.y + 2.0, rect.w, rect.h - 4.0);
        dl.fill_round_rect(r, theme.card_radius, theme.input_bg);
        let border = if self.focused {
            theme.accent
        } else {
            theme.surface_border
        };
        dl.stroke_rect(r, border, 1.0);

        let inner = r.inset(theme.pad, 0.0);
        if self.text.is_empty() {
            self.clear_rect = Rect::ZERO;
            dl.label("Filter (Ctrl+F)", inner, theme.cell, theme.text_dim);
            if self.focused {
                dl.fill_rect(
                    Rect::new(inner.x, inner.y + 5.0, 1.0, inner.h - 10.0),
                    theme.text,
                );
            }
            return;
        }
        let (text_rect, clear) = inner.split_left((inner.w - 18.0).max(0.0));
        self.clear_rect = clear;
        dl.field(&self.text, text_rect, theme.cell, theme.text, self.focused);
        // A small ×, as geometry so it needs no glyph.
        let c = clear.center();
        let s = 3.5;
        let color = if self.hover_clear {
            theme.text
        } else {
            theme.text_dim
        };
        dl.line(
            Point::new(c.x - s, c.y - s),
            Point::new(c.x + s, c.y + s),
            color,
            1.2,
        );
        dl.line(
            Point::new(c.x - s, c.y + s),
            Point::new(c.x + s, c.y - s),
            color,
            1.2,
        );
    }
}
