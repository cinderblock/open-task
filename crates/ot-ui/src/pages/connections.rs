//! The Connections page: every TCP and UDP endpoint, with the process that owns
//! it, as TMOG's Connections view and `netstat -ano` show them.
//!
//! The list is read on demand and again every couple of seconds while the page
//! shows (the shell paces that); the process names come from the latest snapshot.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use ot_core::Snapshot;
use ot_model::connection::{Connection, TcpState};
use ot_paint::{DisplayList, Point, Rect};

use super::{count_text, PageOutcome};
use crate::format;
use crate::list_page::{any_matches, page_row_id, ListOutcome, ListPage};
use crate::pages::services::copy_row;
use crate::table::{Column, RowId, RowSource};
use crate::theme::Theme;
use crate::view::{Cursor, Effect, MenuAction, MenuEntry, Query, Reaction, UiEvent};

mod col {
    pub const PROTOCOL: usize = 0;
    pub const LOCAL: usize = 1;
    pub const LOCAL_PORT: usize = 2;
    pub const REMOTE: usize = 3;
    pub const REMOTE_PORT: usize = 4;
    pub const STATE: usize = 5;
    pub const PID: usize = 6;
    pub const PROCESS: usize = 7;
}

fn columns() -> Vec<Column> {
    vec![
        Column::text("Protocol", 75.0),
        Column::text("Local address", 170.0),
        Column::number("Port", 60.0),
        Column::text("Remote address", 170.0),
        Column::number("Port", 60.0),
        Column::text("State", 100.0),
        Column::number("PID", 70.0),
        Column::text("Process", 220.0),
    ]
}

#[derive(Debug)]
pub(crate) struct ConnectionsPage {
    list: ListPage,
    connections: Option<Vec<Connection>>,
    /// PID to process name, from the latest snapshot.
    names: HashMap<u32, Arc<str>>,
    matched: Vec<bool>,
    buf: String,
}

impl Default for ConnectionsPage {
    fn default() -> Self {
        let mut list = ListPage::new("Connections", columns(), col::PROCESS);
        list.table.sort_desc = false;
        Self {
            list,
            connections: None,
            names: HashMap::new(),
            matched: Vec::new(),
            buf: String::new(),
        }
    }
}

struct Rows<'a> {
    connections: &'a [Connection],
    names: &'a HashMap<u32, Arc<str>>,
    matched: &'a [bool],
}

fn addr_text(out: &mut String, a: Option<SocketAddr>) {
    out.clear();
    if let Some(a) = a {
        let _ = write!(out, "{}", a.ip());
    }
}

impl Rows<'_> {
    fn name(&self, pid: u32) -> &str {
        self.names.get(&pid).map_or("", |n| n.as_ref())
    }
}

impl RowSource for Rows<'_> {
    fn len(&self) -> usize {
        self.connections.len()
    }

    fn id(&self, row: usize) -> RowId {
        let c = &self.connections[row];
        let mut h = u64::from(c.local.port()) | (u64::from(c.pid) << 16);
        h ^= (c.protocol as u64) << 48;
        if let Some(r) = c.remote {
            h ^= u64::from(r.port()).rotate_left(32);
            if let std::net::IpAddr::V4(ip) = r.ip() {
                h ^= u64::from(u32::from(ip)).rotate_left(20);
            }
        }
        page_row_id(4, h)
    }

    fn cell(&self, row: usize, col: usize, out: &mut String) {
        let c = &self.connections[row];
        match col {
            col::PROTOCOL => {
                out.clear();
                out.push_str(c.protocol.label());
            }
            col::LOCAL => addr_text(out, Some(c.local)),
            col::LOCAL_PORT => format::count(out, u32::from(c.local.port())),
            col::REMOTE => addr_text(out, c.remote),
            col::REMOTE_PORT => {
                out.clear();
                if let Some(r) = c.remote {
                    format::count(out, u32::from(r.port()));
                }
            }
            col::STATE => {
                out.clear();
                out.push_str(c.state.label());
            }
            col::PID => format::count(out, c.pid),
            col::PROCESS => {
                out.clear();
                out.push_str(self.name(c.pid));
            }
            _ => out.clear(),
        }
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        let (x, y) = (&self.connections[a], &self.connections[b]);
        match col {
            col::PROTOCOL => x.protocol.label().cmp(y.protocol.label()),
            col::LOCAL => x.local.ip().cmp(&y.local.ip()),
            col::LOCAL_PORT => x.local.port().cmp(&y.local.port()),
            col::REMOTE => x.remote.map(|r| r.ip()).cmp(&y.remote.map(|r| r.ip())),
            col::REMOTE_PORT => x.remote.map(|r| r.port()).cmp(&y.remote.map(|r| r.port())),
            col::STATE => x.state.label().cmp(y.state.label()),
            col::PID => x.pid.cmp(&y.pid),
            _ => self
                .name(x.pid)
                .to_ascii_lowercase()
                .cmp(&self.name(y.pid).to_ascii_lowercase())
                .then_with(|| x.pid.cmp(&y.pid)),
        }
    }

    fn visible(&self, row: usize) -> bool {
        self.matched.get(row).copied().unwrap_or(true)
    }
}

impl ConnectionsPage {
    fn rows(&self) -> Rows<'_> {
        Rows {
            connections: self.connections.as_deref().unwrap_or(&[]),
            names: &self.names,
            matched: &self.matched,
        }
    }

    /// The page always wants a fresh list when shown. The shape matches the
    /// other pages' `query`, which only ask when their list is stale.
    #[must_use]
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub fn query(&self) -> Option<Query> {
        Some(Query::Connections)
    }

    pub fn set_connections(&mut self, connections: Vec<Connection>) {
        self.connections = Some(connections);
        self.refilter();
        self.list.table.refresh();
    }

    pub fn set_snapshot(&mut self, snap: &Snapshot) {
        // Names are interned per process; a steady state re-inserts pointers.
        self.names.clear();
        for p in &snap.processes {
            self.names
                .entry(p.key().pid)
                .or_insert_with(|| Arc::from(p.name()));
        }
    }

    fn refilter(&mut self) {
        let needle = &self.list.search.needle;
        self.matched.clear();
        let names = &self.names;
        if let Some(cs) = &self.connections {
            let mut scratch = String::new();
            self.matched.extend(cs.iter().map(|c| {
                scratch.clear();
                let _ = write!(scratch, "{} {}", c.local, c.pid);
                if let Some(r) = c.remote {
                    let _ = write!(scratch, " {r}");
                }
                any_matches(
                    needle,
                    [
                        scratch.as_str(),
                        c.protocol.label(),
                        c.state.label(),
                        names.get(&c.pid).map_or("", |n| n.as_ref()),
                    ],
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
                connections: self.connections.as_deref().unwrap_or(&[]),
                names: &self.names,
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
        let c = &rows.connections[row];
        let known = c.pid != 0 && self.names.contains_key(&c.pid);
        let entries = vec![
            MenuEntry::item(MenuAction::GoToProcess, "Go to process", known),
            MenuEntry::item(MenuAction::EndTask, "End process", known && c.pid > 4),
            MenuEntry::Separator,
            MenuEntry::item(MenuAction::Copy, "Copy", true),
        ];
        Reaction::effect(Effect::Menu { at, entries })
    }

    pub fn menu_action(&mut self, action: MenuAction, snap: &Snapshot) -> PageOutcome {
        let rows = self.rows();
        let Some(row) = self.list.selected(&rows) else {
            return Reaction::NONE.into();
        };
        let c = &rows.connections[row];
        match action {
            MenuAction::GoToProcess => PageOutcome::SelectPid(c.pid),
            MenuAction::EndTask => {
                let Some(p) = snap.processes.iter().find(|p| p.key().pid == c.pid) else {
                    return Reaction::NONE.into();
                };
                Reaction::effect(Effect::Terminate {
                    targets: vec![p.key()],
                    label: p.name().to_owned(),
                })
                .into()
            }
            MenuAction::Copy => {
                Reaction::effect(Effect::CopyText(copy_row(&self.list, &rows, row))).into()
            }
            _ => Reaction::NONE.into(),
        }
    }

    pub fn paint(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme, buf: &mut String) {
        let rows = Rows {
            connections: self.connections.as_deref().unwrap_or(&[]),
            names: &self.names,
            matched: &self.matched,
        };
        match &self.connections {
            None => {
                self.buf.clear();
                self.buf.push_str("Reading\u{2026}");
            }
            Some(cs) => {
                let listed = self.matched.iter().filter(|&&m| m).count();
                let established = cs
                    .iter()
                    .filter(|c| c.state == TcpState::Established)
                    .count();
                count_text(&mut self.buf, listed, cs.len(), "endpoints");
                let _ = write!(self.buf, ", {established} established");
            }
        }
        self.list.paint(dl, rect, &rows, &self.buf, theme, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_rows::tests::proc;
    use crate::view::Key;
    use ot_model::connection::Protocol;
    use ot_paint::{DrawCmd, Size};

    fn conn(pid: u32, port: u16, remote: Option<&str>) -> Connection {
        Connection {
            protocol: Protocol::Tcp,
            local: format!("127.0.0.1:{port}").parse().unwrap(),
            remote: remote.map(|r| r.parse().unwrap()),
            state: if remote.is_some() {
                TcpState::Established
            } else {
                TcpState::Listen
            },
            pid,
        }
    }

    fn texts(page: &mut ConnectionsPage) -> Vec<String> {
        let mut dl = DisplayList::new();
        let mut buf = String::new();
        page.paint(
            &mut dl,
            Rect::from_size(Size::new(1000.0, 600.0)),
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
    fn names_processes_counts_established_and_jumps() {
        let mut page = ConnectionsPage::default();
        let snap = Snapshot {
            processes: vec![proc(10, None, 0.0), proc(20, None, 0.0)],
            ..Snapshot::default()
        };
        page.set_snapshot(&snap);
        page.set_connections(vec![
            conn(10, 80, None),
            conn(20, 50000, Some("1.2.3.4:443")),
        ]);
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "2 endpoints, 1 established"), "{t:?}");
        assert!(t.iter().any(|s| s == "p10.exe"), "{t:?}");
        assert!(t.iter().any(|s| s == "1.2.3.4"), "{t:?}");
        assert!(t.iter().any(|s| s == "Listening"), "{t:?}");

        let theme = Theme::dark();
        page.handle(UiEvent::Key(Key::Down), &theme);
        assert_eq!(
            page.menu_action(MenuAction::GoToProcess, &snap),
            PageOutcome::SelectPid(10)
        );
        let PageOutcome::Reaction(r) = page.menu_action(MenuAction::EndTask, &snap) else {
            panic!()
        };
        assert!(
            matches!(r.effect, Some(Effect::Terminate { ref label, .. }) if label == "p10.exe")
        );
        for c in "443".chars() {
            page.handle(UiEvent::Char(c), &theme);
        }
        let t = texts(&mut page);
        assert!(
            t.iter().any(|s| s == "1 of 2 endpoints, 1 established"),
            "{t:?}"
        );
        assert_eq!(page.query(), Some(Query::Connections));
    }
}
