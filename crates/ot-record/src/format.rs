//! The file layout: preamble, records, trailer.
//!
//! ```text
//! "OTREC" version:u16-LE
//! record*        kind:u8  len:u32-LE  payload[len]
//! trailer        index_len:u32-LE  "OTIDX"
//! ```
//!
//! Record kinds:
//!
//! | kind | payload |
//! | --- | --- |
//! | `Header` | postcard [`RecordHeader`], uncompressed |
//! | `KeyFrame`, `DeltaFrame` | `tick:u64-LE at_unix_ms:i64-LE` ([`NO_TIME`] for none), then the lz4 frame body |
//! | tables (`Statics` to `Services`) | lz4 postcard `Vec<(id:u32, value)>` |
//! | `Index` | postcard [`Index`], uncompressed; the last record, named by the trailer |
//!
//! A table record precedes the first frame that refers to one of its ids. Frames
//! carry their tick and time uncompressed so a reader rebuilding the index can
//! skip their bodies. lz4 payloads start with the uncompressed length as `u32-LE`,
//! which the reader checks against [`MAX_PAYLOAD`] before allocating.

use std::io::{self, Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ot_model::hardware::Hardware;
use ot_model::Capabilities;
use serde::{Deserialize, Serialize};

use crate::Error;

/// The first five bytes of every recording.
pub const MAGIC: &[u8; 5] = b"OTREC";
/// The format this build writes and reads.
pub const FORMAT_VERSION: u16 = 1;
/// The last five bytes of a finished recording.
pub(crate) const INDEX_MAGIC: &[u8; 5] = b"OTIDX";

/// Magic plus version.
pub(crate) const PREAMBLE_LEN: u64 = 7;
/// Kind plus length.
pub(crate) const RECORD_HEADER_LEN: u64 = 5;
/// Tick plus time, at the start of a frame payload.
pub(crate) const FRAME_PREFIX_LEN: usize = 16;
/// Index length plus magic.
pub(crate) const TRAILER_LEN: u64 = 9;
/// The largest payload or uncompressed body a reader will allocate for. A frame of
/// ten thousand threads is well under a megabyte; anything near this is a corrupt
/// length, not data.
pub(crate) const MAX_PAYLOAD: u32 = 256 << 20;
/// `at_unix_ms` of a frame whose snapshot had no wall time.
pub(crate) const NO_TIME: i64 = i64::MIN;

/// What a record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Kind {
    Header = 1,
    KeyFrame = 2,
    DeltaFrame = 3,
    Index = 4,
    Statics = 16,
    Windows = 17,
    ProcessServices = 18,
    Disks = 19,
    Adapters = 20,
    Gpus = 21,
    Services = 22,
}

impl Kind {
    pub(crate) fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            1 => Self::Header,
            2 => Self::KeyFrame,
            3 => Self::DeltaFrame,
            4 => Self::Index,
            16 => Self::Statics,
            17 => Self::Windows,
            18 => Self::ProcessServices,
            19 => Self::Disks,
            20 => Self::Adapters,
            21 => Self::Gpus,
            22 => Self::Services,
            _ => return None,
        })
    }

    pub(crate) fn is_frame(self) -> bool {
        matches!(self, Self::KeyFrame | Self::DeltaFrame)
    }

    pub(crate) fn is_table(self) -> bool {
        (self as u8) >= Self::Statics as u8
    }
}

/// What a recording says about itself, before any frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordHeader {
    /// The version of the open-task build that wrote the file (`0.8.0`,
    /// `0.8.0-3-gabcdef0-dirty`).
    pub app_version: String,
    /// The machine the recording was made on.
    pub hardware: Hardware,
    /// What its probe could measure.
    pub capabilities: Capabilities,
    /// When recording started.
    pub started: SystemTime,
    /// The sampling interval that was configured when recording started. Each
    /// frame carries the interval that was actually measured.
    pub interval: Duration,
}

/// Where everything is, written last so a reader can seek without scanning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Index {
    pub frames: Vec<FrameEntry>,
    /// Offsets of the table records, in file order.
    pub tables: Vec<u64>,
}

/// One frame in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FrameEntry {
    /// Offset of the record's kind byte.
    pub offset: u64,
    /// A keyframe decodes on its own; a delta frame needs the frame before it.
    pub key: bool,
    pub tick: u64,
    pub at_unix_ms: Option<i64>,
}

/// Write one record from `parts` concatenated. Returns the bytes written.
pub(crate) fn write_record(w: &mut impl Write, kind: Kind, parts: &[&[u8]]) -> io::Result<u64> {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let len32 = u32::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "record payload exceeds 4 GiB"))?;
    w.write_all(&[kind as u8])?;
    w.write_all(&len32.to_le_bytes())?;
    for p in parts {
        w.write_all(p)?;
    }
    Ok(RECORD_HEADER_LEN + len as u64)
}

/// Read a record header. `None` when the stream ended before a whole header:
/// the clean end of a file without an index, or one cut short.
pub(crate) fn read_record_header(r: &mut impl Read) -> io::Result<Option<(u8, u32)>> {
    let mut hdr = [0u8; RECORD_HEADER_LEN as usize];
    let mut got = 0;
    while got < hdr.len() {
        let n = r.read(&mut hdr[got..])?;
        if n == 0 {
            return Ok(None);
        }
        got += n;
    }
    Ok(Some((
        hdr[0],
        u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]),
    )))
}

/// The frame prefix: tick and wall time.
pub(crate) fn frame_prefix(tick: u64, at_unix_ms: Option<i64>) -> [u8; FRAME_PREFIX_LEN] {
    let mut p = [0u8; FRAME_PREFIX_LEN];
    p[..8].copy_from_slice(&tick.to_le_bytes());
    p[8..].copy_from_slice(&at_unix_ms.unwrap_or(NO_TIME).to_le_bytes());
    p
}

/// Parse [`frame_prefix`]'s output.
pub(crate) fn parse_frame_prefix(p: &[u8]) -> Option<(u64, Option<i64>)> {
    if p.len() < FRAME_PREFIX_LEN {
        return None;
    }
    let tick = u64::from_le_bytes(p[..8].try_into().ok()?);
    let at = i64::from_le_bytes(p[8..16].try_into().ok()?);
    Some((tick, (at != NO_TIME).then_some(at)))
}

/// lz4 block compression with the uncompressed length in front.
pub(crate) fn compress(raw: &[u8]) -> Vec<u8> {
    lz4_flex::block::compress_prepend_size(raw)
}

/// Undo [`compress`], refusing a length a corrupt file might claim.
pub(crate) fn decompress(packed: &[u8]) -> Result<Vec<u8>, Error> {
    let Some(len) = packed.get(..4) else {
        return Err(Error::corrupt(
            "compressed block shorter than its length prefix",
        ));
    };
    let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]);
    if len > MAX_PAYLOAD {
        return Err(Error::corrupt(format!(
            "compressed block claims {len} bytes uncompressed"
        )));
    }
    lz4_flex::block::decompress(&packed[4..], len as usize)
        .map_err(|e| Error::corrupt(format!("lz4: {e}")))
}

/// Milliseconds since the Unix epoch, for the frame prefix and the index.
pub(crate) fn unix_ms(t: Option<SystemTime>) -> Option<i64> {
    let t = t?;
    let ms = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).ok()?,
        Err(e) => -i64::try_from(e.duration().as_millis()).ok()?,
    };
    (ms != NO_TIME).then_some(ms)
}

/// Undo [`unix_ms`].
pub(crate) fn system_time(ms: i64) -> SystemTime {
    if ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms.unsigned_abs())
    } else {
        UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_round_trips() {
        let p = frame_prefix(7, Some(-1234));
        assert_eq!(parse_frame_prefix(&p), Some((7, Some(-1234))));
        let p = frame_prefix(u64::MAX, None);
        assert_eq!(parse_frame_prefix(&p), Some((u64::MAX, None)));
        assert_eq!(parse_frame_prefix(&p[..15]), None);
    }

    #[test]
    fn compression_round_trips_and_checks_length() {
        let raw: Vec<u8> = (0..10_000u32).map(|i| (i % 7) as u8).collect();
        let packed = compress(&raw);
        assert!(packed.len() < raw.len());
        assert_eq!(decompress(&packed).unwrap(), raw);

        let mut bad = packed.clone();
        bad[..4].copy_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
        assert!(matches!(decompress(&bad), Err(Error::Corrupt(_))));
        assert!(matches!(decompress(&packed[..3]), Err(Error::Corrupt(_))));
    }

    #[test]
    fn time_round_trips() {
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        assert_eq!(system_time(unix_ms(Some(t)).unwrap()), t);
        assert_eq!(unix_ms(None), None);
        let before = UNIX_EPOCH - Duration::from_millis(5000);
        assert_eq!(system_time(unix_ms(Some(before)).unwrap()), before);
    }

    #[test]
    fn kinds_round_trip() {
        for k in [
            Kind::Header,
            Kind::KeyFrame,
            Kind::DeltaFrame,
            Kind::Index,
            Kind::Statics,
            Kind::Windows,
            Kind::ProcessServices,
            Kind::Disks,
            Kind::Adapters,
            Kind::Gpus,
            Kind::Services,
        ] {
            assert_eq!(Kind::from_u8(k as u8), Some(k));
            assert_eq!(k.is_table(), k as u8 >= 16);
        }
        assert_eq!(Kind::from_u8(0), None);
        assert_eq!(Kind::from_u8(99), None);
    }
}
