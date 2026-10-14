//! Which toolkit to try first.
//!
//! `--toolkit gtk|qt` or `OPEN_TASK_TOOLKIT`, else the desktop's own: Qt on KDE
//! Plasma, `LXQt`, Deepin and UKUI, GTK everywhere else. The other one is always
//! the fallback, for a machine that lacks the first one's libraries.

/// A toolkit a plugin is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Toolkit {
    Gtk,
    Qt,
}

impl Toolkit {
    /// `gtk` or `qt`, any case; `gtk4` and `qt6` too.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gtk" | "gtk4" => Some(Self::Gtk),
            "qt" | "qt6" => Some(Self::Qt),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Gtk => "GTK",
            Self::Qt => "Qt",
        }
    }

    /// The plugin's file name, as `cargo build` names it beside the binary.
    #[must_use]
    pub fn library(self) -> &'static str {
        match self {
            Self::Gtk => "libot_shell_gtk.so",
            Self::Qt => "libot_shell_qt.so",
        }
    }

    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::Gtk => Self::Qt,
            Self::Qt => Self::Gtk,
        }
    }
}

/// Desktops built on Qt, as `XDG_CURRENT_DESKTOP` names them.
const QT_DESKTOPS: [&str; 5] = ["kde", "lxqt", "deepin", "dde", "ukui"];

/// Whether the desktop (`XDG_CURRENT_DESKTOP`, a colon-separated list) is built
/// on Qt.
#[must_use]
pub fn desktop_prefers_qt(desktop: &str) -> bool {
    desktop
        .split(':')
        .any(|d| QT_DESKTOPS.contains(&d.trim().to_ascii_lowercase().as_str()))
}

/// The toolkits to try, in order: the one asked for, else the desktop's, then
/// the other.
#[must_use]
pub fn order(asked: Option<Toolkit>, desktop: Option<&str>) -> [Toolkit; 2] {
    let first = asked.unwrap_or_else(|| {
        if desktop.is_some_and(desktop_prefers_qt) {
            Toolkit::Qt
        } else {
            Toolkit::Gtk
        }
    });
    [first, first.other()]
}

/// Whether there is a display to open a window on.
#[must_use]
pub fn display_available(wayland: Option<&str>, x11: Option<&str>) -> bool {
    wayland.is_some_and(|w| !w.is_empty()) || x11.is_some_and(|d| !d.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_picks_the_toolkit() {
        assert_eq!(order(None, Some("KDE")), [Toolkit::Qt, Toolkit::Gtk]);
        assert_eq!(
            order(None, Some("ubuntu:GNOME")),
            [Toolkit::Gtk, Toolkit::Qt]
        );
        assert_eq!(order(None, Some("LXQt")), [Toolkit::Qt, Toolkit::Gtk]);
        assert_eq!(order(None, Some("XFCE")), [Toolkit::Gtk, Toolkit::Qt]);
        assert_eq!(order(None, None), [Toolkit::Gtk, Toolkit::Qt]);
    }

    #[test]
    fn asking_beats_the_desktop() {
        assert_eq!(
            order(Some(Toolkit::Gtk), Some("KDE")),
            [Toolkit::Gtk, Toolkit::Qt]
        );
        assert_eq!(Toolkit::parse(" QT6 "), Some(Toolkit::Qt));
        assert_eq!(Toolkit::parse("motif"), None);
    }

    #[test]
    fn a_display_is_either_kind() {
        assert!(display_available(Some("wayland-0"), None));
        assert!(display_available(None, Some(":0")));
        assert!(!display_available(Some(""), None));
        assert!(!display_available(None, None));
    }
}
