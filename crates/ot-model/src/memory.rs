//! System memory state.

use crate::units::Bytes;

/// Whole-system memory for one sample.
///
/// "Available" rather than "free" is the number that actually predicts whether the
/// machine is about to start swapping, because it counts reclaimable cache. Reporting
/// free-only is why so many tools claim a healthy machine is out of RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemorySample {
    /// Total physical RAM installed.
    pub total: Bytes,
    /// Physical memory available to new allocations without paging out.
    pub available: Bytes,
    /// Physical memory currently holding cached file data.
    pub cached: Bytes,
    /// Total committed virtual memory (Windows commit charge, Linux `Committed_AS`).
    pub committed: Bytes,
    /// Commit limit: physical + page/swap file.
    pub commit_limit: Bytes,
    /// Bytes reclaimed by in-memory compression, where supported.
    pub compressed: Option<Bytes>,
    /// Bytes currently paged out to disk.
    pub swap_used: Option<Bytes>,
}

impl MemorySample {
    /// Physical memory in use, derived rather than reported so it always agrees with
    /// `total` and `available`.
    #[must_use]
    pub const fn in_use(&self) -> Bytes {
        self.total.saturating_sub(self.available)
    }
}
