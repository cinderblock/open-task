//! Handing the history to the next instance of the app, across an update.
//!
//! When an update closes the app and Setup starts the new version, the new
//! instance would start with empty charts. Instead the closing instance writes
//! its [`Timeline`] and [`Usage`] to a file ([`encode`]) and the new one carries
//! on from it ([`decode`]). The file location, and when it is written and taken,
//! are the shell's business; this module is the format.
//!
//! The two builds can differ, so the format says which build wrote it and which
//! layout its payload has. A build writes [`FORMAT`] and reads the layouts in
//! [`READS`]; anything else is refused with [`Error::UnknownFormat`], and the
//! caller starts fresh. Nothing in the file is trusted: every count, id and order
//! is checked before any of it reaches the app ([`Error::Invalid`]).
//!
//! ```text
//! "OTHANDOF"            magic, 8 bytes
//! format: u32-LE        the payload's layout: FORMAT when written by this build
//! written_ms: i64-LE    when it was written, ms since the Unix epoch
//! pid: u32-LE           the process that wrote it
//! version_len: u16-LE   then that many bytes: the writer's version, UTF-8
//! raw_len: u32-LE       the payload's length uncompressed
//! packed_len: u32-LE    then the payload, lz4 block-compressed:
//! checksum: u64-LE      FNV-1a 64 of the compressed payload
//! payload[packed_len]   postcard of the history, as laid out by `FORMAT`
//! ```
//!
//! The header's layout never changes, so any build can say which build and which
//! format a file came from, even one it cannot read. The payload is postcard,
//! which is not self-describing: its layout is the structs below, and **any change
//! to them, or to the `ot-model` types they hold, must bump [`FORMAT`]**. The test
//! `the_payload_layout_is_pinned` fails until it is bumped. An older layout stays
//! readable while its structs stay: each is pinned by a file in `testdata/`.

use std::fmt;

use ot_model::process::ProcessStatic;
use ot_model::ProcessKey;
use serde::{Deserialize, Serialize};

use crate::timeline::{Retention, Timeline};
use crate::usage::Usage;

/// The first eight bytes of every handoff file.
pub const MAGIC: &[u8; 8] = b"OTHANDOF";
/// The payload layout this build writes.
///
/// - 1: the first: the timeline's series and the usage history.
/// - 2: format 1's, then the timeline's seams (where earlier restarts joined
///   the record).
pub const FORMAT: u32 = 2;
/// The payload layouts this build reads.
pub const READS: [u32; 2] = [1, 2];
/// The largest payload a reader will decompress. An hour of a busy machine's
/// history is a few megabytes; a day, a few tens.
const MAX_RAW: u32 = 256 << 20;
/// The longest writer version a header may carry.
const MAX_VERSION: usize = 256;

/// What the header says about a handoff file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// The payload's layout.
    pub format: u32,
    /// The version of the build that wrote it (`0.21.0`, `0.20.2-3-gabcdef0`).
    pub app_version: String,
    /// When it was written, in ms since the Unix epoch.
    pub written_unix_ms: i64,
    /// The process that wrote it.
    pub pid: u32,
}

/// Why a handoff file could not be read. Each is a reason to start fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// It does not start with [`MAGIC`].
    NotHandoff,
    /// It ends before its header or payload does.
    Truncated,
    /// Written in a layout this build does not know: by a newer build, or an older
    /// one from before a change.
    UnknownFormat { format: u32, app_version: String },
    /// The bytes are damaged: the checksum, the compression or the encoding.
    Corrupt(String),
    /// It decodes, but what it holds breaks a rule the app depends on.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHandoff => f.write_str("not a handoff file"),
            Self::Truncated => f.write_str("the file is cut short"),
            Self::UnknownFormat {
                format,
                app_version,
            } => write!(
                f,
                "written by v{app_version} in format {format}, which this build \
                 (formats {}-{}) does not read",
                READS[0],
                READS[READS.len() - 1]
            ),
            Self::Corrupt(why) => write!(f, "the file is damaged: {why}"),
            Self::Invalid(why) => write!(f, "the history in it is not consistent: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// The file for `timeline` and `usage`, written now by process `pid` of build
/// `app_version`.
#[must_use]
pub fn encode(
    timeline: &Timeline,
    usage: &Usage,
    app_version: &str,
    written_unix_ms: i64,
    pid: u32,
) -> Vec<u8> {
    frame(&payload(timeline, usage), app_version, written_unix_ms, pid)
}

/// A file around an uncompressed `payload`.
fn frame(payload: &[u8], app_version: &str, written_unix_ms: i64, pid: u32) -> Vec<u8> {
    let packed = lz4_flex::block::compress(payload);
    let version = truncate(app_version, MAX_VERSION).as_bytes();
    let mut out = Vec::with_capacity(48 + version.len() + packed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    out.extend_from_slice(&written_unix_ms.to_le_bytes());
    out.extend_from_slice(&pid.to_le_bytes());
    out.extend_from_slice(&(version.len() as u16).to_le_bytes());
    out.extend_from_slice(version);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a(&packed).to_le_bytes());
    out.extend_from_slice(&packed);
    out
}

/// Read the header alone: which build wrote the file, when, and in which format.
/// Works for any format, so a refusal can say what it refused.
///
/// # Errors
/// Not a handoff file, or cut short.
pub fn meta(bytes: &[u8]) -> Result<Meta, Error> {
    split(bytes).map(|(meta, _)| meta)
}

/// Read a file [`encode`] wrote: its header, and the history in it with the
/// timeline's series kept to `retention` (the reader's own, so nothing in the file
/// decides how much is allocated).
///
/// # Errors
/// Any reason the file cannot be used; the caller should start fresh.
pub fn decode(bytes: &[u8], retention: &Retention) -> Result<(Meta, Timeline, Usage), Error> {
    let (meta, body) = split(bytes)?;
    if !READS.contains(&meta.format) {
        return Err(Error::UnknownFormat {
            format: meta.format,
            app_version: meta.app_version,
        });
    }
    let payload = unpack(&body)?;
    let corrupt = |e: postcard::Error| Error::Corrupt(e.to_string());
    let history = if meta.format == 1 {
        let v1: HistoryV1 = postcard::from_bytes(&payload).map_err(corrupt)?;
        HistoryV2 {
            timeline: v1.timeline,
            usage: v1.usage,
            seams: Vec::new(),
        }
    } else {
        postcard::from_bytes(&payload).map_err(corrupt)?
    };
    let mut timeline =
        Timeline::from_handoff(history.timeline, retention).map_err(Error::Invalid)?;
    timeline
        .restore_seams(history.seams)
        .map_err(Error::Invalid)?;
    let usage = Usage::from_handoff(history.usage).map_err(Error::Invalid)?;
    Ok((meta, timeline, usage))
}

/// The uncompressed payload for `timeline` and `usage`: what the layout test pins.
fn payload(timeline: &Timeline, usage: &Usage) -> Vec<u8> {
    let history = HistoryV2 {
        timeline: timeline.to_handoff(),
        usage: usage.to_handoff(),
        seams: timeline.seams_to_handoff(),
    };
    // Serializing plain structs into a Vec cannot fail.
    postcard::to_allocvec(&history).unwrap_or_default()
}

/// The compressed payload after the header.
struct Body<'a> {
    raw_len: u32,
    checksum: u64,
    packed: &'a [u8],
}

fn split(bytes: &[u8]) -> Result<(Meta, Body<'_>), Error> {
    let mut r = Reader(bytes);
    if r.take(MAGIC.len())? != MAGIC {
        return Err(Error::NotHandoff);
    }
    let format = r.u32()?;
    let written_unix_ms = r.i64()?;
    let pid = r.u32()?;
    let version_len = usize::from(r.u16()?);
    let app_version = String::from_utf8_lossy(r.take(version_len)?).into_owned();
    let meta = Meta {
        format,
        app_version,
        written_unix_ms,
        pid,
    };
    if !READS.contains(&format) {
        // The rest may be laid out differently; the header is all that is sure.
        return Ok((
            meta,
            Body {
                raw_len: 0,
                checksum: 0,
                packed: &[],
            },
        ));
    }
    let raw_len = r.u32()?;
    let packed_len = r.u32()? as usize;
    let checksum = r.u64()?;
    let packed = r.take(packed_len)?;
    Ok((
        meta,
        Body {
            raw_len,
            checksum,
            packed,
        },
    ))
}

fn unpack(body: &Body<'_>) -> Result<Vec<u8>, Error> {
    if fnv1a(body.packed) != body.checksum {
        return Err(Error::Corrupt("checksum mismatch".into()));
    }
    if body.raw_len > MAX_RAW {
        return Err(Error::Corrupt(format!(
            "a payload of {} bytes is larger than any history",
            body.raw_len
        )));
    }
    lz4_flex::block::decompress(body.packed, body.raw_len as usize)
        .map_err(|e| Error::Corrupt(e.to_string()))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn u16(&mut self) -> Result<u16, Error> {
        self.array().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, Error> {
        self.array().map(u64::from_le_bytes)
    }

    fn i64(&mut self) -> Result<i64, Error> {
        self.array().map(i64::from_le_bytes)
    }
}

/// FNV-1a, 64 bits: enough to tell a damaged file from a whole one.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// The payload. Every change here bumps FORMAT (see the module docs); a struct a
// format still in READS uses stays as it is.

/// Format 1's payload.
#[derive(Debug, Serialize, Deserialize)]
struct HistoryV1 {
    timeline: TimelineV1,
    usage: UsageV1,
}

/// Format 2's payload: format 1's, then the seams.
#[derive(Debug, Serialize, Deserialize)]
struct HistoryV2 {
    timeline: TimelineV1,
    usage: UsageV1,
    seams: Vec<SeamV2>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SeamV2 {
    pub from_ms: i64,
    pub to_ms: Option<i64>,
    pub kind: SeamKindV2,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SeamKindV2 {
    Update { from: String, to: String },
    Elevated,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct BucketV1 {
    pub first_ms: i64,
    pub last_ms: i64,
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct OpenBucketV1 {
    pub index: i64,
    pub bucket: BucketV1,
    pub sum: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TierV1 {
    pub bucket_ms: i64,
    /// Oldest first.
    pub closed: Vec<BucketV1>,
    pub open: Option<OpenBucketV1>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SeriesV1 {
    /// `(at_unix_ms, value)`, oldest first.
    pub raw: Vec<(i64, f32)>,
    pub tiers: Vec<TierV1>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DiskSeriesV1 {
    pub number: u32,
    pub active: SeriesV1,
    pub read: SeriesV1,
    pub write: SeriesV1,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PairSeriesV1 {
    pub id: u64,
    pub a: SeriesV1,
    pub b: SeriesV1,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TimelineV1 {
    pub cpu_total: SeriesV1,
    pub cores: Vec<SeriesV1>,
    pub mem_in_use: SeriesV1,
    pub disks: Vec<DiskSeriesV1>,
    /// `a` is received, `b` sent.
    pub adapters: Vec<PairSeriesV1>,
    /// `a` is utilization, `b` dedicated memory.
    pub gpus: Vec<PairSeriesV1>,
    pub battery_charge: SeriesV1,
    pub battery_rate: SeriesV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct MachineV1 {
    pub cpu_capacity_ms: f64,
    pub cpu_busy_ms: f64,
    pub ram: f64,
    pub ram_in_use: f64,
    pub gpu_capacity_ms: f64,
    pub gpu_busy_ms: f64,
    pub gpu_dedicated_in_use: f64,
}

/// One stretch's per-program lists, in the order of `usage`'s kinds.
pub(crate) type ListsV1 = [Vec<(u32, f32)>; 6];

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StoredV1 {
    pub end_ms: i64,
    pub span_ms: i64,
    pub lists: ListsV1,
    pub machine: MachineV1,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OpenStepV1 {
    pub index: i64,
    pub end_ms: i64,
    pub span_ms: i64,
    pub lists: ListsV1,
    pub machine: MachineV1,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ProcV1 {
    pub key: ProcessKey,
    pub statics: ProcessStatic,
    pub last_cycles: u64,
    pub last_cpu_ns: u64,
    pub total: f64,
    pub alive: bool,
    pub program: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct SeenV1 {
    pub key: ProcessKey,
    pub parent: Option<ProcessKey>,
    pub lost_parent: Option<u32>,
    pub program: u32,
    pub first_ms: i64,
    pub last_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UsageV1 {
    pub decay: f64,
    pub history_ms: i64,
    pub now_ms: i64,
    pub first_ms: Option<i64>,
    pub trimmed_ms: i64,
    pub programs: Vec<String>,
    /// By key.
    pub procs: Vec<ProcV1>,
    /// By key.
    pub seen: Vec<SeenV1>,
    /// Oldest first.
    pub raw: Vec<StoredV1>,
    pub coarse: Vec<StoredV1>,
    pub open: Option<OpenStepV1>,
    pub faded_out: Vec<f64>,
    pub faded_out_ms: Option<i64>,
}

#[cfg(test)]
mod tests;
