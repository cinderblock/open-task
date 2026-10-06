//! Pages, and the navigation rail down the left edge that switches between them.
//!
//! The rail follows the Windows 11 Task Manager and `NavigationView`: icons and
//! labels when the window is wide enough, icons alone when it is not, and a
//! hamburger button at the top that overrides the automatic choice. Only pages that
//! exist are listed. Settings sits apart at the bottom, as in Task Manager, with the
//! update button above it: the version, and what the updater is doing.

use ot_paint::{DisplayList, HAlign, Icon, Rect, VAlign};

use crate::theme::Theme;
use crate::update::UpdateView;

/// A top-level page of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Page {
    /// Every other page's headline numbers on one screen. First on the rail, as
    /// in TMOG; the app still opens on Processes, as Task Manager does.
    Summary,
    /// The process table, with the summary charts above it.
    #[default]
    Processes,
    /// Devices (CPU, memory, disks, networks, GPUs, the battery) with a chart and
    /// the numbers for each.
    Performance,
    /// Logon sessions and their processes.
    Users,
    /// Every service of the machine.
    Services,
    /// What runs at sign-in.
    Startup,
    /// Open network endpoints by process.
    Connections,
    /// Installed programs.
    Apps,
    /// The machine and its operating system.
    System,
    /// The app's settings, at the bottom of the rail.
    Settings,
}

impl Page {
    /// In rail order. Settings is last and drawn at the bottom.
    pub const ALL: [Self; 10] = [
        Self::Summary,
        Self::Processes,
        Self::Performance,
        Self::Users,
        Self::Services,
        Self::Startup,
        Self::Connections,
        Self::Apps,
        Self::System,
        Self::Settings,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Processes => "Processes",
            Self::Performance => "Performance",
            Self::Users => "Users",
            Self::Services => "Services",
            Self::Startup => "Startup apps",
            Self::Connections => "Connections",
            Self::Apps => "Installed apps",
            Self::System => "System",
            Self::Settings => "Settings",
        }
    }

    #[must_use]
    pub fn icon(self) -> Icon {
        match self {
            Self::Summary => Icon::Summary,
            Self::Processes => Icon::Processes,
            Self::Performance => Icon::Performance,
            Self::Users => Icon::Users,
            Self::Services => Icon::Services,
            Self::Startup => Icon::Startup,
            Self::Connections => Icon::Connections,
            Self::Apps => Icon::Apps,
            Self::System => Icon::System,
            Self::Settings => Icon::Settings,
        }
    }

    /// Whether the page shows time charts, which scroll while it is on screen.
    #[must_use]
    pub fn has_charts(self) -> bool {
        matches!(self, Self::Summary | Self::Processes | Self::Performance)
    }

    /// Whether the page lists things with a search field of its own, so typing
    /// on it filters it rather than the process table.
    #[must_use]
    pub fn has_search(self) -> bool {
        matches!(
            self,
            Self::Processes
                | Self::Users
                | Self::Services
                | Self::Startup
                | Self::Connections
                | Self::Apps
        )
    }

    /// The one-word name the command line's `--page` takes.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Processes => "processes",
            Self::Performance => "performance",
            Self::Users => "users",
            Self::Services => "services",
            Self::Startup => "startup",
            Self::Connections => "connections",
            Self::Apps => "apps",
            Self::System => "system",
            Self::Settings => "settings",
        }
    }

    /// Parse a command-line value, case-insensitively: a page's name or label, or
    /// a prefix of one that fits only one page.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        if s.is_empty() {
            return None;
        }
        if let Some(exact) = Self::ALL
            .into_iter()
            .find(|p| p.name() == s || p.label().to_ascii_lowercase() == s)
        {
            return Some(exact);
        }
        let mut hits = Self::ALL
            .into_iter()
            .filter(|p| p.name().starts_with(&s) || p.label().to_ascii_lowercase().starts_with(&s));
        let first = hits.next()?;
        hits.next().is_none().then_some(first)
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    /// The page `n` steps along the rail, wrapping around.
    #[must_use]
    pub fn step(self, n: isize) -> Self {
        let len = Self::ALL.len().cast_signed();
        Self::ALL[(self.index().cast_signed() + n)
            .rem_euclid(len)
            .cast_unsigned()]
    }

    /// The page at 1-based position `n` on the rail, as Ctrl+1..9 picks it.
    /// Settings has no number.
    #[must_use]
    pub fn nth(n: usize) -> Option<Self> {
        let i = n.checked_sub(1)?;
        Self::ALL
            .into_iter()
            .filter(|&p| p != Self::Settings)
            .nth(i)
    }
}

/// Rail width with labels.
pub(crate) const EXPANDED_W: f32 = 184.0;
/// Rail width with icons only.
pub(crate) const COMPACT_W: f32 = 48.0;
/// `NavigationView`'s default: at least this wide, the rail shows labels.
pub(crate) const EXPAND_AT: f32 = 1008.0;
const ITEM_H: f32 = 40.0;
const ICON_SIZE: f32 = 16.0;
/// The selected item's accent mark, at its left edge.
const PILL_W: f32 = 3.0;
const PILL_H: f32 = 16.0;
/// The update button's attention dot.
const DOT: f32 = 6.0;

/// What is under the pointer on the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavHit {
    Toggle,
    Page(Page),
    /// The version, which is the update button.
    Update,
}

#[derive(Debug, Default)]
pub(crate) struct NavRail {
    /// The user's choice from the hamburger; `None` follows the window width.
    expanded: Option<bool>,
    toggle: Rect,
    items: [Rect; Page::ALL.len()],
    update: Rect,
    hover: Option<NavHit>,
}

impl NavRail {
    #[must_use]
    pub fn is_expanded(&self, window_w: f32) -> bool {
        self.expanded.unwrap_or(window_w >= EXPAND_AT)
    }

    #[must_use]
    pub fn width(&self, window_w: f32) -> f32 {
        if self.is_expanded(window_w) {
            EXPANDED_W
        } else {
            COMPACT_W
        }
    }

    /// The hamburger: show labels if they are hidden, hide them if they are shown.
    pub fn toggle(&mut self, window_w: f32) {
        self.expanded = Some(!self.is_expanded(window_w));
    }

    #[must_use]
    pub fn hit(&self, p: ot_paint::Point) -> Option<NavHit> {
        if self.toggle.contains(p) {
            return Some(NavHit::Toggle);
        }
        if self.update.contains(p) {
            return Some(NavHit::Update);
        }
        Page::ALL
            .iter()
            .zip(&self.items)
            .find(|(_, r)| r.contains(p))
            .map(|(&page, _)| NavHit::Page(page))
    }

    /// Where a page's item was painted last.
    #[cfg(test)]
    pub fn item_rect(&self, page: Page) -> Rect {
        self.items[page.index()]
    }

    /// Where the update button was painted last.
    #[cfg(test)]
    pub fn update_rect(&self) -> Rect {
        self.update
    }

    /// Follow the pointer. Returns whether the highlight moved.
    pub fn set_hover(&mut self, p: Option<ot_paint::Point>) -> bool {
        let hit = p.and_then(|p| self.hit(p));
        std::mem::replace(&mut self.hover, hit) != hit
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        current: Page,
        expanded: bool,
        update: &UpdateView,
        theme: &Theme,
        buf: &mut String,
    ) {
        let inner = rect.inset(4.0, theme.gap);
        let (toggle, mut below) = inner.split_top(ITEM_H);
        self.toggle = toggle;
        self.item(
            dl,
            toggle,
            Icon::Menu,
            None,
            false,
            self.hover == Some(NavHit::Toggle),
            theme,
        );
        for (i, page) in Page::ALL.into_iter().enumerate() {
            let r = if page == Page::Settings {
                let (r, remaining) = below.split_bottom(ITEM_H);
                below = remaining;
                r
            } else {
                let (r, remaining) = below.split_top(ITEM_H);
                below = remaining;
                r
            };
            self.items[i] = r;
            let label = expanded.then(|| page.label());
            let hovered = self.hover == Some(NavHit::Page(page));
            self.item(dl, r, page.icon(), label, page == current, hovered, theme);
        }
        // Above Settings.
        let (r, _) = below.split_bottom(ITEM_H);
        self.update = r;
        let hovered = self.hover == Some(NavHit::Update) && update.action().is_some();
        Self::update_item(dl, r, update, expanded, hovered, theme, buf);
    }

    /// The update button: the icon, with a dot when a release is waiting or the
    /// last try failed; when expanded, the version over what the updater is doing.
    fn update_item(
        dl: &mut DisplayList,
        r: Rect,
        update: &UpdateView,
        expanded: bool,
        hovered: bool,
        theme: &Theme,
        buf: &mut String,
    ) {
        if hovered {
            dl.fill_round_rect(r.inset(0.0, 2.0), theme.card_radius, theme.button_hover);
        }
        let icon_box = Rect::new(r.x, r.y, COMPACT_W - 8.0, r.h);
        let icon_color = if update.busy() {
            theme.text_dim
        } else {
            theme.text
        };
        dl.icon(Icon::Update, icon_box, ICON_SIZE, icon_color);
        if update.attention() {
            let c = icon_box.center();
            let dot = Rect::new(c.x + 4.0, c.y - 10.0, DOT, DOT);
            dl.fill_round_rect(dot, DOT * 0.5, theme.accent);
        }
        if expanded {
            let (_, text) = r.split_left(COMPACT_W - 8.0);
            let (top, bottom) = text.split_top(text.h * 0.5);
            dl.text(
                update.short(),
                top.offset(0.0, 2.0),
                theme.cell,
                theme.text,
                HAlign::Left,
                VAlign::Bottom,
                true,
            );
            update.rail_status(buf);
            let color = if update.attention() {
                theme.accent
            } else {
                theme.text_dim
            };
            dl.text(
                buf,
                bottom.offset(0.0, 1.0),
                theme.small,
                color,
                HAlign::Left,
                VAlign::Top,
                true,
            );
        }
    }

    #[allow(clippy::too_many_arguments, clippy::unused_self)]
    fn item(
        &self,
        dl: &mut DisplayList,
        r: Rect,
        icon: Icon,
        label: Option<&str>,
        selected: bool,
        hovered: bool,
        theme: &Theme,
    ) {
        let bg = r.inset(0.0, 2.0);
        if selected {
            dl.fill_round_rect(bg, theme.card_radius, theme.button_hover);
            let pill = Rect::new(bg.x, bg.center().y - PILL_H * 0.5, PILL_W, PILL_H);
            dl.fill_round_rect(pill, PILL_W * 0.5, theme.accent);
        } else if hovered {
            dl.fill_round_rect(bg, theme.card_radius, theme.button_hover);
        }
        let icon_box = Rect::new(r.x, r.y, COMPACT_W - 8.0, r.h);
        dl.icon(icon, icon_box, ICON_SIZE, theme.text);
        if let Some(text) = label {
            let (_, text_rect) = r.split_left(COMPACT_W - 8.0);
            dl.label(text, text_rect, theme.cell, theme.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_paint::{DrawCmd, Point};

    #[test]
    fn pages_parse_step_and_number() {
        assert_eq!(Page::parse("Performance"), Some(Page::Performance));
        assert_eq!(Page::parse("perf"), Some(Page::Performance));
        assert_eq!(Page::parse("P"), None, "ambiguous prefix");
        assert_eq!(Page::parse(""), None);
        assert_eq!(Page::parse("services"), Some(Page::Services));
        assert_eq!(
            Page::parse("s"),
            None,
            "ambiguous: Summary, Services, Startup, System, Settings"
        );
        assert_eq!(Page::parse("sum"), Some(Page::Summary));
        assert_eq!(Page::Summary.step(1), Page::Processes);
        assert_eq!(Page::Processes.step(1), Page::Performance);
        assert_eq!(Page::Performance.step(1), Page::Users);
        assert_eq!(Page::Settings.step(1), Page::Summary, "wraps");
        assert_eq!(Page::Summary.step(-1), Page::Settings, "wraps back");
        assert_eq!(Page::parse("set"), Some(Page::Settings));
        assert_eq!(
            Page::parse("apps"),
            Some(Page::Apps),
            "the page's name wins"
        );
        assert_eq!(Page::parse("Installed apps"), Some(Page::Apps));
        assert_eq!(Page::parse("startup"), Some(Page::Startup));
        assert_eq!(Page::parse("Startup apps"), Some(Page::Startup));
        assert_eq!(Page::nth(1), Some(Page::Summary));
        assert_eq!(Page::nth(2), Some(Page::Processes));
        assert_eq!(Page::nth(3), Some(Page::Performance));
        assert_eq!(Page::nth(9), Some(Page::System));
        assert_eq!(Page::nth(10), None, "Settings has no number");
        assert_eq!(Page::nth(0), None);
    }

    #[test]
    fn the_rail_expands_with_the_window_until_the_user_says_otherwise() {
        let mut nav = NavRail::default();
        assert!(!nav.is_expanded(900.0));
        assert!(nav.is_expanded(1200.0));
        assert!((nav.width(900.0) - COMPACT_W).abs() < f32::EPSILON);
        nav.toggle(900.0);
        assert!(nav.is_expanded(900.0), "the hamburger opened it");
        assert!(nav.is_expanded(1200.0), "and it stays open when wide");
        nav.toggle(1200.0);
        assert!(!nav.is_expanded(1200.0));
    }

    fn painted(nav: &mut NavRail, expanded: bool) -> DisplayList {
        painted_with(nav, expanded, &UpdateView::new("0.2.1", true))
    }

    fn painted_with(nav: &mut NavRail, expanded: bool, update: &UpdateView) -> DisplayList {
        let mut dl = DisplayList::new();
        let w = if expanded { EXPANDED_W } else { COMPACT_W };
        nav.paint(
            &mut dl,
            Rect::new(0.0, 0.0, w, 600.0),
            Page::Performance,
            expanded,
            update,
            &Theme::dark(),
            &mut String::new(),
        );
        dl
    }

    fn texts(dl: &DisplayList) -> Vec<&str> {
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn items_hit_test_and_label_only_when_expanded() {
        let mut nav = NavRail::default();
        let dl = painted(&mut nav, false);
        let icons: Vec<Icon> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Icon { icon, .. } => Some(*icon),
                _ => None,
            })
            .collect();
        assert_eq!(
            icons,
            [
                Icon::Menu,
                Icon::Summary,
                Icon::Processes,
                Icon::Performance,
                Icon::Users,
                Icon::Services,
                Icon::Startup,
                Icon::Connections,
                Icon::Apps,
                Icon::System,
                Icon::Settings,
                Icon::Update,
            ]
        );
        assert!(!dl.cmds().iter().any(|c| matches!(c, DrawCmd::Text(_))));

        let theme = Theme::dark();
        let y0 = theme.gap + ITEM_H * 0.5;
        assert_eq!(nav.hit(Point::new(20.0, y0)), Some(NavHit::Toggle));
        assert_eq!(
            nav.hit(Point::new(20.0, y0 + ITEM_H)),
            Some(NavHit::Page(Page::Summary))
        );
        assert_eq!(
            nav.hit(Point::new(20.0, y0 + 2.0 * ITEM_H)),
            Some(NavHit::Page(Page::Processes))
        );
        assert_eq!(
            nav.hit(Point::new(20.0, y0 + 3.0 * ITEM_H)),
            Some(NavHit::Page(Page::Performance))
        );
        // Nine pages, then nothing until the buttons at the bottom.
        assert_eq!(
            nav.hit(Point::new(20.0, y0 + 9.0 * ITEM_H)),
            Some(NavHit::Page(Page::System))
        );
        assert_eq!(nav.hit(Point::new(20.0, y0 + 10.0 * ITEM_H)), None);
        // Settings is at the bottom of the rail, the update button above it.
        assert_eq!(
            nav.hit(Point::new(20.0, 600.0 - theme.gap - ITEM_H * 0.5)),
            Some(NavHit::Page(Page::Settings))
        );
        assert_eq!(
            nav.hit(Point::new(20.0, 600.0 - theme.gap - ITEM_H * 1.5)),
            Some(NavHit::Update)
        );
        assert!(nav.set_hover(Some(Point::new(20.0, y0))));
        assert!(!nav.set_hover(Some(Point::new(21.0, y0))), "same item");
        assert!(nav.set_hover(None));

        let dl = painted(&mut nav, true);
        assert_eq!(
            texts(&dl),
            [
                "Summary",
                "Processes",
                "Performance",
                "Users",
                "Services",
                "Startup apps",
                "Connections",
                "Installed apps",
                "System",
                "Settings",
                "v0.2.1",
                "Check for updates"
            ]
        );
    }

    #[test]
    fn the_update_button_shows_the_build_and_flags_a_waiting_release() {
        let mut nav = NavRail::default();
        let dots = |dl: &DisplayList| {
            dl.cmds()
                .iter()
                .filter(|c| matches!(c, DrawCmd::FillRoundRect { color, .. } if *color == Theme::dark().accent))
                .count()
        };
        let mut update = UpdateView::new("0.2.1-20-gdbfe022-dirty", true);
        let dl = painted_with(&mut nav, true, &update);
        assert_eq!(texts(&dl)[10..], ["dbfe022-dirty", "Check for updates"]);
        // The Performance page's pill is the only accent so far.
        let before = dots(&dl);

        update.set_status(ot_update::Status::Ready {
            version: ot_update::Version::parse("0.3.0").unwrap(),
        });
        let dl = painted_with(&mut nav, false, &update);
        assert!(texts(&dl).is_empty(), "compact: icons only");
        assert_eq!(dots(&dl), before + 1, "a dot on the icon");
        let dl = painted_with(&mut nav, true, &update);
        assert_eq!(texts(&dl)[11], "Install v0.3.0");
    }
}
