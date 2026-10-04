//! The Startup page: what runs at sign-in, from the Run keys and the Startup
//! folders, enabled or not, with a switch for each, as Task Manager's Startup
//! apps page has it.
//!
//! The list is read on demand (it means opening shortcuts and reading version
//! resources) when the page is shown and again after a change.

use std::cmp::Ordering;

use ot_model::startup::StartupEntry;
use ot_paint::{DisplayList, Point, Rect};

use super::{count_text, PageOutcome};
use crate::list_page::{any_matches, page_row_id, ListOutcome, ListPage};
use crate::pages::services::copy_row;
use crate::process_rows::hash_str;
use crate::table::{Column, RowId, RowSource};
use crate::theme::Theme;
use crate::view::{search_url, Cursor, Effect, MenuAction, MenuEntry, Query, Reaction, UiEvent};

mod col {
    pub const NAME: usize = 0;
    pub const PUBLISHER: usize = 1;
    pub const STATUS: usize = 2;
    pub const COMMAND: usize = 3;
    pub const LOCATION: usize = 4;
}

fn columns() -> Vec<Column> {
    vec![
        Column::text("Name", 220.0),
        Column::text("Publisher", 180.0),
        Column::text("Status", 90.0),
        Column::text("Command", 420.0),
        Column::text("Location", 200.0),
    ]
}

#[derive(Debug)]
pub(crate) struct StartupPage {
    list: ListPage,
    /// `None` until the first read arrives.
    entries: Option<Vec<StartupEntry>>,
    matched: Vec<bool>,
    buf: String,
}

impl Default for StartupPage {
    fn default() -> Self {
        let mut list = ListPage::new("Startup apps", columns(), col::NAME);
        list.table.sort_desc = false;
        Self {
            list,
            entries: None,
            matched: Vec::new(),
            buf: String::new(),
        }
    }
}

struct Rows<'a> {
    entries: &'a [StartupEntry],
    matched: &'a [bool],
}

impl RowSource for Rows<'_> {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn id(&self, row: usize) -> RowId {
        let e = &self.entries[row];
        page_row_id(
            3,
            hash_str(&e.name) ^ hash_str(e.location.label()).rotate_left(17),
        )
    }

    fn images(&self) -> bool {
        true
    }

    fn image(&self, row: usize) -> Option<&str> {
        self.entries[row].image_path.as_deref()
    }

    fn cell(&self, row: usize, col: usize, out: &mut String) {
        let e = &self.entries[row];
        out.clear();
        match col {
            col::NAME => out.push_str(&e.name),
            col::PUBLISHER => out.push_str(e.publisher.as_deref().unwrap_or("")),
            col::STATUS => out.push_str(if e.enabled { "Enabled" } else { "Disabled" }),
            col::COMMAND => out.push_str(&e.command),
            col::LOCATION => out.push_str(e.location.label()),
            _ => {}
        }
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        let (x, y) = (&self.entries[a], &self.entries[b]);
        let ci = |p: &str, q: &str| p.to_ascii_lowercase().cmp(&q.to_ascii_lowercase());
        match col {
            col::PUBLISHER => x.publisher.cmp(&y.publisher),
            col::STATUS => y.enabled.cmp(&x.enabled),
            col::COMMAND => ci(&x.command, &y.command),
            col::LOCATION => ci(x.location.label(), y.location.label()),
            _ => ci(&x.name, &y.name),
        }
    }

    fn visible(&self, row: usize) -> bool {
        self.matched.get(row).copied().unwrap_or(true)
    }
}

impl StartupPage {
    fn rows(&self) -> Rows<'_> {
        Rows {
            entries: self.entries.as_deref().unwrap_or(&[]),
            matched: &self.matched,
        }
    }

    /// What the page needs read, if anything.
    #[must_use]
    pub fn query(&self) -> Option<Query> {
        self.entries.is_none().then_some(Query::Startup)
    }

    pub fn set_entries(&mut self, entries: Vec<StartupEntry>) {
        self.entries = Some(entries);
        self.refilter();
        self.list.table.refresh();
    }

    fn refilter(&mut self) {
        let needle = &self.list.search.needle;
        self.matched.clear();
        if let Some(entries) = &self.entries {
            self.matched.extend(entries.iter().map(|e| {
                any_matches(
                    needle,
                    [e.name.as_str(), e.command.as_str(), e.location.label()]
                        .into_iter()
                        .chain(e.publisher.as_deref()),
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
                entries: self.entries.as_deref().unwrap_or(&[]),
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
        let e = &rows.entries[row];
        let has_file = e.image_path.is_some();
        let entries = vec![
            if e.enabled {
                MenuEntry::item(MenuAction::StartupEnable(false), "Disable", true)
            } else {
                MenuEntry::item(MenuAction::StartupEnable(true), "Enable", true)
            },
            MenuEntry::Separator,
            MenuEntry::item(MenuAction::OpenFileLocation, "Open file location", has_file),
            MenuEntry::item(MenuAction::SearchOnline, "Search online", true),
            MenuEntry::item(MenuAction::Properties, "Properties", has_file),
            MenuEntry::item(MenuAction::Copy, "Copy", true),
        ];
        Reaction::effect(Effect::Menu { at, entries })
    }

    pub fn menu_action(&mut self, action: MenuAction) -> PageOutcome {
        let rows = self.rows();
        let Some(row) = self.list.selected(&rows) else {
            return Reaction::NONE.into();
        };
        let e = &rows.entries[row];
        let r = match action {
            MenuAction::StartupEnable(on) => Reaction::effect(Effect::Startup {
                entry: e.clone(),
                on,
            }),
            MenuAction::OpenFileLocation => match &e.image_path {
                Some(p) => Reaction::effect(Effect::OpenFileLocation(p.clone())),
                None => Reaction::NONE,
            },
            MenuAction::Properties => match &e.image_path {
                Some(p) => Reaction::effect(Effect::Properties(p.clone())),
                None => Reaction::NONE,
            },
            MenuAction::SearchOnline => {
                let mut q = e.name.clone();
                if let Some(p) = &e.publisher {
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
            entries: self.entries.as_deref().unwrap_or(&[]),
            matched: &self.matched,
        };
        match &self.entries {
            None => {
                self.buf.clear();
                self.buf.push_str("Reading\u{2026}");
            }
            Some(entries) => {
                let listed = self.matched.iter().filter(|&&m| m).count();
                count_text(&mut self.buf, listed, entries.len(), "entries");
            }
        }
        self.list.paint(dl, rect, &rows, &self.buf, theme, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Key;
    use ot_model::startup::StartupLocation;
    use ot_paint::{DrawCmd, Size};

    fn entry(name: &str, enabled: bool) -> StartupEntry {
        StartupEntry {
            name: name.to_owned(),
            location: StartupLocation::UserRun,
            command: format!("C:\\{name}.exe --tray"),
            enabled,
            publisher: Some("Acme".to_owned()),
            image_path: Some(format!("C:\\{name}.exe")),
        }
    }

    fn texts(page: &mut StartupPage) -> Vec<String> {
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        page.paint(
            &mut dl,
            Rect::from_size(Size::new(900.0, 600.0)),
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
    fn entries_draw_their_program_icons() {
        let mut page = StartupPage::default();
        page.set_entries(vec![entry("OneDrive", true), entry("Discord", false)]);
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        page.paint(
            &mut dl,
            Rect::from_size(Size::new(900.0, 600.0)),
            &Theme::dark(),
            &mut buf,
        );
        let images: Vec<String> = dl
            .cmds()
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Image { path, .. } => Some(dl.str(*path).to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 2, "{images:?}");
        assert!(images.iter().any(|p| p == "C:\\OneDrive.exe"), "{images:?}");
    }

    #[test]
    fn asks_for_its_list_once_and_toggles_entries() {
        let mut page = StartupPage::default();
        assert_eq!(page.query(), Some(Query::Startup));
        assert!(texts(&mut page).iter().any(|s| s == "Reading\u{2026}"));
        page.set_entries(vec![entry("OneDrive", true), entry("Discord", false)]);
        assert_eq!(page.query(), None);
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "2 entries"), "{t:?}");
        assert!(t.iter().any(|s| s == "Disabled"), "{t:?}");

        let theme = Theme::dark();
        page.handle(UiEvent::Key(Key::Down), &theme); // Discord, A to Z
        let PageOutcome::Reaction(r) = page.handle(UiEvent::ContextMenu { at: None }, &theme)
        else {
            panic!()
        };
        let Some(Effect::Menu { entries, .. }) = r.effect else {
            panic!("{r:?}");
        };
        assert!(matches!(
            entries[0],
            MenuEntry::Item {
                action: MenuAction::StartupEnable(true),
                ..
            }
        ));
        let PageOutcome::Reaction(r) = page.menu_action(MenuAction::StartupEnable(true)) else {
            panic!()
        };
        assert!(matches!(
            r.effect,
            Some(Effect::Startup { entry, on: true }) if entry.name == "Discord"
        ));
        let PageOutcome::Reaction(r) = page.menu_action(MenuAction::OpenFileLocation) else {
            panic!()
        };
        assert_eq!(
            r.effect,
            Some(Effect::OpenFileLocation("C:\\Discord.exe".to_owned()))
        );
    }
}
