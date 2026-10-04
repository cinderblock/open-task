//! Where the UI's snapshots come from: a live sampler or a replay.

use std::sync::Arc;
use std::time::Duration;

use crate::player::Player;
use crate::sampler::Sampler;
use crate::snapshot::Snapshot;

/// A source of snapshots. The UI holds one of these and does not care which.
#[derive(Debug)]
pub enum Feed {
    /// Measuring this machine now.
    Live(Sampler),
    /// Playing a recording.
    Replay(Player),
}

impl Feed {
    /// The newest snapshot. Never blocks.
    #[must_use]
    pub fn latest(&self) -> Arc<Snapshot> {
        match self {
            Self::Live(s) => s.latest(),
            Self::Replay(p) => p.latest(),
        }
    }

    /// The sampling interval: the live one, or the one the recording was made
    /// with.
    #[must_use]
    pub fn interval(&self) -> Duration {
        match self {
            Self::Live(s) => s.interval(),
            Self::Replay(p) => p.interval(),
        }
    }

    /// Change the live cadence. A replay's cadence is in the file; this does
    /// nothing to it (use [`Player::set_speed`]).
    pub fn set_interval(&self, interval: Duration) {
        if let Self::Live(s) = self {
            s.set_interval(interval);
        }
    }

    /// Failed passes in a row, for the "probe failing" notice. A replay has none.
    #[must_use]
    pub fn consecutive_errors(&self) -> u64 {
        match self {
            Self::Live(s) => s.consecutive_errors(),
            Self::Replay(_) => 0,
        }
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }

    #[must_use]
    pub fn sampler(&self) -> Option<&Sampler> {
        match self {
            Self::Live(s) => Some(s),
            Self::Replay(_) => None,
        }
    }

    #[must_use]
    pub fn player(&self) -> Option<&Player> {
        match self {
            Self::Live(_) => None,
            Self::Replay(p) => Some(p),
        }
    }
}

impl From<Sampler> for Feed {
    fn from(s: Sampler) -> Self {
        Self::Live(s)
    }
}

impl From<Player> for Feed {
    fn from(p: Player) -> Self {
        Self::Replay(p)
    }
}
