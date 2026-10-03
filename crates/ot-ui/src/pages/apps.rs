//! The Installed apps page: the programs the system's uninstall list knows, with
//! their publisher, version, date and size, as TMOG's Installed Apps view and
//! Programs and Features show them; uninstall, open the folder, look it up.
//!
//! The list is read on demand when the page is shown; it changes only when
//! something is installed or removed, so it is not re-read on its own.

use std::cmp::Ordering;

use ot_model::apps::InstalledApp;
use ot_paint::{DisplayList, Point, Rect};

use super::{count_text, PageOutcome};
use crate::format;
use crate::list_page::{any_matches, page_row_id, ListOutcome, ListPage};
use crate::pages::services::copy_row;
use crate::process_rows::hash_str;
use crate::table::{Column, RowId, RowSource};
use crate::theme::Theme;
use crate::view::{search_url, Cursor, Effect, MenuAction, MenuEntry, Query, Reaction, UiEvent};

mod col {
    pub const NAME: usize = 0;
    pub const PUBLISHER: usize = 1;
    pub const VERSION: usize = 2;
    pub const INSTALLED: usize = 3;
    pub const SIZE: usize = 4;
    pub const SCOPE: usize = 5;
    pub const LOCATION: usize = 6;
}

fn columns() -> Vec<Column> {
    vec![
        Column::text("Name", 280.0),
        Column::text("Publisher", 180.0),
        Column::text("Version", 110.0),
        Column::text("Installed on", 100.0),
        Column::number("Size", 90.0),
        Column::text("Scope", 90.0),
        Column::text("Location", 320.0),
    ]
}

#[derive(Debug)]
pub(crate) struct AppsPage {
    list: ListPage,
    apps: Option<Vec<InstalledApp>>,
    matched: Vec<bool>,
    buf: String,
}

impl Default for AppsPage {
    fn default() -> Self {
        let mut list = ListPage::new("Installed apps", columns(), col::NAME);
        list.table.sort_desc = false;
        Self {
            list,
            apps: None,
            matched: Vec::new(),
            buf: String::new(),
        }
    }
}

struct Rows<'a> {
    apps: &'a [InstalledApp],
    matched: &'a [bool],
}

fn scope(a: &InstalledApp) -> &'static str {
    match (a.per_user, a.x86) {
        (true, true) => "User, 32-bit",
        (true, false) => "User",
        (false, true) => "Machine, 32-bit",
        (false, false) => "Machine",
    }
}

impl RowSource for Rows<'_> {
    fn len(&self) -> usize {
        self.apps.len()
    }

    fn id(&self, row: usize) -> RowId {
        let a = &self.apps[row];
        page_row_id(
            5,
            hash_str(&a.name)
                ^ hash_str(a.version.as_deref().unwrap_or("")).rotate_left(23)
                ^ u64::from(a.per_user) << 1
                ^ u64::from(a.x86),
        )
    }

    fn cell(&self, row: usize, col: usize, out: &mut String) {
        let a = &self.apps[row];
        out.clear();
        match col {
            col::NAME => out.push_str(&a.name),
            col::PUBLISHER => out.push_str(a.publisher.as_deref().unwrap_or("")),
            col::VERSION => out.push_str(a.version.as_deref().unwrap_or("")),
            col::INSTALLED => out.push_str(a.installed_on.as_deref().unwrap_or("")),
            col::SIZE => {
                if let Some(s) = a.size {
                    format::bytes(out, s);
                }
            }
            col::SCOPE => out.push_str(scope(a)),
            col::LOCATION => out.push_str(a.location.as_deref().unwrap_or("")),
            _ => {}
        }
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        let (x, y) = (&self.apps[a], &self.apps[b]);
        let ci = |p: &str, q: &str| p.to_ascii_lowercase().cmp(&q.to_ascii_lowercase());
        match col {
            col::PUBLISHER => x.publisher.cmp(&y.publisher),
            col::VERSION => x.version.cmp(&y.version),
            col::INSTALLED => x.installed_on.cmp(&y.installed_on),
            col::SIZE => x.size.cmp(&y.size),
            col::SCOPE => scope(x).cmp(scope(y)),
            col::LOCATION => x.location.cmp(&y.location),
            _ => ci(&x.name, &y.name),
        }
    }

    fn visible(&self, row: usize) -> bool {
        self.matched.get(row).copied().unwrap_or(true)
    }
}

impl AppsPage {
    fn rows(&self) -> Rows<'_> {
        Rows {
            apps: self.apps.as_deref().unwrap_or(&[]),
            matched: &self.matched,
        }
    }

    #[must_use]
    pub fn query(&self) -> Option<Query> {
        self.apps.is_none().then_some(Query::InstalledApps)
    }

    pub fn set_apps(&mut self, apps: Vec<InstalledApp>) {
        self.apps = Some(apps);
        self.refilter();
        self.list.table.refresh();
    }

    fn refilter(&mut self) {
        let needle = &self.list.search.needle;
        self.matched.clear();
        if let Some(apps) = &self.apps {
            self.matched.extend(apps.iter().map(|a| {
                any_matches(
                    needle,
                    [a.name.as_str()]
                        .into_iter()
                        .chain(a.publisher.as_deref())
                        .chain(a.version.as_deref())
                        .chain(a.location.as_deref()),
                )
            }));
        }
        self.list.table.invalidate_order();
    }

    /// Whether the search field has the keyboard.
    #[must_use]
    pub fn search_focused(&self) -> bool {
        self.list.search.focused
    }

    #[must_use]
    pub fn cursor(&self) -> Cursor {
        if self.list.over_divider() {
            Cursor::ResizeColumn
        } else if self.list.over_text_field() {
            Cursor::Text
        } else {
            Cursor::Arrow
        }
    }

    pub fn handle(&mut self, ev: UiEvent, theme: &Theme) -> PageOutcome {
        let outcome = {
            let rows = Rows {
                apps: self.apps.as_deref().unwrap_or(&[]),
                matched: &self.matched,
            };
            self.list.handle(ev, &rows, theme)
        };
        match outcome {
            ListOutcome::Nothing => Reaction::NONE.into(),
            ListOutcome::Repaint | ListOutcome::Button(_) => Reaction::REPAINT.into(),
            ListOutcome::SearchChanged => {
                self.refilter();
                Reaction::REPAINT.into()
            }
            ListOutcome::Menu(at) => self.menu(at).into(),
        }
    }

    fn menu(&self, at: Point) -> Reaction {
        let rows = self.rows();
        let Some(row) = self.list.selected(&rows) else {
            return Reaction::NONE;
        };
        let a = &rows.apps[row];
        let entries = vec![
            MenuEntry::item(MenuAction::Uninstall, "Uninstall", a.uninstall.is_some()),
            MenuEntry::item(
                MenuAction::OpenInstallLocation,
                "Open install location",
                a.location.is_some() || a.uninstall.is_some(),
            ),
            MenuEntry::Separator,
            MenuEntry::item(MenuAction::SearchOnline, "Search online", true),
            MenuEntry::item(MenuAction::Copy, "Copy", true),
        ];
        Reaction::effect(Effect::Menu { at, entries })
    }

    pub fn menu_action(&mut self, action: MenuAction) -> PageOutcome {
        let rows = self.rows();
        let Some(row) = self.list.selected(&rows) else {
            return Reaction::NONE.into();
        };
        let a = &rows.apps[row];
        let r = match action {
            MenuAction::Uninstall => match &a.uninstall {
                Some(command) => Reaction::effect(Effect::Uninstall {
                    name: a.name.clone(),
                    command: command.clone(),
                }),
                None => Reaction::NONE,
            },
            MenuAction::OpenInstallLocation => {
                // The shell works the folder out: the location when it is
                // recorded, else the uninstaller's folder.
                if a.location.is_none() && a.uninstall.is_none() {
                    Reaction::NONE
                } else {
                    Reaction::effect(Effect::OpenInstallLocation {
                        location: a.location.clone(),
                        uninstall: a.uninstall.clone(),
                    })
                }
            }
            MenuAction::SearchOnline => {
                let mut q = a.name.clone();
                if let Some(p) = &a.publisher {
                    q.push(' ');
                    q.push_str(p);
                }
                Reaction::effect(Effect::OpenUrl(search_url(&q)))
            }
            MenuAction::Copy => {
                Reaction::effect(Effect::CopyText(copy_row(&self.list, &rows, row)))
            }
            _ => Reaction::NONE,
        };
        r.into()
    }

    pub fn paint(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme, buf: &mut String) {
        let rows = Rows {
            apps: self.apps.as_deref().unwrap_or(&[]),
            matched: &self.matched,
        };
        match &self.apps {
            None => {
                self.buf.clear();
                self.buf.push_str("Reading\u{2026}");
            }
            Some(apps) => {
                let listed = self.matched.iter().filter(|&&m| m).count();
                count_text(&mut self.buf, listed, apps.len(), "programs");
            }
        }
        self.list.paint(dl, rect, &rows, &self.buf, theme, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Key;
    use ot_model::Bytes;
    use ot_paint::{DrawCmd, Size};

    fn app(name: &str, per_user: bool) -> InstalledApp {
        InstalledApp {
            name: name.to_owned(),
            publisher: Some("Acme".to_owned()),
            version: Some("1.2".to_owned()),
            installed_on: Some("2026-09-01".to_owned()),
            size: Some(Bytes(50 << 20)),
            location: Some(format!("C:\\Program Files\\{name}")),
            uninstall: Some(format!("\"C:\\Program Files\\{name}\\unins.exe\"")),
            per_user,
            x86: false,
        }
    }

    fn texts(page: &mut AppsPage) -> Vec<String> {
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        page.paint(
            &mut dl,
            Rect::from_size(Size::new(1100.0, 600.0)),
            &Theme::dark(),
            &mut buf,
        );
        dl.cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text(t) => Some(dl.str(t.text).to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn lists_programs_and_offers_uninstall() {
        let mut page = AppsPage::default();
        assert_eq!(page.query(), Some(Query::InstalledApps));
        page.set_apps(vec![app("Zed", false), app("Audacity", true)]);
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "2 programs"), "{t:?}");
        assert!(t.iter().any(|s| s == "50.0 MB"), "{t:?}");
        assert!(t.iter().any(|s| s == "User"), "{t:?}");
        let theme = Theme::dark();
        page.handle(UiEvent::Key(Key::Down), &theme);
        let PageOutcome::Reaction(r) = page.menu_action(MenuAction::Uninstall) else {
            panic!()
        };
        assert!(matches!(
            r.effect,
            Some(Effect::Uninstall { ref name, .. }) if name == "Audacity"
        ));
    }
}
