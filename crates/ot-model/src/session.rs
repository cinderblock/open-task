//! Logon sessions: who is signed in, and how.
use serde::{Deserialize, Serialize};

/// What a session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum SessionState {
    /// Signed in and at the console or over a remote connection.
    Active,
    /// Signed in, but nobody is connected to it.
    Disconnected,
    /// A session that exists but has nobody signed in (the logon screen, a
    /// listener).
    Idle,
    #[default]
    Other,
}

impl SessionState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Disconnected => "Disconnected",
            Self::Idle => "Idle",
            Self::Other => "",
        }
    }
}

/// One logon session, as the platform's session manager describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The session id processes carry ([`crate::process::ProcessStatic::session_id`]).
    pub id: u32,
    /// `DOMAIN\user`, or `None` for a session nobody is signed in to.
    pub user: Option<String>,
    /// The station name: `Console`, `RDP-Tcp#3`, `Services`.
    pub station: String,
    pub state: SessionState,
    /// The remote client's machine name, for a remote session.
    pub client: Option<String>,
    /// Whether this is the session open-task itself runs in.
    pub current: bool,
}
