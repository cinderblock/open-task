//! The app's settings, and the page that shows them.
//!
//! [`Settings`] is what the user can change and what the shell persists (the Windows
//! shell keeps it per user in the registry). The page lists each setting as a card
//! with a title, a line or two of explanation and a switch; clicking anywhere on the
//! card flips it, and the change comes back to the shell as
//! [`Effect::SaveSettings`] so it can be stored.
//!
//! A setting that follows a system preference (row animation follows Windows'
//! animation effects, the reduced-motion switch) does so only until the user flips
//! it here; from then on the user's choice stands. Whenever that system preference
//! is off, the card says so, whichever way the switch is set, so the state of both
//! is always in view.

use ot_paint::{DisplayList, HAlign, Point, Rect, VAlign};

use crate::theme::Theme;
use crate::view::{Effect, MouseButton, Reaction, UiEvent};

/// Everything the user can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// Rows slide to their new places when the process table re-sorts, rather than
    /// jumping. `None` until the user chooses: then it follows the platform's
    /// animation setting.
    pub animate_rows: Option<bool>,
}

impl Settings {
    /// Whether rows slide, given whether the platform has animations on.
    #[must_use]
    pub fn animates_rows(self, system_animations: bool) -> bool {
        self.animate_rows.unwrap_or(system_animations)
    }
}

/// One switch on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    AnimateRows,
}

impl Toggle {
    const ALL: [Self; 1] = [Self::AnimateRows];

    fn title(self) -> &'static str {
        match self {
            Self::AnimateRows => "Animate rows as the order changes",
        }
    }

    /// The line under the title: what the setting does, and while it still
    /// follows Windows (with Windows' animations on), that it does.
    fn detail(self, s: Settings, system_animations: bool) -> &'static str {
        match (self, s.animate_rows, system_animations) {
            (Self::AnimateRows, None, true) => {
                "Rows slide to their new place when the table re-sorts. Following \
                 Windows' animation effects."
            }
            (Self::AnimateRows, _, _) => {
                "Rows slide to their new place when the table re-sorts, instead of \
                 jumping there."
            }
        }
    }

    /// A line of its own when Windows' animation effects are off, whatever the
    /// switch says, with what that means for the switch.
    fn system_note(self, s: Settings, system_animations: bool) -> Option<&'static str> {
        if system_animations {
            return None;
        }
        Some(match (self, s.animate_rows) {
            (Self::AnimateRows, None) => {
                "Windows' animation effects are off, so this is off until you turn it \
                 on here."
            }
            (Self::AnimateRows, Some(true)) => {
                "Windows' animation effects are off; open-task animates rows anyway."
            }
            (Self::AnimateRows, Some(false)) => "Windows' animation effects are off too.",
        })
    }

    /// Whether the switch shows on.
    fn get(self, s: Settings, system_animations: bool) -> bool {
        match self {
            Self::AnimateRows => s.animates_rows(system_animations),
        }
    }

    /// Flip what the switch shows, making it the user's explicit choice.
    fn flip(self, s: &mut Settings, system_animations: bool) {
        let on = self.get(*s, system_animations);
        match self {
            Self::AnimateRows => s.animate_rows = Some(!on),
        }
    }
}

const CARD_H: f32 = 64.0;
/// A card with a line about the system setting under its detail.
const CARD_H_NOTE: f32 = 80.0;
const TITLE_LINE_H: f32 = 20.0;
const LINE_H: f32 = 17.0;
const SWITCH_W: f32 = 40.0;
const SWITCH_H: f32 = 20.0;
const KNOB_R: f32 = 5.0;
/// Room for "On" / "Off" to the left of the switch.
const STATE_W: f32 = 36.0;

#[derive(Debug, Default)]
pub(crate) struct SettingsPage {
    cards: [Rect; Toggle::ALL.len()],
    hover: Option<usize>,
}

impl SettingsPage {
    /// Where the `i`th card was painted last.
    #[cfg(test)]
    pub fn card(&self, i: usize) -> Rect {
        self.cards[i]
    }

    fn card_at(&self, p: Point) -> Option<usize> {
        self.cards.iter().position(|r| r.contains(p))
    }

    /// Handle input on the page, changing `settings` when a card is clicked.
    /// `system_animations` is what a setting that still follows the system shows.
    pub fn handle(
        &mut self,
        ev: UiEvent,
        settings: &mut Settings,
        system_animations: bool,
    ) -> Reaction {
        match ev {
            UiEvent::MouseMove(p) => {
                let hit = self.card_at(p);
                Reaction::painted(std::mem::replace(&mut self.hover, hit) != hit)
            }
            UiEvent::MouseLeave => Reaction::painted(self.hover.take().is_some()),
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => match self.card_at(at) {
                Some(i) => {
                    Toggle::ALL[i].flip(settings, system_animations);
                    Reaction {
                        repaint: true,
                        effect: Some(Effect::SaveSettings(*settings)),
                    }
                }
                None => Reaction::NONE,
            },
            _ => Reaction::NONE,
        }
    }

    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        settings: Settings,
        system_animations: bool,
        theme: &Theme,
    ) {
        let (title, remaining) = rect.split_top(40.0);
        dl.text(
            "Settings",
            title,
            theme.big,
            theme.text,
            HAlign::Left,
            VAlign::Middle,
            true,
        );
        let (heading, mut remaining) = remaining.split_top(28.0);
        dl.label("Process table", heading, theme.title, theme.text_dim);

        for (i, toggle) in Toggle::ALL.into_iter().enumerate() {
            let note = toggle.system_note(settings, system_animations);
            let card_h = if note.is_some() { CARD_H_NOTE } else { CARD_H };
            let (card, below) = remaining.split_top(card_h);
            let (_, below) = below.split_top(4.0);
            remaining = below;
            self.cards[i] = card;
            let fill = if self.hover == Some(i) {
                theme.button_hover
            } else {
                theme.surface
            };
            dl.fill_round_rect(card, theme.card_radius, fill);
            dl.stroke_rect(card, theme.surface_border, 1.0);

            let inner = card.inset(theme.pad * 2.0, theme.pad);
            let (text, control) = inner.split_left((inner.w - SWITCH_W - STATE_W).max(0.0));
            // Title, detail and the note, as one block centered in the card.
            let lines = if note.is_some() { 2.0 } else { 1.0 };
            let block_h = TITLE_LINE_H + LINE_H * lines;
            let top = text.y + ((text.h - block_h) * 0.5).max(0.0);
            let line = |y: f32, h: f32| Rect::new(text.x, y, text.w, h);
            dl.label(
                toggle.title(),
                line(top, TITLE_LINE_H),
                theme.cell,
                theme.text,
            );
            let below_title = top + TITLE_LINE_H;
            dl.label(
                toggle.detail(settings, system_animations),
                line(below_title, LINE_H),
                theme.small,
                theme.text_dim,
            );
            if let Some(note) = note {
                dl.label(
                    note,
                    line(below_title + LINE_H, LINE_H),
                    theme.small,
                    theme.accent,
                );
            }
            let on = toggle.get(settings, system_animations);

            let (state, switch) = control.split_left(STATE_W);
            dl.text(
                if on { "On" } else { "Off" },
                state,
                theme.cell,
                theme.text,
                HAlign::Left,
                VAlign::Middle,
                false,
            );
            paint_switch(dl, switch, on, theme);
        }
    }
}

/// A Windows 11 style toggle switch, vertically centered in `r`: a pill, filled in
/// the accent color with a dark knob on the right when on, outlined with the knob
/// on the left when off.
fn paint_switch(dl: &mut DisplayList, r: Rect, on: bool, theme: &Theme) {
    let pill = Rect::new(
        r.right() - SWITCH_W,
        r.center().y - SWITCH_H * 0.5,
        SWITCH_W,
        SWITCH_H,
    );
    let radius = SWITCH_H * 0.5;
    let (knob_x, knob) = if on {
        // The window's own background reads as a hole in the accent, in both
        // themes, the way Windows draws it.
        dl.fill_round_rect(pill, radius, theme.accent);
        (pill.right() - radius, theme.bg_solid)
    } else {
        dl.fill_round_rect(pill.inset(0.5, 0.5), radius, theme.input_bg);
        dl.stroke_round_rect(pill, radius, theme.text_dim, 1.0);
        (pill.x + radius, theme.text_dim)
    };
    let c = Point::new(knob_x, pill.center().y);
    dl.fill_round_rect(
        Rect::new(c.x - KNOB_R, c.y - KNOB_R, 2.0 * KNOB_R, 2.0 * KNOB_R),
        KNOB_R,
        knob,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_paint::DrawCmd;

    fn texts(dl: &DisplayList) -> Vec<String> {
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn clicking_a_card_flips_its_setting_and_asks_to_save_it() {
        let theme = Theme::dark();
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        assert_eq!(s.animate_rows, None, "follows the system until chosen");
        let mut dl = DisplayList::new();
        page.paint(&mut dl, Rect::new(0.0, 0.0, 600.0, 400.0), s, true, &theme);
        let strings = texts(&dl);
        assert!(strings
            .iter()
            .any(|t| t == "Animate rows as the order changes"));
        assert!(strings.iter().any(|t| t == "On"));
        assert!(strings
            .iter()
            .any(|t| t.ends_with("Following Windows' animation effects.")));

        let card = page.cards[0].center();
        assert!(page.handle(UiEvent::MouseMove(card), &mut s, true).repaint);
        let r = page.handle(
            UiEvent::MouseDown {
                at: card,
                button: MouseButton::Left,
            },
            &mut s,
            true,
        );
        assert_eq!(s.animate_rows, Some(false));
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        dl.clear();
        page.paint(&mut dl, Rect::new(0.0, 0.0, 600.0, 400.0), s, true, &theme);
        assert!(texts(&dl).iter().any(|t| t == "Off"));

        // Off the cards: nothing.
        let r = page.handle(
            UiEvent::MouseDown {
                at: Point::new(5.0, 390.0),
                button: MouseButton::Left,
            },
            &mut s,
            true,
        );
        assert_eq!(r, Reaction::NONE);
        assert!(page.handle(UiEvent::MouseLeave, &mut s, true).repaint);
    }

    #[test]
    fn windows_animations_off_is_the_default_until_the_user_chooses() {
        let theme = Theme::dark();
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        assert!(!s.animates_rows(false));
        let mut dl = DisplayList::new();
        page.paint(&mut dl, Rect::new(0.0, 0.0, 600.0, 400.0), s, false, &theme);
        let strings = texts(&dl);
        assert!(strings.iter().any(|t| t == "Off"));
        assert!(strings.iter().any(|t| t
            == "Windows' animation effects are off, so this is off until you turn it on here."));
        let tall = page.cards[0].h;
        // Turning it on here overrides Windows, for this app, and the card still
        // says Windows has them off.
        let card = page.cards[0].center();
        let click = |page: &mut SettingsPage, s: &mut Settings| {
            let _ = page.handle(
                UiEvent::MouseDown {
                    at: card,
                    button: MouseButton::Left,
                },
                s,
                false,
            );
        };
        click(&mut page, &mut s);
        assert_eq!(s.animate_rows, Some(true));
        assert!(s.animates_rows(false));
        dl.clear();
        page.paint(&mut dl, Rect::new(0.0, 0.0, 600.0, 400.0), s, false, &theme);
        let strings = texts(&dl);
        assert!(strings.iter().any(|t| t == "On"));
        assert!(strings
            .iter()
            .any(|t| t == "Windows' animation effects are off; open-task animates rows anyway."));
        // Off by choice: still said.
        click(&mut page, &mut s);
        assert_eq!(s.animate_rows, Some(false));
        dl.clear();
        page.paint(&mut dl, Rect::new(0.0, 0.0, 600.0, 400.0), s, false, &theme);
        assert!(texts(&dl)
            .iter()
            .any(|t| t == "Windows' animation effects are off too."));
        // With Windows' animations on there is nothing to say, and the card is
        // back to its usual height.
        dl.clear();
        page.paint(&mut dl, Rect::new(0.0, 0.0, 600.0, 400.0), s, true, &theme);
        assert!(!texts(&dl).iter().any(|t| t.starts_with("Windows'")));
        assert!(page.cards[0].h < tall);
    }
}
