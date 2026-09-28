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
//! it here; from then on the user's choice stands.

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

    /// The line under the title: what the setting does, or, while it still
    /// follows the system, that it does.
    fn detail(self, s: Settings, system_animations: bool) -> &'static str {
        match (self, s.animate_rows, system_animations) {
            (Self::AnimateRows, Some(_), _) => {
                "Rows slide to their new place when the table re-sorts, instead of \
                 jumping there."
            }
            (Self::AnimateRows, None, true) => {
                "Rows slide to their new place when the table re-sorts. Following \
                 Windows' animation effects."
            }
            (Self::AnimateRows, None, false) => {
                "Off because Windows' animation effects are off. Turn on to animate \
                 the table anyway."
            }
        }
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
            let (card, below) = remaining.split_top(CARD_H);
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
            let (head, sub) = text.split_top(text.h * 0.5);
            dl.text(
                toggle.title(),
                head,
                theme.cell,
                theme.text,
                HAlign::Left,
                VAlign::Bottom,
                true,
            );
            let on = toggle.get(settings, system_animations);
            dl.text(
                toggle.detail(settings, system_animations),
                sub,
                theme.small,
                theme.text_dim,
                HAlign::Left,
                VAlign::Top,
                true,
            );

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
        assert!(strings
            .iter()
            .any(|t| t.starts_with("Off because Windows' animation effects are off")));
        // Turning it on here overrides Windows, for this app.
        let card = page.cards[0].center();
        let _ = page.handle(
            UiEvent::MouseDown {
                at: card,
                button: MouseButton::Left,
            },
            &mut s,
            false,
        );
        assert_eq!(s.animate_rows, Some(true));
        assert!(s.animates_rows(false));
    }
}
