//! The pages beyond Processes and Performance: lists of users, services, startup
//! entries, connections and installed programs, and the System facts page. Each
//! is built on [`crate::list_page::ListPage`] (or, for System, on a plain scroll
//! of facts) and answers input with a [`PageOutcome`].

use crate::view::Reaction;

pub(crate) mod apps;
pub(crate) mod connections;
pub(crate) mod services;
pub(crate) mod startup;
pub(crate) mod system;
pub(crate) mod users;

/// What a page asks of the app after an event: the usual repaint or effect, or
/// a jump to a process on the Processes page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PageOutcome {
    Reaction(Reaction),
    /// Select this PID on the Processes page.
    SelectPid(u32),
}

impl From<Reaction> for PageOutcome {
    fn from(r: Reaction) -> Self {
        Self::Reaction(r)
    }
}

/// `count` of `total` things, or just the count when nothing is filtered out.
pub(crate) fn count_text(buf: &mut String, listed: usize, total: usize, noun: &str) {
    use std::fmt::Write as _;
    buf.clear();
    if listed == total {
        let _ = write!(buf, "{total} {noun}");
    } else {
        let _ = write!(buf, "{listed} of {total} {noun}");
    }
}
