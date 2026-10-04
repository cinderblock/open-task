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
//!
//! The Windows section holds the Task Manager card ([`TaskManager`]). It is not a
//! setting: its switch shows what the system does, and flipping it asks the shell
//! to change that ([`Effect::ReplaceTaskManager`]) rather than to store anything.
//! Where there is nothing to replace, the section is not shown.
//!
//! The fade card is a number: how fast the cycle totals fade ([`ot_core::Usage`]),
//! stepped through [`DECAY_STEPS`] with a minus and a plus button, the card saying
//! how long a total takes to halve at that rate. The speed and history cards step
//! the same way: how often the system is sampled, and how far back the charts
//! reach ([`HISTORY_STEPS`]), the history card saying what that commits to in
//! memory on this machine ([`HistoryCost`]).
//!
//! Below the page's title the sections scroll with the wheel when the window is too
//! short to show them all.

use std::fmt::Write as _;

use ot_model::Bytes;
use ot_paint::{DisplayList, HAlign, Point, Rect, VAlign};

use crate::format;
use crate::replay::RecordingView;
use crate::task_manager::TaskManager;
use crate::theme::Theme;
use crate::update::UpdateView;
use crate::view::{Effect, MouseButton, Reaction, UiEvent};

/// The rates the cycle totals can fade at, in percent a second: from halving in
/// over a minute to halving in a second.
pub const DECAY_STEPS: [u8; 10] = [1, 2, 3, 5, 7, 10, 15, 20, 30, 50];
/// The rate unless changed.
pub const DECAY_DEFAULT: u8 = 5;

/// How far back the charts can reach, in minutes: ten minutes to a day.
pub const HISTORY_STEPS: [u32; 8] = [10, 30, 60, 120, 180, 360, 720, 1440];
/// The reach unless changed: an hour.
pub const HISTORY_DEFAULT: u32 = 60;

/// Everything the user can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Each is an independent switch the user flips; an enum or bit set would only
// obscure them.
#[allow(clippy::struct_excessive_bools)]
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
    /// How fast the cycles a process has used fade from its total, in percent a
    /// second: one of [`DECAY_STEPS`]. The Cycles column, the Map, the usage strip
    /// and the History all show totals fading at this rate.
    pub usage_decay_percent: u8,
    /// Keep the window above every other, as Task Manager's option does.
    pub always_on_top: bool,
    /// Minimizing hides the window; the tray icon brings it back.
    pub hide_when_minimized: bool,
    /// How often the system is sampled, in milliseconds: one of [`SPEED_STEPS`].
    /// Task Manager's update speed: Low, Normal, High.
    pub update_interval_ms: u32,
    /// How far back the charts reach, in minutes: one of [`HISTORY_STEPS`]. Older
    /// samples are dropped, and the time axis spans what is held.
    pub history_minutes: u32,
}

/// The sampling intervals, slowest first, in milliseconds.
pub const SPEED_STEPS: [u32; 4] = [4000, 2000, 1000, 500];
pub const SPEED_DEFAULT: u32 = 1000;

impl Default for Settings {
    fn default() -> Self {
        Self {
            usage_decay_percent: DECAY_DEFAULT,
            animate_rows: None,
            check_updates: true,
            download_updates: false,
            install_updates: false,
            always_on_top: false,
            hide_when_minimized: false,
            update_interval_ms: SPEED_DEFAULT,
            history_minutes: HISTORY_DEFAULT,
        }
    }
}

impl Settings {
    /// Whether rows slide, given whether the platform has animations on.
    #[must_use]
    pub fn animates_rows(self, system_animations: bool) -> bool {
        self.animate_rows.unwrap_or(system_animations)
    }

    /// These settings with the fade rate set to the step nearest `percent`, as
    /// when reading a stored value.
    #[must_use]
    pub fn with_usage_decay(self, percent: u32) -> Self {
        let nearest = DECAY_STEPS
            .into_iter()
            .min_by_key(|&s| u32::from(s).abs_diff(percent))
            .unwrap_or(DECAY_DEFAULT);
        Self {
            usage_decay_percent: nearest,
            ..self
        }
    }

    /// The share of a cycle total lost each second, `0.05` for 5 %.
    #[must_use]
    pub fn usage_decay(self) -> f64 {
        f64::from(self.usage_decay_percent) / 100.0
    }

    /// These settings with the sampling interval set to the step nearest `ms`.
    #[must_use]
    pub fn with_interval(self, ms: u32) -> Self {
        let nearest = SPEED_STEPS
            .into_iter()
            .min_by_key(|&s| s.abs_diff(ms))
            .unwrap_or(SPEED_DEFAULT);
        Self {
            update_interval_ms: nearest,
            ..self
        }
    }

    /// The interval one step slower (`-1`) or faster (`1`).
    fn speed_step(self, by: isize) -> Option<u32> {
        let at = SPEED_STEPS
            .iter()
            .position(|&s| s <= self.update_interval_ms)
            .unwrap_or(SPEED_STEPS.len() - 1);
        at.checked_add_signed(by)
            .and_then(|i| SPEED_STEPS.get(i))
            .copied()
    }

    /// These settings with the history reach set to the step nearest `minutes`.
    #[must_use]
    pub fn with_history_minutes(self, minutes: u32) -> Self {
        let nearest = HISTORY_STEPS
            .into_iter()
            .min_by_key(|&s| s.abs_diff(minutes))
            .unwrap_or(HISTORY_DEFAULT);
        Self {
            history_minutes: nearest,
            ..self
        }
    }

    /// How far back the charts reach, in milliseconds.
    #[must_use]
    pub fn history_ms(self) -> i64 {
        i64::from(self.history_minutes) * 60_000
    }

    /// The reach one step shorter (`-1`) or longer (`1`), or `None` at the end of
    /// the steps.
    fn history_step(self, by: isize) -> Option<u32> {
        let at = HISTORY_STEPS
            .iter()
            .position(|&s| s >= self.history_minutes)
            .unwrap_or(HISTORY_STEPS.len() - 1);
        at.checked_add_signed(by)
            .and_then(|i| HISTORY_STEPS.get(i))
            .copied()
    }

    /// The reach as its card shows it: `10 min`, `1 h`, `24 h`.
    fn history_label(self, out: &mut String) {
        out.clear();
        let m = self.history_minutes;
        let _ = if m < 60 {
            write!(out, "{m} min")
        } else {
            write!(out, "{} h", m / 60)
        };
    }

    /// Task Manager's name for the interval.
    #[must_use]
    pub fn speed_label(self) -> &'static str {
        match self.update_interval_ms {
            0..=500 => "High",
            501..=1000 => "Normal",
            1001..=2000 => "Slow",
            _ => "Low",
        }
    }

    /// The fade rate one step slower (`-1`) or faster (`1`), or `None` at the end
    /// of the steps.
    fn decay_step(self, by: isize) -> Option<u8> {
        let at = DECAY_STEPS
            .iter()
            .position(|&s| s >= self.usage_decay_percent)
            .unwrap_or(DECAY_STEPS.len() - 1);
        at.checked_add_signed(by)
            .and_then(|i| DECAY_STEPS.get(i))
            .copied()
    }
}

/// What the charts' history costs on this machine, for the history card to say:
/// how many series the timeline holds, and what a frame of the usage history
/// takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryCost {
    /// Series in the timeline, each committed to [`ot_core::Retention::bytes_per_series`].
    pub series: usize,
    /// Bytes a frame of the usage history takes, [`ot_core::Usage::frame_bytes`].
    pub usage_frame_bytes: usize,
}

impl HistoryCost {
    /// Bytes a history reaching back `ms` commits to: every series' rings and the
    /// usage history.
    #[must_use]
    pub fn bytes(self, ms: i64) -> usize {
        ot_core::Retention::covering(ms).bytes_per_series() * self.series
            + ot_core::usage::history_bytes(ms, self.usage_frame_bytes)
    }
}

/// What the page shows besides the settings themselves.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Context<'a> {
    /// What a setting that still follows the system shows.
    pub system_animations: bool,
    pub update: &'a UpdateView,
    pub task_manager: &'a TaskManager,
    /// Whether this copy runs as administrator; the card offering to is shown
    /// only when it does not.
    pub elevated: bool,
    /// What a history length costs here.
    pub history: HistoryCost,
    /// What is being recorded, for the Recording card; `None` when nothing is.
    pub recording: Option<&'a RecordingView>,
    /// A recording is playing: recording makes no sense, so its card is hidden.
    pub replaying: bool,
}

/// One switch on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    AnimateRows,
    AlwaysOnTop,
    HideWhenMinimized,
    CheckUpdates,
    DownloadUpdates,
    InstallUpdates,
}

impl Toggle {
    fn title(self) -> &'static str {
        match self {
            Self::AnimateRows => "Animate rows as the order changes",
            Self::AlwaysOnTop => "Always on top",
            Self::HideWhenMinimized => "Hide when minimized",
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
            (Self::AlwaysOnTop, _, _) => {
                "Keeps the window above every other, so it stays in view while you \
                 work in the program you are watching."
            }
            (Self::HideWhenMinimized, _, _) => {
                "Minimizing takes the window off the taskbar; the icon in the \
                 notification area, with its CPU meter, brings it back."
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
            Self::AlwaysOnTop => s.always_on_top,
            Self::HideWhenMinimized => s.hide_when_minimized,
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
            Self::AlwaysOnTop => s.always_on_top = on,
            Self::HideWhenMinimized => s.hide_when_minimized = on,
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
    /// Whether the system opens open-task in its task manager's place.
    TaskManager,
    /// How fast the cycle totals fade: a number with a minus and a plus button.
    Decay,
    /// How often the system is sampled: Task Manager's update speed, stepped.
    Speed,
    /// How far back the charts reach: a length with a minus and a plus button.
    History,
    /// Start a copy as administrator; shown while this one is not.
    RunAsAdministrator,
    /// Record every pass to a file the user names, or stop.
    Record,
}

impl Card {
    /// Whether the card is on the page at all.
    fn shown(self, cx: Context<'_>) -> bool {
        match self {
            Self::TaskManager => cx.task_manager.available(),
            Self::RunAsAdministrator => !cx.elevated,
            Self::Record => !cx.replaying,
            Self::Toggle(_) | Self::Update | Self::Decay | Self::Speed | Self::History => true,
        }
    }

    /// The line of note under the card's detail, if it has one. `scratch` holds it
    /// when it has to be written out.
    fn note<'s>(
        self,
        settings: Settings,
        cx: Context<'_>,
        scratch: &'s mut String,
    ) -> Option<&'s str> {
        match self {
            Self::Toggle(t) => t.note(settings, cx),
            Self::Update => cx.update.note(),
            Self::TaskManager => cx.task_manager.note(scratch).then_some(scratch.as_str()),
            Self::Decay | Self::Speed | Self::History | Self::RunAsAdministrator | Self::Record => {
                None
            }
        }
    }

    /// Whether a click anywhere on the card does something now. The stepper
    /// cards answer only on their two buttons.
    fn clickable(self, cx: Context<'_>) -> bool {
        match self {
            Self::Decay | Self::Speed | Self::History => false,
            Self::Toggle(_) | Self::RunAsAdministrator | Self::Record => true,
            Self::Update => cx.update.action().is_some(),
            Self::TaskManager => !cx.task_manager.pending,
        }
    }
}

/// The page, top to bottom: each section's heading and its cards.
const SECTIONS: [(&str, &[Card]); 6] = [
    (
        "Process table",
        &[Card::Toggle(Toggle::AnimateRows), Card::Decay, Card::Speed],
    ),
    ("Charts", &[Card::History]),
    (
        "Window",
        &[
            Card::Toggle(Toggle::AlwaysOnTop),
            Card::Toggle(Toggle::HideWhenMinimized),
        ],
    ),
    (
        "Updates",
        &[
            Card::Update,
            Card::Toggle(Toggle::CheckUpdates),
            Card::Toggle(Toggle::DownloadUpdates),
            Card::Toggle(Toggle::InstallUpdates),
        ],
    ),
    ("Windows", &[Card::TaskManager, Card::RunAsAdministrator]),
    ("Recording", &[Card::Record]),
];
const CARD_COUNT: usize = 13;
/// The fade card's place among the cards, the speed card's and the history card's.
const DECAY_CARD: usize = 1;
const SPEED_CARD: usize = 2;
const HISTORY_CARD: usize = 3;

/// The `i`th card, counting through every section.
fn card_kind(i: usize) -> Option<Card> {
    SECTIONS
        .iter()
        .flat_map(|(_, cards)| cards.iter().copied())
        .nth(i)
}

const HEADING_H: f32 = 28.0;
const CARD_H: f32 = 64.0;
/// A card with a line of note under its detail.
const CARD_H_NOTE: f32 = 80.0;
const CARD_GAP: f32 = 4.0;
/// How far a notch of the wheel scrolls the page.
const WHEEL_STEP: f32 = 48.0;
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
/// The fade card's minus and plus buttons.
const STEP_W: f32 = 28.0;
/// A stepper's minus, value and plus: room between the buttons for the longest
/// value, "Normal" ("Norma" was all that showed in a switch's width).
const STEPPER_W: f32 = 2.0 * STEP_W + 60.0;

#[derive(Debug, Default)]
pub(crate) struct SettingsPage {
    /// Where each section's heading was placed, or `None` when none of its cards
    /// is shown.
    headings: [Option<Rect>; SECTIONS.len()],
    /// Where each card was placed; [`Rect::ZERO`] for a card not shown.
    cards: [Rect; CARD_COUNT],
    hover: Option<usize>,
    /// The fade card's minus and plus buttons, then the speed card's, then the
    /// history card's, and the one under the pointer.
    steppers: [Rect; 6],
    hover_step: Option<usize>,
    /// Where the sections scroll: the page below its title.
    view: Rect,
    /// How far the sections are scrolled, in DIPs, and how far they can be.
    scroll: f32,
    max_scroll: f32,
}

impl SettingsPage {
    /// Where the `i`th card was painted last.
    #[cfg(test)]
    pub fn card(&self, i: usize) -> Rect {
        self.cards[i]
    }

    /// The card under `p`, if it is in view.
    fn card_at(&self, p: Point) -> Option<usize> {
        if !self.view.contains(p) {
            return None;
        }
        self.cards.iter().position(|r| r.contains(p))
    }

    /// The stepper button under `p`: 0 and 1 for the fade card's minus and plus,
    /// 2 and 3 for the speed card's, 4 and 5 for the history card's.
    fn step_at(&self, p: Point) -> Option<usize> {
        if !self.view.contains(p) {
            return None;
        }
        self.steppers.iter().position(|r| r.contains(p))
    }

    /// Handle input on the page: flip a switch (changing `settings`), press the
    /// update button, ask for Task Manager to be replaced or restored, or scroll.
    pub fn handle(&mut self, ev: UiEvent, settings: &mut Settings, cx: Context<'_>) -> Reaction {
        match ev {
            UiEvent::MouseMove(p) => {
                let hit = self.card_at(p);
                let step = self.step_at(p);
                let card = std::mem::replace(&mut self.hover, hit) != hit;
                let button = std::mem::replace(&mut self.hover_step, step) != step;
                Reaction::painted(card || button)
            }
            UiEvent::MouseLeave => {
                let step = self.hover_step.take().is_some();
                Reaction::painted(self.hover.take().is_some() || step)
            }
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
                Some(Card::TaskManager) if !cx.task_manager.pending => {
                    Reaction::effect(Effect::ReplaceTaskManager(!cx.task_manager.on()))
                }
                Some(Card::Decay) => {
                    let by = match self.step_at(at) {
                        Some(0) => -1,
                        Some(1) => 1,
                        _ => return Reaction::NONE,
                    };
                    let Some(percent) = settings.decay_step(by) else {
                        return Reaction::NONE;
                    };
                    settings.usage_decay_percent = percent;
                    Reaction {
                        repaint: true,
                        effect: Some(Effect::SaveSettings(*settings)),
                    }
                }
                Some(Card::Speed) => {
                    let by = match self.step_at(at) {
                        Some(2) => -1,
                        Some(3) => 1,
                        _ => return Reaction::NONE,
                    };
                    let Some(ms) = settings.speed_step(by) else {
                        return Reaction::NONE;
                    };
                    settings.update_interval_ms = ms;
                    Reaction {
                        repaint: true,
                        effect: Some(Effect::SaveSettings(*settings)),
                    }
                }
                Some(Card::History) => {
                    let by = match self.step_at(at) {
                        Some(4) => -1,
                        Some(5) => 1,
                        _ => return Reaction::NONE,
                    };
                    let Some(minutes) = settings.history_step(by) else {
                        return Reaction::NONE;
                    };
                    settings.history_minutes = minutes;
                    Reaction {
                        repaint: true,
                        effect: Some(Effect::SaveSettings(*settings)),
                    }
                }
                Some(Card::RunAsAdministrator) => Reaction::effect(Effect::RunAsAdministrator),
                Some(Card::Record) => Reaction::effect(Effect::Record(cx.recording.is_none())),
                Some(Card::TaskManager) | None => Reaction::NONE,
            },
            UiEvent::Wheel {
                at,
                lines,
                horizontal: false,
            } if self.view.contains(at) => {
                let scroll = (self.scroll + lines * WHEEL_STEP).clamp(0.0, self.max_scroll);
                let moved = scroll - self.scroll;
                if moved.abs() < f32::EPSILON {
                    return Reaction::NONE;
                }
                self.scroll = scroll;
                // Move what was placed, so the pointer finds the card now under it
                // before the next paint places them again.
                self.shift(-moved);
                self.hover = self.card_at(at);
                self.hover_step = self.step_at(at);
                Reaction::REPAINT
            }
            _ => Reaction::NONE,
        }
    }

    /// Place the headings and cards top to bottom in `view`, scrolled, keeping the
    /// scroll in range. `scratch` holds the notes that have to be written out.
    fn lay_out(&mut self, view: Rect, settings: Settings, cx: Context<'_>, scratch: &mut String) {
        self.view = view;
        let mut y = 0.0;
        let mut i = 0;
        for (s, (_, cards)) in SECTIONS.iter().enumerate() {
            self.headings[s] = cards.iter().any(|c| c.shown(cx)).then(|| {
                let r = Rect::new(view.x, y, view.w, HEADING_H);
                y += HEADING_H;
                r
            });
            for &card in *cards {
                self.cards[i] = if card.shown(cx) {
                    let h = if card.note(settings, cx, scratch).is_some() {
                        CARD_H_NOTE
                    } else {
                        CARD_H
                    };
                    let r = Rect::new(view.x, y, view.w, h);
                    y += h + CARD_GAP;
                    r
                } else {
                    Rect::ZERO
                };
                i += 1;
            }
        }
        // The stepper cards' buttons, either side of their value, at the right.
        let pair = |card: Rect| {
            let right = card.right() - 16.0;
            let button = |x: f32| Rect::new(x, card.center().y - BUTTON_H * 0.5, STEP_W, BUTTON_H);
            [button(right - STEPPER_W), button(right - STEP_W)]
        };
        let [d0, d1] = pair(self.cards[DECAY_CARD]);
        let [s0, s1] = pair(self.cards[SPEED_CARD]);
        let [h0, h1] = pair(self.cards[HISTORY_CARD]);
        self.steppers = [d0, d1, s0, s1, h0, h1];
        self.max_scroll = (y - view.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        self.shift(view.y - self.scroll);
    }

    /// Move every placed heading and card down by `dy`.
    fn shift(&mut self, dy: f32) {
        for r in self.headings.iter_mut().flatten() {
            r.y += dy;
        }
        for r in self.cards.iter_mut().filter(|r| r.w > 0.0) {
            r.y += dy;
        }
        for r in &mut self.steppers {
            r.y += dy;
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        settings: Settings,
        cx: Context<'_>,
        theme: &Theme,
        buf: &mut String,
    ) {
        let (title, view) = rect.split_top(40.0);
        dl.text(
            "Settings",
            title,
            theme.big,
            theme.text,
            HAlign::Left,
            VAlign::Middle,
            true,
        );
        let mut note_buf = String::new();
        self.lay_out(view, settings, cx, &mut note_buf);
        dl.push_clip(view);
        for ((heading, _), r) in SECTIONS.iter().zip(self.headings) {
            if let Some(r) = r {
                dl.label(heading, r, theme.title, theme.text_dim);
            }
        }
        let cards = SECTIONS.iter().flat_map(|(_, cards)| cards.iter().copied());
        for (i, card) in cards.enumerate() {
            let r = self.cards[i];
            if r.w <= 0.0 || r.bottom() <= view.y || r.y >= view.bottom() {
                continue;
            }
            let note = card.note(settings, cx, &mut note_buf);
            let fill = if card.clickable(cx) && self.hover == Some(i) {
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
                    paint_state_and_switch(dl, control, t.get(settings, cx), theme);
                }
                Card::TaskManager => {
                    let (title, detail) = (TaskManager::title(), TaskManager::detail());
                    paint_text(dl, text, title, detail, note, theme);
                    paint_state_and_switch(dl, control, cx.task_manager.on(), theme);
                }
                Card::Decay => {
                    use std::fmt::Write as _;
                    buf.clear();
                    let _ = write!(
                        buf,
                        "Every second each total loses this share, so a total halves in \
                         {:.0} s. Slower keeps past work in view longer.",
                        ot_core::usage::half_life(settings.usage_decay())
                    );
                    paint_text(dl, text, "How fast cycles used fade", buf, note, theme);
                    let [minus, plus, ..] = self.steppers;
                    for (i, (r, by)) in [(minus, -1), (plus, 1)].into_iter().enumerate() {
                        let enabled = settings.decay_step(by).is_some();
                        let hover = enabled && self.hover_step == Some(i);
                        paint_stepper(dl, r, by > 0, enabled, hover, theme);
                    }
                    buf.clear();
                    let _ = write!(buf, "{}%", settings.usage_decay_percent);
                    let value = Rect::new(minus.right(), minus.y, plus.x - minus.right(), minus.h);
                    dl.text(
                        buf,
                        value,
                        theme.cell_num,
                        theme.text,
                        HAlign::Center,
                        VAlign::Middle,
                        false,
                    );
                }
                Card::Speed => {
                    use std::fmt::Write as _;
                    buf.clear();
                    let _ = write!(
                        buf,
                        "The system is sampled every {} s. Task Manager calls this {}. \
                         Space pauses the display.",
                        f64::from(settings.update_interval_ms) / 1000.0,
                        settings.speed_label().to_ascii_lowercase()
                    );
                    paint_text(dl, text, "Update speed", buf, note, theme);
                    let [_, _, minus, plus, _, _] = self.steppers;
                    for (i, (r, by)) in [(minus, -1), (plus, 1)].into_iter().enumerate() {
                        let enabled = settings.speed_step(by).is_some();
                        let hover = enabled && self.hover_step == Some(i + 2);
                        paint_stepper(dl, r, by > 0, enabled, hover, theme);
                    }
                    let value = Rect::new(minus.right(), minus.y, plus.x - minus.right(), minus.h);
                    dl.text(
                        settings.speed_label(),
                        value,
                        theme.cell_num,
                        theme.text,
                        HAlign::Center,
                        VAlign::Middle,
                        false,
                    );
                }
                Card::History => {
                    let ms = settings.history_ms();
                    let points = ot_core::Retention::covering(ms).points();
                    let mut size = String::new();
                    format::bytes(&mut size, Bytes(cx.history.bytes(ms) as u64));
                    buf.clear();
                    let _ = write!(
                        buf,
                        "Charts span this much and drop what is older: {points} points a \
                         chart, about {size} for the charts on this machine."
                    );
                    paint_text(dl, text, "How far charts reach back", buf, note, theme);
                    let [_, _, _, _, minus, plus] = self.steppers;
                    for (i, (r, by)) in [(minus, -1), (plus, 1)].into_iter().enumerate() {
                        let enabled = settings.history_step(by).is_some();
                        let hover = enabled && self.hover_step == Some(i + 4);
                        paint_stepper(dl, r, by > 0, enabled, hover, theme);
                    }
                    settings.history_label(buf);
                    let value = Rect::new(minus.right(), minus.y, plus.x - minus.right(), minus.h);
                    dl.text(
                        buf,
                        value,
                        theme.cell_num,
                        theme.text,
                        HAlign::Center,
                        VAlign::Middle,
                        false,
                    );
                }
                Card::RunAsAdministrator => {
                    paint_text(
                        dl,
                        text,
                        "Run as administrator",
                        "Starts a copy with administrator rights, which can read every \
                         process, sample CPU, and start and stop services; this one closes.",
                        note,
                        theme,
                    );
                    paint_button(dl, control, "Restart", true, theme);
                }
                Card::Record => {
                    let mut detail = String::new();
                    let (title, button) = if let Some(r) = cx.recording {
                        let mut size = String::new();
                        format::bytes(&mut size, Bytes(r.bytes));
                        let _ = write!(
                            detail,
                            "Recording to {}: {} frames, {size}",
                            r.name, r.frames
                        );
                        if r.dropped > 0 {
                            let _ = write!(detail, ", {} dropped", r.dropped);
                        }
                        ("Recording", "Stop")
                    } else {
                        detail.push_str(
                            "Every pass goes to a .otrec file you name, to play back later \
                             with open-task --replay <file>.",
                        );
                        ("Record to a file", "Record\u{2026}")
                    };
                    paint_text(dl, text, title, &detail, note, theme);
                    paint_button(dl, control, button, true, theme);
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
        }
        dl.pop_clip();
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

/// "On" or "Off" and the switch, at the right of `control`.
fn paint_state_and_switch(dl: &mut DisplayList, control: Rect, on: bool, theme: &Theme) {
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

/// One of the fade card's buttons: a minus, or a plus, drawn as geometry so it
/// needs no glyph; quiet at the end of the steps.
fn paint_stepper(
    dl: &mut DisplayList,
    r: Rect,
    plus: bool,
    enabled: bool,
    hover: bool,
    theme: &Theme,
) {
    let fill = if hover {
        theme.button_active
    } else {
        theme.input_bg
    };
    dl.fill_round_rect(r, theme.card_radius, fill);
    dl.stroke_round_rect(r, theme.card_radius, theme.surface_border, 1.0);
    let ink = if enabled {
        theme.text
    } else {
        theme.text_dim.with_alpha(0.3)
    };
    let c = r.center();
    let s = 5.0;
    dl.fill_rect(Rect::new(c.x - s, c.y - 0.75, 2.0 * s, 1.5), ink);
    if plus {
        dl.fill_rect(Rect::new(c.x - 0.75, c.y - s, 1.5, 2.0 * s), ink);
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
    use crate::task_manager::Replacement;
    use crate::update::UpdateAction;
    use ot_paint::DrawCmd;
    use ot_update::{Scope, Status, Version};

    /// Tall enough for every card at once; the scrolling test shortens it.
    const PAGE: Rect = Rect::new(0.0, 0.0, 600.0, 1100.0);

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

    /// Nothing to replace, as on a platform without Task Manager.
    static NO_TASK_MANAGER: TaskManager = TaskManager {
        replacement: Replacement::Unavailable,
        install: None,
        pending: false,
    };

    fn cx(system_animations: bool, update: &UpdateView) -> Context<'_> {
        // Elevated: the Run as administrator card is out of the way.
        Context {
            system_animations,
            update,
            task_manager: &NO_TASK_MANAGER,
            elevated: true,
            history: HistoryCost::default(),
            recording: None,
            replaying: false,
        }
    }

    /// The Task Manager card's place among the cards.
    const TASK_MANAGER: usize = 10;

    fn with_task_manager<'a>(update: &'a UpdateView, tm: &'a TaskManager) -> Context<'a> {
        Context {
            task_manager: tm,
            ..cx(true, update)
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
        let card = page.cards[6].center();
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
        assert_eq!(state_of(&dl, page.cards[7]), "On");
        assert_eq!(state_of(&dl, page.cards[8]), "Off");
        // A copy that cannot install says so, once, on the update card.
        let note = update.note().unwrap();
        assert_eq!(texts(&dl).iter().filter(|t| *t == note).count(), 1);

        let at = page.cards[7].center();
        let r = click(&mut page, at, &mut s, cx(true, &update));
        assert!(!s.check_updates);
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        let _ = paint(&mut page, s, cx(true, &update));
        let at = page.cards[8].center();
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
        assert_eq!(state_of(&dl, page.cards[9]), "Off");
        assert!(texts(&dl)
            .iter()
            .any(|t| t == "Install updates automatically"));

        // On: downloading and checking come with it.
        s.check_updates = false;
        let at = page.cards[9].center();
        let r = click(&mut page, at, &mut s, cx(true, &update));
        assert!(s.install_updates && s.download_updates && s.check_updates);
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        // Downloading off: installing goes too, checking stays.
        let _ = paint(&mut page, s, cx(true, &update));
        let at = page.cards[8].center();
        let _ = click(&mut page, at, &mut s, cx(true, &update));
        assert!(!s.download_updates && !s.install_updates && s.check_updates);
        // Checking off takes everything with it.
        s.download_updates = true;
        s.install_updates = true;
        let _ = paint(&mut page, s, cx(true, &update));
        let at = page.cards[7].center();
        let _ = click(&mut page, at, &mut s, cx(true, &update));
        assert!(!s.check_updates && !s.download_updates && !s.install_updates);
    }

    #[test]
    fn the_fade_card_steps_the_rate_and_asks_to_save_it() {
        let update = UpdateView::new("0.2.1", true);
        let cx = cx(true, &update);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        assert_eq!(s.usage_decay_percent, 5);
        assert!((s.usage_decay() - 0.05).abs() < 1e-12);
        let dl = paint(&mut page, s, cx);
        let strings = texts(&dl);
        assert!(strings.iter().any(|t| t == "How fast cycles used fade"));
        assert!(strings.iter().any(|t| t == "5%"));
        assert!(
            strings.iter().any(|t| t.contains("a total halves in 14 s")),
            "{strings:?}"
        );

        // The card itself does nothing; its buttons step the rate.
        let card = page.cards[DECAY_CARD];
        let r = click(
            &mut page,
            Point::new(card.x + 20.0, card.center().y),
            &mut s,
            cx,
        );
        assert_eq!(r, Reaction::NONE);
        let [minus, plus, ..] = page.steppers;
        assert!(card.contains(minus.center()) && card.contains(plus.center()));
        let r = click(&mut page, plus.center(), &mut s, cx);
        assert_eq!(s.usage_decay_percent, 7);
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        let _ = click(&mut page, minus.center(), &mut s, cx);
        let _ = click(&mut page, minus.center(), &mut s, cx);
        assert_eq!(s.usage_decay_percent, 3);
        assert!(texts(&paint(&mut page, s, cx)).iter().any(|t| t == "3%"));
        assert!(
            page.handle(UiEvent::MouseMove(minus.center()), &mut s, cx)
                .repaint
        );

        // The steps end at both ends.
        for _ in 0..DECAY_STEPS.len() {
            let _ = click(&mut page, minus.center(), &mut s, cx);
        }
        assert_eq!(s.usage_decay_percent, 1);
        assert_eq!(click(&mut page, minus.center(), &mut s, cx), Reaction::NONE);
        for _ in 0..DECAY_STEPS.len() {
            let _ = click(&mut page, plus.center(), &mut s, cx);
        }
        assert_eq!(s.usage_decay_percent, 50);
        assert_eq!(click(&mut page, plus.center(), &mut s, cx), Reaction::NONE);

        // A stored value that is not a step reads as the nearest one.
        assert_eq!(s.with_usage_decay(6).usage_decay_percent, 5);
        assert_eq!(s.with_usage_decay(0).usage_decay_percent, 1);
        assert_eq!(s.with_usage_decay(4000).usage_decay_percent, 50);
    }

    #[test]
    fn the_history_card_steps_the_reach_and_says_what_it_costs() {
        let update = UpdateView::new("0.2.1", true);
        let cx = Context {
            recording: None,
            replaying: false,
            history: HistoryCost {
                series: 50,
                usage_frame_bytes: 200,
            },
            ..cx(true, &update)
        };
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        assert_eq!(s.history_minutes, 60);
        assert_eq!(s.history_ms(), 3_600_000);
        let strings = texts(&paint(&mut page, s, cx));
        assert!(strings.iter().any(|t| t == "How far charts reach back"));
        assert!(strings.iter().any(|t| t == "1 h"), "{strings:?}");
        let detail = strings
            .iter()
            .find(|t| t.contains("points a chart"))
            .expect("the cost");
        assert!(detail.contains("963 points"), "{detail}");
        assert!(detail.contains("MB"), "{detail}");

        // The card itself does nothing; its buttons step the reach.
        let card = page.cards[HISTORY_CARD];
        let r = click(
            &mut page,
            Point::new(card.x + 20.0, card.center().y),
            &mut s,
            cx,
        );
        assert_eq!(r, Reaction::NONE);
        let [_, _, _, _, minus, plus] = page.steppers;
        assert!(card.contains(minus.center()) && card.contains(plus.center()));
        let r = click(&mut page, plus.center(), &mut s, cx);
        assert_eq!(s.history_minutes, 120);
        assert_eq!(r.effect, Some(Effect::SaveSettings(s)));
        for _ in 0..HISTORY_STEPS.len() {
            let _ = click(&mut page, plus.center(), &mut s, cx);
        }
        assert_eq!(s.history_minutes, 1440);
        assert_eq!(click(&mut page, plus.center(), &mut s, cx), Reaction::NONE);
        let strings = texts(&paint(&mut page, s, cx));
        assert!(strings.iter().any(|t| t == "24 h"), "{strings:?}");
        for _ in 0..HISTORY_STEPS.len() {
            let _ = click(&mut page, minus.center(), &mut s, cx);
        }
        assert_eq!(s.history_minutes, 10);
        assert_eq!(click(&mut page, minus.center(), &mut s, cx), Reaction::NONE);
        assert!(texts(&paint(&mut page, s, cx))
            .iter()
            .any(|t| t == "10 min"));

        // A stored value that is not a step reads as the nearest one; a longer
        // reach costs more.
        assert_eq!(s.with_history_minutes(100).history_minutes, 120);
        assert_eq!(s.with_history_minutes(0).history_minutes, 10);
        assert_eq!(s.with_history_minutes(100_000).history_minutes, 1440);
        assert!(cx.history.bytes(24 * 3_600_000) > cx.history.bytes(3_600_000));
    }

    #[test]
    fn nothing_to_replace_shows_no_windows_section() {
        let update = UpdateView::new("0.2.1", true);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        let dl = paint(&mut page, s, cx(true, &update));
        let strings = texts(&dl);
        assert!(!strings.iter().any(|t| t == "Windows"));
        assert!(!strings.iter().any(|t| t == "Replace Task Manager"));
        assert_eq!(page.cards[TASK_MANAGER], Rect::ZERO);
        let r = click(&mut page, Point::new(0.0, 0.0), &mut s, cx(true, &update));
        assert_eq!(r, Reaction::NONE);
    }

    #[test]
    fn the_task_manager_switch_asks_the_shell_and_shows_what_windows_does() {
        let update = UpdateView::new("0.2.1", true);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        let mut tm = TaskManager {
            replacement: Replacement::Off,
            install: Some(Scope::Machine),
            pending: false,
        };
        let dl = paint(&mut page, s, with_task_manager(&update, &tm));
        let strings = texts(&dl);
        for expected in ["Windows", "Replace Task Manager", TaskManager::detail()] {
            assert!(strings.iter().any(|t| t == expected), "{expected}");
        }
        let card = page.cards[TASK_MANAGER];
        assert_eq!(state_of(&dl, card), "Off");
        assert!(
            (card.h - CARD_H).abs() < f32::EPSILON,
            "an install for everyone has nothing to add"
        );

        // A click asks; it changes no setting, and the switch waits for the answer.
        let r = click(
            &mut page,
            card.center(),
            &mut s,
            with_task_manager(&update, &tm),
        );
        assert_eq!(r.effect, Some(Effect::ReplaceTaskManager(true)));
        assert_eq!(s, Settings::default());

        // Waiting for permission: said, and a second click does nothing.
        tm.pending = true;
        let dl = paint(&mut page, s, with_task_manager(&update, &tm));
        assert!(texts(&dl)
            .iter()
            .any(|t| t == "Waiting for permission from Windows\u{2026}"));
        let card = page.cards[TASK_MANAGER];
        let r = click(
            &mut page,
            card.center(),
            &mut s,
            with_task_manager(&update, &tm),
        );
        assert_eq!(r, Reaction::NONE);

        // Done: on, and a click turns it off.
        tm.pending = false;
        tm.replacement = Replacement::ThisCopy;
        let dl = paint(&mut page, s, with_task_manager(&update, &tm));
        let card = page.cards[TASK_MANAGER];
        assert_eq!(state_of(&dl, card), "On");
        let r = click(
            &mut page,
            card.center(),
            &mut s,
            with_task_manager(&update, &tm),
        );
        assert_eq!(r.effect, Some(Effect::ReplaceTaskManager(false)));
    }

    #[test]
    fn another_replacement_shows_off_and_is_named() {
        let update = UpdateView::new("0.2.1", true);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        let tm = TaskManager {
            replacement: Replacement::Other {
                path: r"C:\Tools\procexp64.exe".into(),
                exists: true,
            },
            install: Some(Scope::Machine),
            pending: false,
        };
        let dl = paint(&mut page, s, with_task_manager(&update, &tm));
        let card = page.cards[TASK_MANAGER];
        assert_eq!(state_of(&dl, card), "Off");
        assert!((card.h - CARD_H_NOTE).abs() < f32::EPSILON);
        assert!(texts(&dl)
            .iter()
            .any(|t| t.ends_with(r"C:\Tools\procexp64.exe")));
        // Turning it on takes over, as Process Explorer would.
        let r = click(
            &mut page,
            card.center(),
            &mut s,
            with_task_manager(&update, &tm),
        );
        assert_eq!(r.effect, Some(Effect::ReplaceTaskManager(true)));
    }

    #[test]
    fn a_short_window_scrolls_to_the_last_card() {
        let update = UpdateView::new("0.2.1", true);
        let tm = TaskManager {
            replacement: Replacement::Off,
            install: Some(Scope::Machine),
            pending: false,
        };
        let cx = with_task_manager(&update, &tm);
        let short = Rect::new(0.0, 0.0, 600.0, 300.0);
        let mut page = SettingsPage::default();
        let mut s = Settings::default();
        let mut dl = DisplayList::new();
        page.paint(&mut dl, short, s, cx, &Theme::dark(), &mut String::new());
        let last = page.cards[TASK_MANAGER];
        assert!(last.y >= short.bottom(), "starts below the window");
        assert!(!texts(&dl).iter().any(|t| t == "Replace Task Manager"));
        // Out of view, a click there does nothing.
        let r = click(&mut page, Point::new(10.0, 299.0), &mut s, cx);
        assert_eq!(r, Reaction::NONE);

        let wheel = |page: &mut SettingsPage, s: &mut Settings, lines: f32| {
            page.handle(
                UiEvent::Wheel {
                    at: Point::new(300.0, 200.0),
                    lines,
                    horizontal: false,
                },
                s,
                cx,
            )
        };
        // Far past the end: stops with the last card at the bottom.
        assert_eq!(wheel(&mut page, &mut s, 50.0), Reaction::REPAINT);
        let mut dl = DisplayList::new();
        page.paint(&mut dl, short, s, cx, &Theme::dark(), &mut String::new());
        // The Recording card is the last on the page.
        let last = page.cards[CARD_COUNT - 1];
        assert!(
            (last.bottom() + CARD_GAP - short.bottom()).abs() < 0.01,
            "{last:?}"
        );
        assert!(texts(&dl).iter().any(|t| t == "Record to a file"));
        let r = click(&mut page, last.center(), &mut s, cx);
        assert_eq!(
            r.effect,
            Some(Effect::Record(true)),
            "nothing recording: start"
        );
        // Already at the end: nothing moves.
        assert_eq!(wheel(&mut page, &mut s, 1.0), Reaction::NONE);
        // Back to the top.
        assert_eq!(wheel(&mut page, &mut s, -50.0), Reaction::REPAINT);
        assert!(page.cards[0].y > 0.0 && page.cards[0].y < 100.0);
        assert_eq!(wheel(&mut page, &mut s, -1.0), Reaction::NONE);
    }
}
