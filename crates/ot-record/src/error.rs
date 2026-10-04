//! What can go wrong writing or reading a recording.

use crate::format::FORMAT_VERSION;

/// Why a recording could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The file could not be created, written, opened or read.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The file does not start with the recording magic: it is something else.
    #[error("not an open-task recording")]
    NotARecording,
    /// The file was written by a build with a different format version.
    #[error(
        "recording format version {0} is not supported (this build reads version \
         {FORMAT_VERSION})"
    )]
    UnsupportedVersion(u16),
    /// The file's structure does not hold together: a record of an unexpected
    /// kind, a reference to a value that was never written, a frame that does not
    /// decompress.
    #[error("corrupt recording: {0}")]
    Corrupt(String),
    /// A value could not be encoded or decoded.
    #[error("encoding: {0}")]
    Encoding(#[from] postcard::Error),
    /// A frame index past the end of the recording.
    #[error("frame {index} is out of range: the recording has {len} frames")]
    OutOfRange { index: usize, len: usize },
    /// The recorder has already written its index; nothing more can be added.
    #[error("the recording is finished")]
    Finished,
}

impl Error {
    pub(crate) fn corrupt(what: impl Into<String>) -> Self {
        Self::Corrupt(what.into())
    }
}
