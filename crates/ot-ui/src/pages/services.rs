//! The Services page: every service of the machine, running or not, with the
//! process hosting it, when it starts and what it is for; start, stop and
//! restart, and a jump to the host process.
//!
//! The list comes with every snapshot (the probe republishes it by pointer while
//! nothing changes), so the page is as live as the process table. Starting and
//! stopping need administrator rights; without them the actions are shown but
//! disabled, and the Settings page says how to get them.

use std::cmp::Ordering;
use std::sync::Arc;

use ot_core::Snapshot;
use ot_model::service::{ServiceEntry, ServiceState};
use ot_paint::{DisplayList, Point, Rect};

use super::{count_text, PageOutcome};
use crate::format;
use crate::list_page::{any_matches, page_row_id, ListOutcome, ListPage, PageButton};
use crate::process_rows::hash_str;
use crate::table::{Column, RowId, RowSource};
use crate::theme::Theme;
use crate::view::{
    search_url, Cursor, Effect, MenuAction, MenuEntry, Reaction, ServiceAction, UiEvent,
};

mod col {
    pub const NAME: usize = 0;
    pub const PID: usize = 1;
    pub const DESCRIPTION: usize = 2;
    pub const STATUS: usize = 3;
    pub const START: usize = 4;
    pub const GROUP: usize = 5;
    pub const DETAILS: usize = 6;
}

fn columns() -> Vec<Column> {
    vec![
        Column::text("Name", 200.0),
        Column::number("PID", 70.0),
        Column::text("Description", 320.0),
        Column::text("Status", 90.0),
        Column::text("Start type", 130.0),
        Column::text("Group", 120.0),
        Column::text("Details", 400.0).hidden(),
    ]
}

#[derive(Debug)]
pub(crate) struct ServicesPage {
    list: ListPage,
    services: Arc<[ServiceEntry]>,
    matched: Vec<bool>,
    buf: String,
}

impl Default for ServicesPage {
    fn default() -> Self {
        let mut list = ListPage::new("Services", columns(), col::NAME);
        list.table.sort_desc = false;
        list.buttons.push(PageButton {
            label: "Open Services",
            width: 110.0,
            enabled: true,
        });
        Self {
            list,
            services: Vec::new().into(),
            matched: Vec::new(),
            buf: String::new(),
        }
    }
}

struct Rows<'a> {
    services: &'a [ServiceEntry],
    matched: &'a [bool],
}

impl RowSource for Rows<'_> {
    fn len(&self) -> usize {
        self.services.len()
    }

    fn id(&self, row: usize) -> RowId {
        page_row_id(2, hash_str(&self.services[row].name))
    }

    fn cell(&self, row: usize, col: usize, out: &mut String) {
        let s = &self.services[row];
        out.clear();
        match col {
            col::NAME => out.push_str(&s.name),
            col::PID => {
                if let Some(pid) = s.pid {
                    format::count(out, pid);
                }
            }
            col::DESCRIPTION => out.push_str(&s.display_name),
            col::STATUS => out.push_str(s.state.label()),
            col::START => out.push_str(s.start.label()),
            col::GROUP => out.push_str(s.group.as_deref().unwrap_or("")),
            col::DETAILS => out.push_str(s.description.as_deref().unwrap_or("")),
            _ => {}
        }
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        let (x, y) = (&self.services[a], &self.services[b]);
        let ci = |p: &str, q: &str| p.to_ascii_lowercase().cmp(&q.to_ascii_lowercase());
        match col {
            col::PID => x.pid.cmp(&y.pid),
            col::DESCRIPTION => ci(&x.display_name, &y.display_name),
            col::STATUS => ci(x.state.label(), y.state.label()),
            col::START => ci(x.start.label(), y.start.label()),
            col::GROUP => x.group.cmp(&y.group),
            col::DETAILS => x.description.cmp(&y.description),
            _ => ci(&x.name, &y.name),
        }
    }

    fn visible(&self, row: usize) -> bool {
        self.matched.get(row).copied().unwrap_or(true)
    }
}

impl ServicesPage {
    fn rows(&self) -> Rows<'_> {
        Rows {
            services: &self.services,
            matched: &self.matched,
        }
    }

    pub fn set_snapshot(&mut self, snap: &Snapshot) {
        if Arc::ptr_eq(&self.services, &snap.services) {
            return;
        }
        self.services = Arc::clone(&snap.services);
        self.refilter();
        self.list.table.refresh();
    }

    fn refilter(&mut self) {
        let needle = &self.list.search.needle;
        self.matched.clear();
        self.matched.extend(self.services.iter().map(|s| {
            any_matches(
                needle,
                [s.name.as_ref(), s.display_name.as_ref()]
                    .into_iter()
                    .chain(s.description.as_deref())
                    .chain(s.group.as_deref()),
            )
        }));
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

    pub fn handle(&mut self, ev: UiEvent, theme: &Theme, elevated: bool) -> PageOutcome {
        let outcome = {
            let rows = Rows {
                services: &self.services,
                matched: &self.matched,
            };
            self.list.handle(ev, &rows, theme)
        };
        match outcome {
            ListOutcome::Nothing => Reaction::NONE.into(),
            ListOutcome::Repaint => Reaction::REPAINT.into(),
            ListOutcome::SearchChanged => {
                self.refilter();
                Reaction::REPAINT.into()
            }
            ListOutcome::Menu(at) => self.menu(at, elevated).into(),
            ListOutcome::Button(_) => Reaction::effect(Effect::OpenServices).into(),
        }
    }

    fn menu(&self, at: Point, elevated: bool) -> Reaction {
        let rows = self.rows();
        let Some(row) = self.list.selected(&rows) else {
            return Reaction::NONE;
        };
        let s = &self.services[row];
        let running = s.state == ServiceState::Running;
        let stopped = s.state == ServiceState::Stopped;
        let entries = vec![
            MenuEntry::item(MenuAction::ServiceStart, "Start", elevated && stopped),
            MenuEntry::item(
                MenuAction::ServiceStop,
                "Stop",
                elevated && running && s.can_stop,
            ),
            MenuEntry::item(
                MenuAction::ServiceRestart,
                "Restart",
                elevated && running && s.can_stop,
            ),
            MenuEntry::Separator,
            MenuEntry::item(MenuAction::GoToProcess, "Go to process", s.pid.is_some()),
            MenuEntry::item(MenuAction::OpenServices, "Open Services", true),
            MenuEntry::item(MenuAction::SearchOnline, "Search online", true),
            MenuEntry::item(MenuAction::Copy, "Copy", true),
        ];
        Reaction::effect(Effect::Menu { at, entries })
    }

    pub fn menu_action(&mut self, action: MenuAction) -> PageOutcome {
        if action == MenuAction::OpenServices {
            return Reaction::effect(Effect::OpenServices).into();
        }
        let rows = self.rows();
        let Some(row) = self.list.selected(&rows) else {
            return Reaction::NONE.into();
        };
        let s = &self.services[row];
        let service = |action: ServiceAction| {
            Reaction::effect(Effect::Service {
                name: s.name.to_string(),
                display_name: s.display_name.to_string(),
                action,
            })
            .into()
        };
        match action {
            MenuAction::ServiceStart => service(ServiceAction::Start),
            MenuAction::ServiceStop => service(ServiceAction::Stop),
            MenuAction::ServiceRestart => service(ServiceAction::Restart),
            MenuAction::GoToProcess => match s.pid {
                Some(pid) => PageOutcome::SelectPid(pid),
                None => Reaction::NONE.into(),
            },
            MenuAction::SearchOnline => Reaction::effect(Effect::OpenUrl(search_url(&format!(
                "{} {}",
                s.name, s.display_name
            ))))
            .into(),
            MenuAction::Copy => {
                Reaction::effect(Effect::CopyText(copy_row(&self.list, &rows, row))).into()
            }
            _ => Reaction::NONE.into(),
        }
    }

    pub fn paint(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme, buf: &mut String) {
        let rows = Rows {
            services: &self.services,
            matched: &self.matched,
        };
        let listed = self.matched.iter().filter(|&&m| m).count();
        count_text(&mut self.buf, listed, self.services.len(), "services");
        self.list.paint(dl, rect, &rows, &self.buf, theme, buf);
    }
}

/// The selected row's visible cells, tab-separated.
pub(crate) fn copy_row<S: RowSource>(list: &ListPage, rows: &S, row: usize) -> String {
    let mut text = String::new();
    let mut cell = String::new();
    for (i, (ci, _)) in list.table.shown().enumerate() {
        rows.cell(row, ci, &mut cell);
        if i > 0 {
            text.push('\t');
        }
        text.push_str(&cell);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Key;
    use ot_model::service::StartType;
    use ot_paint::{DrawCmd, Size};

    fn entry(name: &str, state: ServiceState, pid: Option<u32>) -> ServiceEntry {
        ServiceEntry {
            name: Arc::from(name),
            display_name: Arc::from(format!("{name} Service").as_str()),
            description: Some(Arc::from("Does things.")),
            state,
            start: StartType::Manual,
            pid,
            group: None,
            can_stop: true,
        }
    }

    fn snap(services: Vec<ServiceEntry>) -> Snapshot {
        Snapshot {
            services: services.into(),
            ..Snapshot::default()
        }
    }

    fn texts(page: &mut ServicesPage) -> Vec<String> {
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
    fn lists_sorted_by_name_filters_and_offers_the_right_actions() {
        let mut page = ServicesPage::default();
        page.set_snapshot(&snap(vec![
            entry("Spooler", ServiceState::Running, Some(1234)),
            entry("BITS", ServiceState::Stopped, None),
        ]));
        let t = texts(&mut page);
        let bits = t.iter().position(|s| s == "BITS").unwrap();
        let spooler = t.iter().position(|s| s == "Spooler").unwrap();
        assert!(bits < spooler, "A to Z: {t:?}");
        assert!(t.iter().any(|s| s == "2 services"));

        let theme = Theme::dark();
        page.handle(UiEvent::Key(Key::Down), &theme, true);
        page.handle(UiEvent::Key(Key::Down), &theme, true);
        let PageOutcome::Reaction(r) = page.handle(UiEvent::ContextMenu { at: None }, &theme, true)
        else {
            panic!()
        };
        let Some(Effect::Menu { entries, .. }) = r.effect else {
            panic!("{r:?}");
        };
        // Spooler runs: Stop and Restart on, Start off; it has a PID to go to.
        let enabled = |label: &str| {
            entries
                .iter()
                .any(|e| matches!(e, MenuEntry::Item { label: l, enabled: true, .. } if l == label))
        };
        assert!(enabled("Stop") && enabled("Restart") && !enabled("Start"));
        assert!(enabled("Go to process"));
        assert_eq!(
            page.menu_action(MenuAction::GoToProcess),
            PageOutcome::SelectPid(1234)
        );
        let PageOutcome::Reaction(r) = page.menu_action(MenuAction::ServiceStop) else {
            panic!()
        };
        assert_eq!(
            r.effect,
            Some(Effect::Service {
                name: "Spooler".to_owned(),
                display_name: "Spooler Service".to_owned(),
                action: ServiceAction::Stop
            })
        );
        // Unelevated: nothing to start or stop.
        let PageOutcome::Reaction(r) =
            page.handle(UiEvent::ContextMenu { at: None }, &theme, false)
        else {
            panic!()
        };
        let Some(Effect::Menu { entries, .. }) = r.effect else {
            panic!("{r:?}");
        };
        assert!(entries.iter().all(|e| !matches!(
            e,
            MenuEntry::Item {
                action: MenuAction::ServiceStop,
                enabled: true,
                ..
            }
        )));

        for c in "bits".chars() {
            page.handle(UiEvent::Char(c), &theme, true);
        }
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "1 of 2 services"), "{t:?}");
        assert!(!t.iter().any(|s| s == "Spooler"), "{t:?}");
    }
}
