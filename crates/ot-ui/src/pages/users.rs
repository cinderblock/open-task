//! The Users page: who is signed in, what each is using, and their processes.
//!
//! A tree: one row per logon session, folded by default, with the session's
//! processes beneath it, as Task Manager arranges it. A session's numbers are the
//! sums of its processes'. The actions are Task Manager's too: disconnect a
//! session (its programs keep running) or sign it out; and on a process, end it
//! or jump to it on the Processes page.

use std::cmp::Ordering;
use std::sync::Arc;

use ot_core::Snapshot;
use ot_model::session::SessionInfo;
use ot_model::Bytes;
use ot_paint::{DisplayList, Point, Rect};

use super::{count_text, PageOutcome};
use crate::format;
use crate::list_page::{any_matches, page_row_id, ListOutcome, ListPage};
use crate::process_rows::{self, status_label};
use crate::table::{Column, RowId, RowSource};
use crate::theme::Theme;
use crate::view::{Cursor, Effect, MenuAction, MenuEntry, Reaction, SessionAction, UiEvent};

mod col {
    pub const USER: usize = 0;
    pub const ID: usize = 1;
    pub const STATUS: usize = 2;
    pub const CPU: usize = 3;
    pub const MEMORY: usize = 4;
    pub const DISK: usize = 5;
    pub const STATION: usize = 6;
}

fn columns() -> Vec<Column> {
    vec![
        Column::text("User", 260.0),
        Column::number("ID", 60.0),
        Column::text("Status", 110.0),
        Column::number("CPU %", 70.0),
        Column::number("Memory", 95.0),
        Column::number("Disk", 95.0),
        Column::text("Station", 180.0),
    ]
}

/// A session's processes added up.
#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    cpu: f32,
    memory: u64,
    disk: u64,
    processes: u32,
}

#[derive(Debug)]
pub(crate) struct UsersPage {
    list: ListPage,
    snap: Arc<Snapshot>,
    /// Each process's session row, or `u32::MAX`.
    session_of: Vec<u32>,
    totals: Vec<Totals>,
    /// Per row: the session or process matches the search.
    matched: Vec<bool>,
    buf: String,
}

impl Default for UsersPage {
    fn default() -> Self {
        Self {
            list: ListPage::new("Users", columns(), col::CPU),
            snap: Arc::new(Snapshot::default()),
            session_of: Vec::new(),
            totals: Vec::new(),
            matched: Vec::new(),
            buf: String::new(),
        }
    }
}

/// The row source: sessions first, then every process.
struct Rows<'a> {
    snap: &'a Snapshot,
    session_of: &'a [u32],
    totals: &'a [Totals],
    matched: &'a [bool],
    interval_secs: f32,
}

impl Rows<'_> {
    fn sessions(&self) -> usize {
        self.snap.sessions.len()
    }

    fn session(&self, row: usize) -> Option<&SessionInfo> {
        self.snap.sessions.get(row)
    }

    fn process(&self, row: usize) -> Option<&ot_model::process::ProcessSample> {
        row.checked_sub(self.sessions())
            .and_then(|i| self.snap.processes.get(i))
    }

    /// The figure a row sorts by in a numeric column.
    fn number(&self, row: usize, col: usize) -> Option<f64> {
        if let Some(s) = self.session(row) {
            let t = self.totals.get(row).copied().unwrap_or_default();
            return Some(match col {
                col::ID => f64::from(s.id),
                col::CPU => f64::from(t.cpu),
                col::MEMORY => t.memory as f64,
                col::DISK => t.disk as f64,
                _ => return None,
            });
        }
        let p = self.process(row)?;
        Some(match col {
            col::ID => f64::from(p.key().pid),
            col::CPU => f64::from(p.cpu.get()),
            col::MEMORY => p.private_bytes.get() as f64,
            col::DISK => (p.disk_read.get() + p.disk_write.get()) as f64,
            _ => return None,
        })
    }

    fn name(&self, row: usize) -> &str {
        if let Some(s) = self.session(row) {
            return s.user.as_deref().unwrap_or(&s.station);
        }
        self.process(row).map_or("", |p| p.name())
    }
}

fn session_id(s: &SessionInfo) -> RowId {
    page_row_id(1, u64::from(s.id))
}

impl RowSource for Rows<'_> {
    fn len(&self) -> usize {
        self.sessions() + self.snap.processes.len()
    }

    fn id(&self, row: usize) -> RowId {
        match self.session(row) {
            Some(s) => session_id(s),
            None => self
                .process(row)
                .map_or(RowId(0), |p| process_rows::row_id(p.key())),
        }
    }

    fn images(&self) -> bool {
        true
    }

    /// A process row shows its program's icon; a session row shows none.
    fn image(&self, row: usize) -> Option<&str> {
        self.process(row)
            .and_then(|p| p.statics.image_path.as_deref())
    }

    fn cell(&self, row: usize, col: usize, out: &mut String) {
        out.clear();
        if let Some(s) = self.session(row) {
            let t = self.totals.get(row).copied().unwrap_or_default();
            match col {
                col::USER => out.push_str(self.name(row)),
                col::ID => format::count(out, s.id),
                col::STATUS => out.push_str(s.state.label()),
                col::CPU => format::percent(out, t.cpu),
                col::MEMORY => format::bytes(out, Bytes(t.memory)),
                col::DISK => format::rate(out, Bytes(t.disk), self.interval_secs),
                col::STATION => {
                    out.push_str(&s.station);
                    if let Some(c) = &s.client {
                        out.push_str(" \u{b7} ");
                        out.push_str(c);
                    }
                    if s.current {
                        out.push_str(" (this session)");
                    }
                }
                _ => {}
            }
            return;
        }
        let Some(p) = self.process(row) else {
            return;
        };
        match col {
            col::USER => {
                out.push_str(p.name());
                if let Some(d) = &p.statics.description {
                    out.push_str(" \u{b7} ");
                    out.push_str(d);
                }
            }
            col::ID => format::count(out, p.key().pid),
            col::STATUS => out.push_str(status_label(p)),
            col::CPU => format::percent(out, p.cpu.get()),
            col::MEMORY => format::bytes(out, p.private_bytes),
            col::DISK => format::rate(
                out,
                Bytes(p.disk_read.get() + p.disk_write.get()),
                self.interval_secs,
            ),
            _ => {}
        }
    }

    fn heat(&self, row: usize, col: usize) -> Option<f32> {
        match col {
            col::CPU => self.number(row, col).map(|v| (v as f32 / 100.0).min(1.0)),
            _ => None,
        }
    }

    fn compare(&self, a: usize, b: usize, col: usize) -> Ordering {
        match (self.number(a, col), self.number(b, col)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => match col {
                col::STATUS | col::STATION => {
                    let (mut x, mut y) = (String::new(), String::new());
                    self.cell(a, col, &mut x);
                    self.cell(b, col, &mut y);
                    x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase())
                }
                _ => self
                    .name(a)
                    .to_ascii_lowercase()
                    .cmp(&self.name(b).to_ascii_lowercase()),
            },
        }
    }

    fn visible(&self, row: usize) -> bool {
        if let Some(p) = self.process(row) {
            if p.key().pid == 0 {
                return false;
            }
            let session = self.session_of[row - self.sessions()];
            return self.matched.get(row).copied().unwrap_or(true)
                || self.matched.get(session as usize).copied().unwrap_or(false);
        }
        // A session is listed when it or one of its processes matches.
        self.matched.get(row).copied().unwrap_or(true)
            || self
                .session_of
                .iter()
                .enumerate()
                .any(|(i, &s)| s as usize == row && self.matched[self.sessions() + i])
    }

    fn muted(&self, row: usize) -> bool {
        !self.matched.get(row).copied().unwrap_or(true)
    }

    fn parent(&self, row: usize) -> Option<usize> {
        let i = row.checked_sub(self.sessions())?;
        let s = *self.session_of.get(i)?;
        (s != u32::MAX).then_some(s as usize)
    }

    fn collapsed_by_default(&self, row: usize) -> bool {
        self.session(row).is_some()
    }
}

impl UsersPage {
    fn rows(&self) -> Rows<'_> {
        Rows {
            snap: &self.snap,
            session_of: &self.session_of,
            totals: &self.totals,
            matched: &self.matched,
            interval_secs: self.snap.interval.as_secs_f32().max(0.001),
        }
    }

    /// A new snapshot: map processes to sessions, add up, and filter.
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.snap = snap;
        let sessions = &self.snap.sessions;
        self.session_of.clear();
        self.totals.clear();
        self.totals.resize(sessions.len(), Totals::default());
        for p in &self.snap.processes {
            let s = sessions
                .iter()
                .position(|s| s.id == p.statics.session_id)
                .map_or(u32::MAX, |i| i as u32);
            self.session_of.push(s);
            if let Some(t) = self.totals.get_mut(s as usize) {
                if p.key().pid != 0 {
                    t.cpu += p.cpu.get();
                    t.memory = t.memory.saturating_add(p.private_bytes.get());
                    t.disk = t
                        .disk
                        .saturating_add(p.disk_read.get() + p.disk_write.get());
                    t.processes += 1;
                }
            }
        }
        if !self.list.table.tree() {
            let theme = Theme::dark();
            let rows = Rows {
                snap: &self.snap,
                session_of: &self.session_of,
                totals: &self.totals,
                matched: &self.matched,
                interval_secs: 1.0,
            };
            self.list.table.set_tree(true, &rows, &theme);
        }
        self.refilter();
        self.list.table.refresh();
    }

    fn refilter(&mut self) {
        let needle = self.list.search.needle.clone();
        self.matched.clear();
        for s in &self.snap.sessions {
            self.matched.push(any_matches(
                &needle,
                [s.user.as_deref().unwrap_or(""), &s.station]
                    .into_iter()
                    .chain(s.client.as_deref()),
            ));
        }
        let mut pid_buf = String::new();
        for p in &self.snap.processes {
            self.matched
                .push(process_rows::process_matches(p, &needle, &mut pid_buf));
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
                snap: &self.snap,
                session_of: &self.session_of,
                totals: &self.totals,
                matched: &self.matched,
                interval_secs: 1.0,
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
        let entries = if let Some(s) = rows.session(row) {
            let signed_in = s.user.is_some();
            vec![
                MenuEntry::item(MenuAction::SessionDisconnect, "Disconnect", signed_in),
                MenuEntry::item(MenuAction::SessionSignOut, "Sign out", signed_in),
            ]
        } else {
            vec![
                MenuEntry::item(MenuAction::GoToProcess, "Go to process", true),
                MenuEntry::item(MenuAction::EndTask, "End task", true),
                MenuEntry::Separator,
                MenuEntry::item(MenuAction::Copy, "Copy", true),
            ]
        };
        Reaction::effect(Effect::Menu { at, entries })
    }

    pub fn menu_action(&mut self, action: MenuAction) -> PageOutcome {
        let rows = self.rows();
        let Some(row) = self.list.selected(&rows) else {
            return Reaction::NONE.into();
        };
        if let Some(s) = rows.session(row) {
            let user = s.user.clone().unwrap_or_else(|| s.station.clone());
            let act = match action {
                MenuAction::SessionDisconnect => SessionAction::Disconnect,
                MenuAction::SessionSignOut => SessionAction::SignOut,
                _ => return Reaction::NONE.into(),
            };
            return Reaction::effect(Effect::Session {
                id: s.id,
                user,
                action: act,
            })
            .into();
        }
        let Some(p) = rows.process(row) else {
            return Reaction::NONE.into();
        };
        match action {
            MenuAction::GoToProcess => PageOutcome::SelectPid(p.key().pid),
            MenuAction::EndTask => Reaction::effect(Effect::Terminate {
                targets: vec![p.key()],
                label: p.name().to_owned(),
            })
            .into(),
            MenuAction::Copy => {
                let mut text = String::new();
                let mut cell = String::new();
                for (i, (ci, _)) in self.list.table.shown().enumerate() {
                    rows.cell(row, ci, &mut cell);
                    if i > 0 {
                        text.push('\t');
                    }
                    text.push_str(&cell);
                }
                Reaction::effect(Effect::CopyText(text)).into()
            }
            _ => Reaction::NONE.into(),
        }
    }

    pub fn paint(&mut self, dl: &mut DisplayList, rect: Rect, theme: &Theme, buf: &mut String) {
        let rows = Rows {
            snap: &self.snap,
            session_of: &self.session_of,
            totals: &self.totals,
            matched: &self.matched,
            interval_secs: self.snap.interval.as_secs_f32().max(0.001),
        };
        let total = self
            .snap
            .sessions
            .iter()
            .filter(|s| s.user.is_some())
            .count();
        count_text(
            &mut self.buf,
            total,
            total,
            if total == 1 { "user" } else { "users" },
        );
        self.list.paint(dl, rect, &rows, &self.buf, theme, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_rows::tests::proc;
    use crate::view::{Command, Key, MouseButton};
    use ot_model::process::ProcessStatic;
    use ot_model::session::SessionState;
    use ot_paint::{DrawCmd, Size};
    use std::time::{Duration, SystemTime};

    fn session(id: u32, user: Option<&str>) -> SessionInfo {
        SessionInfo {
            id,
            user: user.map(str::to_owned),
            station: if id == 0 {
                "Services".into()
            } else {
                "Console".into()
            },
            state: if user.is_some() {
                SessionState::Active
            } else {
                SessionState::Idle
            },
            client: None,
            current: id == 1,
        }
    }

    fn in_session(pid: u32, session: u32, cpu: f32) -> ot_model::process::ProcessSample {
        let mut p = proc(pid, None, cpu);
        p.statics = Arc::new(ProcessStatic {
            session_id: session,
            ..(*p.statics).clone()
        });
        p
    }

    #[test]
    fn session_rows_have_no_icon_and_process_rows_have_their_programs() {
        let mut page = UsersPage::default();
        let mut s = (*snapshot()).clone();
        let mut statics = (*s.processes[0].statics).clone();
        statics.image_path = Some("C:\\x\\a.exe".to_owned());
        s.processes[0].statics = Arc::new(statics);
        page.set_snapshot(Arc::new(s));
        let rows = page.rows();
        assert!(rows.images());
        assert!(rows.image(0).is_none(), "a session row");
        let first_process = rows.sessions();
        assert_eq!(rows.image(first_process), Some("C:\\x\\a.exe"));
    }

    fn snapshot() -> Arc<Snapshot> {
        Arc::new(Snapshot {
            taken_at: Some(SystemTime::UNIX_EPOCH),
            interval: Duration::from_secs(1),
            sessions: vec![session(0, None), session(1, Some("camer"))],
            processes: vec![
                in_session(0, 0, 90.0),
                in_session(4, 0, 2.0),
                in_session(100, 1, 10.0),
                in_session(101, 1, 5.0),
            ],
            ..Snapshot::default()
        })
    }

    fn texts(page: &mut UsersPage) -> Vec<String> {
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
    fn sessions_add_up_their_processes_and_fold_by_default() {
        let mut page = UsersPage::default();
        page.set_snapshot(snapshot());
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "camer"), "{t:?}");
        assert!(t.iter().any(|s| s == "Services"), "{t:?}");
        assert!(t.iter().any(|s| s == "1 user"), "{t:?}");
        // 10 + 5 for camer; the idle process is left out of session 0's 2.
        assert!(t.iter().any(|s| s == "15"), "{t:?}");
        assert!(t.iter().any(|s| s == "2.0"), "{t:?}");
        assert!(!t.iter().any(|s| s == "p100.exe"), "folded: {t:?}");

        // Expand camer (sorted first by CPU) with the keyboard, and the menu on a
        // process row jumps to it.
        let theme = Theme::dark();
        page.handle(UiEvent::Key(Key::Down), &theme);
        page.handle(UiEvent::Key(Key::Right), &theme);
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "p100.exe"), "{t:?}");
        page.handle(UiEvent::Key(Key::Down), &theme);
        assert_eq!(
            page.menu_action(MenuAction::GoToProcess),
            PageOutcome::SelectPid(100)
        );
        let r = page.menu_action(MenuAction::SessionSignOut);
        assert_eq!(r, PageOutcome::Reaction(Reaction::NONE));
        page.handle(UiEvent::Key(Key::Up), &theme);
        let PageOutcome::Reaction(r) = page.menu_action(MenuAction::SessionSignOut) else {
            panic!()
        };
        assert_eq!(
            r.effect,
            Some(Effect::Session {
                id: 1,
                user: "camer".to_owned(),
                action: SessionAction::SignOut
            })
        );
        // The search narrows to matching processes and keeps their session.
        for c in "p101".chars() {
            page.handle(UiEvent::Char(c), &theme);
        }
        let t = texts(&mut page);
        assert!(t.iter().any(|s| s == "p101.exe"), "{t:?}");
        assert!(!t.iter().any(|s| s == "p100.exe"), "{t:?}");
        assert!(t.iter().any(|s| s == "camer"), "{t:?}");
        let _ = page.handle(UiEvent::Command(Command::Find), &theme);
        let _ = page.handle(
            UiEvent::MouseDown {
                at: Point::new(1.0, 1.0),
                button: MouseButton::Left,
            },
            &theme,
        );
    }
}
