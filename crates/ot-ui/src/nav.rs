//! Pages, and the navigation rail down the left edge that switches between them.
//!
//! The rail follows the Windows 11 Task Manager and `NavigationView`: icons and
//! labels when the window is wide enough, icons alone when it is not, and a
//! hamburger button at the top that overrides the automatic choice. Only pages that
//! exist are listed. Settings sits apart at the bottom, as in Task Manager.

use ot_paint::{DisplayList, Icon, Rect};

use crate::theme::Theme;

/// A top-level page of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Page {
    /// The process table, with the summary charts above it.
    #[default]
    Processes,
    /// Devices (CPU, memory) with a chart and the numbers for each.
    Performance,
    /// The app's settings, at the bottom of the rail.
    Settings,
}

impl Page {
    /// In rail order. Settings is last and drawn at the bottom.
    pub const ALL: [Self; 3] = [Self::Processes, Self::Performance, Self::Settings];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Performance => "Performance",
            Self::Settings => "Settings",
        }
    }

    #[must_use]
    pub fn icon(self) -> Icon {
        match self {
            Self::Processes => Icon::Processes,
            Self::Performance => Icon::Performance,
            Self::Settings => Icon::Settings,
        }
    }

    /// Parse a command-line value, case-insensitively, by name or unique prefix.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        let mut hits = Self::ALL
            .into_iter()
            .filter(|p| !s.is_empty() && p.label().to_ascii_lowercase().starts_with(&s));
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

/// What is under the pointer on the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavHit {
    Toggle,
    Page(Page),
}

#[derive(Debug, Default)]
pub(crate) struct NavRail {
    /// The user's choice from the hamburger; `None` follows the window width.
    expanded: Option<bool>,
    toggle: Rect,
    items: [Rect; Page::ALL.len()],
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

    /// Follow the pointer. Returns whether the highlight moved.
    pub fn set_hover(&mut self, p: Option<ot_paint::Point>) -> bool {
        let hit = p.and_then(|p| self.hit(p));
        std::mem::replace(&mut self.hover, hit) != hit
    }

    pub fn paint(
        &mut self,
        dl: &mut DisplayList,
        rect: Rect,
        current: Page,
        expanded: bool,
        theme: &Theme,
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
        assert_eq!(Page::parse("services"), None);
        assert_eq!(Page::Processes.step(1), Page::Performance);
        assert_eq!(Page::Performance.step(1), Page::Settings);
        assert_eq!(Page::Settings.step(1), Page::Processes, "wraps");
        assert_eq!(Page::Processes.step(-1), Page::Settings, "wraps back");
        assert_eq!(Page::parse("set"), Some(Page::Settings));
        assert_eq!(Page::nth(1), Some(Page::Processes));
        assert_eq!(Page::nth(2), Some(Page::Performance));
        assert_eq!(Page::nth(3), None, "Settings has no number");
        assert_eq!(Page::nth(0), None);
        assert_eq!(Page::nth(9), None);
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
        let mut dl = DisplayList::new();
        let w = if expanded { EXPANDED_W } else { COMPACT_W };
        nav.paint(
            &mut dl,
            Rect::new(0.0, 0.0, w, 600.0),
            Page::Performance,
            expanded,
            &Theme::dark(),
        );
        dl
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
                Icon::Processes,
                Icon::Performance,
                Icon::Settings
            ]
        );
        assert!(!dl.cmds().iter().any(|c| matches!(c, DrawCmd::Text(_))));

        let theme = Theme::dark();
        let y0 = theme.gap + ITEM_H * 0.5;
        assert_eq!(nav.hit(Point::new(20.0, y0)), Some(NavHit::Toggle));
        assert_eq!(
            nav.hit(Point::new(20.0, y0 + ITEM_H)),
            Some(NavHit::Page(Page::Processes))
        );
        assert_eq!(
            nav.hit(Point::new(20.0, y0 + 2.0 * ITEM_H)),
            Some(NavHit::Page(Page::Performance))
        );
        assert_eq!(nav.hit(Point::new(20.0, y0 + 3.0 * ITEM_H)), None);
        // Settings is at the bottom of the rail.
        assert_eq!(
            nav.hit(Point::new(20.0, 600.0 - theme.gap - ITEM_H * 0.5)),
            Some(NavHit::Page(Page::Settings))
        );
        assert!(nav.set_hover(Some(Point::new(20.0, y0))));
        assert!(!nav.set_hover(Some(Point::new(21.0, y0))), "same item");
        assert!(nav.set_hover(None));

        let dl = painted(&mut nav, true);
        let labels: Vec<&str> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text)),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["Processes", "Performance", "Settings"]);
    }
}
