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
//!
//! The Updates section starts with the update button as a card: the full version,
//! what the updater is doing in a sentence, and a button for the next step, the
//! same step a click on the rail's version takes. Its three switches build on each
//! other (checking, then downloading, then installing), so turning one on turns on
//! those it needs, and turning one off turns off those that need it.

use ot_paint::{DisplayList, HAlign, Point, Rect, VAlign};

use crate::theme::Theme;
use crate::update::UpdateView;
use crate::view::{Effect, MouseButton, Reaction, UiEvent};

/// Everything the user can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Rows slide to their new places when the process table re-sorts, rather than
    /// jumping. `None` until the user chooses: then it follows the platform's
    /// animation setting.
    pub animate_rows: Option<bool>,
    /// Look for a new release at start and once a day, and say so on the update
    /// button. On unless turned off.
    pub check_updates: bool,
    /// Download and verify a new release as soon as a check finds it, so a click
    /// installs it. Off unless turned on.
    pub download_updates: bool,
    /// Install a downloaded release when open-task is closed, so the next start
    /// is the new version. Off unless turned on; implies the two above. Never while
    /// it runs: restarting would lose the history it has gathered.
    pub install_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            animate_rows: None,
            check_updates: true,
            download_updates: false,
            install_updates: false,
        }
    }
}

impl Settings {
    /// Whether rows slide, given whether the platform has animations on.
    #[must_use]
    pub fn animates_rows(self, system_animations: bool) -> bool {
        self.animate_rows.unwrap_or(system_animations)
    }
}

/// What the page shows besides the settings themselves.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Context<'a> {
    /// What a setting that still follows the system shows.
    pub system_animations: bool,
    pub update: &'a UpdateView,
}

/// One switch on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    AnimateRows,
    CheckUpdates,
    DownloadUpdates,
    InstallUpdates,
}

impl Toggle {
    fn title(self) -> &'static str {
        match self {
            Self::AnimateRows => "Animate rows as the order changes",
            Self::CheckUpdates => "Check for updates automatically",
            Self::DownloadUpdates => "Download updates automatically",
            Self::InstallUpdates => "Install updates automatically",
        }
    }

    /// The line under the title: what the setting does, and while it still
    /// follows Windows (with Windows' animations on), that it does.
    fn detail(self, s: Settings, cx: Context<'_>) -> &'static str {
        match (self, s.animate_rows, cx.system_animations) {
            (Self::AnimateRows, None, true) => {
                "Rows slide to their new place when the table re-sorts. Following \
                 Windows' animation effects."
            }
            (Self::AnimateRows, _, _) => {
                "Rows slide to their new place when the table re-sorts, instead of \
                 jumping there."
            }
            (Self::CheckUpdates, _, _) => {
                "Looks on GitHub for a new release when open-task starts and once a day."
            }
            (Self::DownloadUpdates, _, _) => {
                "Downloads and verifies a new release as soon as one is found, so one \
                 click installs it."
            }
            (Self::InstallUpdates, _, _) => {
                "Installs a downloaded release when you close open-task, so it starts \
                 as the new version next time."
            }
        }
    }

    /// A line of its own under the detail: Windows' animation effects are off,
    /// whatever the switch says, with what that means for the switch.
    fn note(self, s: Settings, cx: Context<'_>) -> Option<&'static str> {
        match self {
            Self::AnimateRows if !cx.system_animations => Some(match s.animate_rows {
                None => {
                    "Windows' animation effects are off, so this is off until you turn it \
                     on here."
                }
                Some(true) => "Windows' animation effects are off; open-task animates rows anyway.",
                Some(false) => "Windows' animation effects are off too.",
            }),
            _ => None,
        }
    }

    /// Whether the switch shows on.
    fn get(self, s: Settings, cx: Context<'_>) -> bool {
        match self {
            Self::AnimateRows => s.animates_rows(cx.system_animations),
            Self::CheckUpdates => s.check_updates,
            Self::DownloadUpdates => s.download_updates,
            Self::InstallUpdates => s.install_updates,
        }
    }

    /// Flip what the switch shows, making it the user's explicit choice. The
    /// update switches keep their chain: installing needs downloading, which needs
    /// checking.
    fn flip(self, s: &mut Settings, cx: Context<'_>) {
        let on = !self.get(*s, cx);
        match self {
            Self::AnimateRows => s.animate_rows = Some(on),
            Self::CheckUpdates => {
                s.check_updates = on;
                s.download_updates &= on;
                s.install_updates &= on;
            }
            Self::DownloadUpdates => {
                s.download_updates = on;
                s.check_updates |= on;
                s.install_updates &= on;
            }
            Self::InstallUpdates => {
                s.install_updates = on;
                s.check_updates |= on;
                s.download_updates |= on;
            }
        }
    }
}

/// A card on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    Toggle(Toggle),
    /// The version and the update button.
    Update,
}

/// The page, top to bottom: each section's heading and its cards.
const SECTIONS: [(&str, &[Card]); 2] = [
    ("Process table", &[Card::Toggle(Toggle::AnimateRows)]),
    (
        "Updates",
        &[
            Card::Update,
            Card::Toggle(Toggle::CheckUpdates),
            Card::Toggle(Toggle::DownloadUpdates),
            Card::Toggle(Toggle::InstallUpdates),
        ],
    ),
];
const CARD_COUNT: usize = 5;

/// The `i`th card, counting through every section.
fn card_kind(i: usize) -> Option<Card> {
    SECTIONS
        .iter()
        .flat_map(|(_, cards)| cards.iter().copied())
        .nth(i)
}

const CARD_H: f32 = 64.0;
/// A card with a line of note under its detail.
const CARD_H_NOTE: f32 = 80.0;
const TITLE_LINE_H: f32 = 20.0;
const LINE_H: f32 = 17.0;
const SWITCH_W: f32 = 40.0;
const SWITCH_H: f32 = 20.0;
const KNOB_R: f32 = 5.0;
/// Room for "On" / "Off" to the left of the switch.
const STATE_W: f32 = 36.0;
/// The update card's button: as wide as a switch with its state.
const BUTTON_W: f32 = SWITCH_W + STATE_W + 16.0;
const BUTTON_H: f32 = 28.0;

#[derive(Debug, Default)]
pub(crate) struct SettingsPage {
    cards: [Rect; CARD_COUNT],
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

    /// Handle input on the page: flip a switch (changing `settings`) or press the
    /// update button.
    pub fn handle(&mut self, ev: UiEvent, settings: &mut Settings, cx: Context<'_>) -> Reaction {
        match ev {
            UiEvent::MouseMove(p) => {
                let hit = self.card_at(p);
                Reaction::painted(std::mem::replace(&mut self.hover, hit) != hit)
            }
            UiEvent::MouseLeave => Reaction::painted(self.hover.take().is_some()),
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            } => match self.card_at(at).and_then(card_kind) {
                Some(Card::Toggle(toggle)) => {
                    toggle.flip(settings, cx);
                    Reaction {
                        repaint: true,
                        effect: Some(Effect::SaveSettings(*settings)),
                    }
                }
                Some(Card::Update) => cx
                    .update
                    .action()
                    .map_or(Reaction::NONE, |a| Reaction::effect(Effect::Update(a))),
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
        cx: Context<'_>,
        theme: &Theme,
        buf: &mut String,
    ) {
        let (title, mut remaining) = rect.split_top(40.0);
        dl.text(
            "Settings",
            title,
            theme.big,
            theme.text,
            HAlign::Left,
            VAlign::Middle,
            true,
        );
        let mut i = 0;
        for (heading, cards) in SECTIONS {
            let (heading_rect, below) = remaining.split_top(28.0);
            remaining = below;
            dl.label(heading, heading_rect, theme.title, theme.text_dim);
            for &card in cards {
                let note = match card {
                    Card::Toggle(t) => t.note(settings, cx),
                    Card::Update => cx.update.note(),
                };
                let card_h = if note.is_some() { CARD_H_NOTE } else { CARD_H };
                let (r, below) = remaining.split_top(card_h);
                let (_, below) = below.split_top(4.0);
                remaining = below;
                self.cards[i] = r;
                let clickable = match card {
                    Card::Toggle(_) => true,
                    Card::Update => cx.update.action().is_some(),
                };
                let fill = if clickable && self.hover == Some(i) {
                    theme.button_hover
                } else {
                    theme.surface
                };
                dl.fill_round_rect(r, theme.card_radius, fill);
                dl.stroke_rect(r, theme.surface_border, 1.0);
                let inner = r.inset(theme.pad * 2.0, theme.pad);
                let (text, control) = inner.split_left((inner.w - BUTTON_W).max(0.0));
                match card {
                    Card::Toggle(t) => {
                        paint_text(dl, text, t.title(), t.detail(settings, cx), note, theme);
                        let on = t.get(settings, cx);
                        let (state, switch) = control.split_left(control.w - SWITCH_W - 10.0);
                        dl.text(
                            if on { "On" } else { "Off" },
                            state,
                            theme.cell,
                            theme.text,
                            HAlign::Right,
                            VAlign::Middle,
                            false,
                        );
                        paint_switch(dl, switch, on, theme);
                    }
                    Card::Update => {
                        let mut title = String::new();
                        cx.update.title(&mut title);
                        cx.update.detail(buf);
                        paint_text(dl, text, &title, buf, note, theme);
                        let enabled = cx.update.button(buf);
                        paint_button(dl, control, buf, enabled, theme);
                    }
                }
                i += 1;
            }
        }
    }
}

/// A card's title, detail and note, as one block centered in `text`.
fn paint_text(
    dl: &mut DisplayList,
    text: Rect,
    title: &str,
    detail: &str,
    note: Option<&str>,
    theme: &Theme,
) {
    let lines = if note.is_some() { 2.0 } else { 1.0 };
    let block_h = TITLE_LINE_H + LINE_H * lines;
    let top = text.y + ((text.h - block_h) * 0.5).max(0.0);
    let line = |y: f32, h: f32| Rect::new(text.x, y, text.w - 8.0, h);
    dl.label(title, line(top, TITLE_LINE_H), theme.cell, theme.text);
    let below_title = top + TITLE_LINE_H;
    dl.label(
        detail,
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
}

/// A Windows 11 style button, at the right of `r` and vertically centered: filled
/// in the accent color when it does something, quiet when it does not.
fn paint_button(dl: &mut DisplayList, r: Rect, label: &str, enabled: bool, theme: &Theme) {
    let b = Rect::new(
        r.right() - BUTTON_W,
        r.center().y - BUTTON_H * 0.5,
        BUTTON_W,
        BUTTON_H,
    );
    let (fill, text) = if enabled {
        (theme.accent, theme.bg_solid)
    } else {
        (theme.input_bg, theme.text_dim)
    };
    dl.fill_round_rect(b, theme.card_radius, fill);
    dl.text(
        label,
        b,
        theme.cell,
        text,
        HAlign::Center,
        VAlign::Middle,
        true,
    );
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
    use crate::update::UpdateAction;
    use ot_paint::DrawCmd;
    use ot_update::{Status, Version};

    const PAGE: Rect = Rect::new(0.0, 0.0, 600.0, 700.0);

    fn texts(dl: &DisplayList) -> Vec<String> {
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect()
    }

    /// "On" or "Off", as painted on card `card`.
    fn state_of(dl: &DisplayList, card: Rect) -> String {
        dl.cmds()
            .iter()
            .find_map(|c| match c {
                DrawCmd::Text(t)
                    if card.contains(t.rect.center()) && matches!(dl.str(t.text), "On" | "Off") =>
                {
                    Some(dl.str(t.text).to_owned())
                }
                _ => None,
            })
            .expect("the card has a switch")
    }

    fn cx(system_animations: bool, update: &UpdateView) -> Context<'_> {
        Context {
            system_animations,
            update,
        }
    }

    fn paint(page: &mut SettingsPage, s: Settings, cx: Context<'_>) -> DisplayList {
        let mut dl = DisplayList::new();
        page.paint(&mut dl, PAGE, s, cx, &Theme::dark(), &mut String::new());
        dl
    }

    fn click(page: &mut SettingsPage, at: Point, s: &mut Settings, cx: Context<'_>) -> Reaction {
        page.handle(
            UiEvent::MouseDown {
                at,
                button: MouseButton::Left,
            },
            s,
            cx,
        )
    }

    #[test]
    fn clicking_a_card_flips_its_setting_and_asks_to_save_it() {
        let update = UpdateView::new("0.2.1", true);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        assert_eq!(s.animate_rows, None, "follows the system until chosen");
        let dl = paint(&mut page, s, cx(true, &update));
        let strings = texts(&dl);
        assert!(strings
            .iter()
            .any(|t| t == "Animate rows as the order changes"));
        assert_eq!(state_of(&dl, page.cards[0]), "On");
        assert!(strings
            .iter()
            .any(|t| t.ends_with("Following Windows' animation effects.")));

        let card = page.cards[0].center();
        assert!(
            page.handle(UiEvent::MouseMove(card), &mut s, cx(true, &update))
                .repaint
        );
        let r = click(&mut page, card, &mut s, cx(true, &update));
        assert_eq!(s.animate_rows, Some(false));
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        let dl = paint(&mut page, s, cx(true, &update));
        assert_eq!(state_of(&dl, page.cards[0]), "Off");

        // Off the cards: nothing.
        let r = click(
            &mut page,
            Point::new(5.0, PAGE.bottom() - 5.0),
            &mut s,
            cx(true, &update),
        );
        assert_eq!(r, Reaction::NONE);
        assert!(
            page.handle(UiEvent::MouseLeave, &mut s, cx(true, &update))
                .repaint
        );
    }

    #[test]
    fn windows_animations_off_is_the_default_until_the_user_chooses() {
        let update = UpdateView::new("0.2.1", true);
        let off = cx(false, &update);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        assert!(!s.animates_rows(false));
        let dl = paint(&mut page, s, off);
        assert_eq!(state_of(&dl, page.cards[0]), "Off");
        assert!(texts(&dl).iter().any(|t| t
            == "Windows' animation effects are off, so this is off until you turn it on here."));
        let tall = page.cards[0].h;
        // Turning it on here overrides Windows, for this app, and the card still
        // says Windows has them off.
        let card = page.cards[0].center();
        let _ = click(&mut page, card, &mut s, off);
        assert_eq!(s.animate_rows, Some(true));
        assert!(s.animates_rows(false));
        let dl = paint(&mut page, s, off);
        assert_eq!(state_of(&dl, page.cards[0]), "On");
        assert!(texts(&dl)
            .iter()
            .any(|t| t == "Windows' animation effects are off; open-task animates rows anyway."));
        // Off by choice: still said.
        let _ = click(&mut page, card, &mut s, off);
        assert_eq!(s.animate_rows, Some(false));
        let dl = paint(&mut page, s, off);
        assert!(texts(&dl)
            .iter()
            .any(|t| t == "Windows' animation effects are off too."));
        // With Windows' animations on there is nothing to say, and the card is
        // back to its usual height.
        let dl = paint(&mut page, s, cx(true, &update));
        assert!(!texts(&dl).iter().any(|t| t.starts_with("Windows'")));
        assert!(page.cards[0].h < tall);
    }

    #[test]
    fn the_update_card_shows_the_version_and_takes_the_next_step() {
        let mut update = UpdateView::new("0.2.1-20-gdbfe022-dirty", true);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        let dl = paint(&mut page, s, cx(true, &update));
        let strings = texts(&dl);
        for expected in [
            "Updates",
            "open-task v0.2.1-20-gdbfe022-dirty",
            "Looks on GitHub for a newer release.",
            "Check now",
        ] {
            assert!(strings.iter().any(|t| t == expected), "{expected}");
        }
        let card = page.cards[1].center();
        let r = click(&mut page, card, &mut s, cx(true, &update));
        assert_eq!(r.effect, Some(Effect::Update(UpdateAction::Check)));
        assert_eq!(s, Settings::default(), "no setting changed");

        // Busy: the card does nothing.
        update.set_status(Status::Checking);
        let _ = paint(&mut page, s, cx(true, &update));
        let r = click(&mut page, card, &mut s, cx(true, &update));
        assert_eq!(r, Reaction::NONE);

        update.set_status(Status::Ready {
            version: Version::parse("0.3.0").unwrap(),
        });
        let dl = paint(&mut page, s, cx(true, &update));
        assert!(texts(&dl).iter().any(|t| t == "Install"));
        let r = click(&mut page, card, &mut s, cx(true, &update));
        assert_eq!(r.effect, Some(Effect::Update(UpdateAction::Install)));
    }

    #[test]
    fn update_switches_default_to_check_but_not_download() {
        let update = UpdateView::new("0.2.1", false);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        let dl = paint(&mut page, s, cx(true, &update));
        assert_eq!(state_of(&dl, page.cards[2]), "On");
        assert_eq!(state_of(&dl, page.cards[3]), "Off");
        // A copy that cannot install says so, once, on the update card.
        let note = update.note().unwrap();
        assert_eq!(texts(&dl).iter().filter(|t| *t == note).count(), 1);

        let at = page.cards[2].center();
        let r = click(&mut page, at, &mut s, cx(true, &update));
        assert!(!s.check_updates);
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        let _ = paint(&mut page, s, cx(true, &update));
        let at = page.cards[3].center();
        let _ = click(&mut page, at, &mut s, cx(true, &update));
        assert!(s.download_updates);
        assert!(s.check_updates, "downloading needs checking");
    }

    #[test]
    fn installing_automatically_is_off_and_brings_what_it_needs() {
        let update = UpdateView::new("0.2.1", true);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        let dl = paint(&mut page, s, cx(true, &update));
        assert_eq!(state_of(&dl, page.cards[4]), "Off");
        assert!(texts(&dl)
            .iter()
            .any(|t| t == "Install updates automatically"));

        // On: downloading and checking come with it.
        s.check_updates = false;
        let at = page.cards[4].center();
        let r = click(&mut page, at, &mut s, cx(true, &update));
        assert!(s.install_updates && s.download_updates && s.check_updates);
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        // Downloading off: installing goes too, checking stays.
        let _ = paint(&mut page, s, cx(true, &update));
        let at = page.cards[3].center();
        let _ = click(&mut page, at, &mut s, cx(true, &update));
        assert!(!s.download_updates && !s.install_updates && s.check_updates);
        // Checking off takes everything with it.
        s.download_updates = true;
        s.install_updates = true;
        let _ = paint(&mut page, s, cx(true, &update));
        let at = page.cards[2].center();
        let _ = click(&mut page, at, &mut s, cx(true, &update));
        assert!(!s.check_updates && !s.download_updates && !s.install_updates);
    }
}
