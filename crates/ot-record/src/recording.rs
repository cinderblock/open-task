//! Reading a recording.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use ot_model::hardware::Hardware;
use ot_model::snapshot::Snapshot;
use ot_model::Tick;

use crate::format::{
    self, FrameEntry, Index, Kind, RecordHeader, FORMAT_VERSION, FRAME_PREFIX_LEN, MAGIC,
    MAX_PAYLOAD, PREAMBLE_LEN, RECORD_HEADER_LEN, TRAILER_LEN,
};
use crate::frame::{self, Readers};
use crate::Error;

/// A recording open for reading.
///
/// Opening reads the header, the index (or rebuilds it by scanning when the file
/// was cut short) and every table, so [`frame`](Recording::frame) needs only one
/// read and one decode per frame. Frames come back behind an `Arc`, with the shared
/// values (`ProcessStatic` and its kin) one `Arc` per distinct value across every
/// frame, as they are live.
///
/// Reading is `&self`, so a `Recording` can sit behind an `Arc` and be read from a
/// player thread while the UI asks it for its length and times.
#[derive(Debug)]
pub struct Recording {
    path: PathBuf,
    file: Mutex<File>,
    file_len: u64,
    header: RecordHeader,
    hardware: Arc<Hardware>,
    frames: Vec<FrameEntry>,
    readers: Readers,
    had_index: bool,
    /// The last frame decoded, so sequential reads decode one frame each, and a
    /// delta frame right after it needs no keyframe.
    cache: Mutex<Option<(usize, Arc<Snapshot>)>>,
}

impl Recording {
    /// Open `path`.
    ///
    /// # Errors
    /// [`Error::NotARecording`] or [`Error::UnsupportedVersion`] if the file is not
    /// one of ours; [`Error::Corrupt`] if its records do not hold together;
    /// [`Error::Io`] if it cannot be read. A file without its index (the recorder
    /// never finished) opens fine, with every complete frame.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let file_len = file.metadata()?.len();

        let mut preamble = [0u8; PREAMBLE_LEN as usize];
        file.read_exact(&mut preamble)
            .map_err(|_| Error::NotARecording)?;
        if &preamble[..MAGIC.len()] != MAGIC {
            return Err(Error::NotARecording);
        }
        let version = u16::from_le_bytes([preamble[5], preamble[6]]);
        if version != FORMAT_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }

        let (kind, header_raw) = read_record(&mut file, PREAMBLE_LEN, file_len)?
            .ok_or_else(|| Error::corrupt("no header record"))?;
        if kind != Kind::Header {
            return Err(Error::corrupt("the first record is not the header"));
        }
        let header: RecordHeader = postcard::from_bytes(&header_raw)?;
        let body_start = PREAMBLE_LEN + RECORD_HEADER_LEN + header_raw.len() as u64;

        let mut readers = Readers::default();
        let (frames, had_index) = match read_index(&mut file, body_start, file_len)? {
            Some((index, index_at)) if load_tables(&mut file, &index, index_at, &mut readers) => {
                (index.frames, true)
            }
            _ => {
                readers = Readers::default();
                (scan(&mut file, body_start, file_len, &mut readers)?, false)
            }
        };

        Ok(Self {
            path,
            file: Mutex::new(file),
            file_len,
            hardware: Arc::new(header.hardware.clone()),
            header,
            frames,
            readers,
            had_index,
            cache: Mutex::new(None),
        })
    }

    /// Where the file is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's size in bytes.
    #[must_use]
    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    #[must_use]
    pub fn header(&self) -> &RecordHeader {
        &self.header
    }

    /// The machine the recording was made on, as every frame shares it.
    #[must_use]
    pub fn hardware(&self) -> &Arc<Hardware> {
        &self.hardware
    }

    /// Whether the file ended with its index. `false` means the recorder never
    /// finished (the app died, or the file was cut) and the frames were found by
    /// scanning.
    #[must_use]
    pub fn had_index(&self) -> bool {
        self.had_index
    }

    /// Frames in the recording.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Distinct processes the recording has statics for.
    #[must_use]
    pub fn processes_seen(&self) -> usize {
        self.readers.statics_len()
    }

    /// The tick of frame `i`, without decoding it.
    #[must_use]
    pub fn tick_at(&self, i: usize) -> Option<Tick> {
        self.frames.get(i).map(|f| Tick(f.tick))
    }

    /// The wall time of frame `i`, without decoding it. `None` past the end or
    /// when the snapshot had no time.
    #[must_use]
    pub fn time_at(&self, i: usize) -> Option<SystemTime> {
        self.frames.get(i)?.at_unix_ms.map(format::system_time)
    }

    /// Milliseconds from the first frame to frame `i`, for pacing a replay. Frames
    /// without a wall time are placed by the header's interval.
    #[must_use]
    pub fn offset_ms(&self, i: usize) -> Option<i64> {
        let entry = self.frames.get(i)?;
        let first = self.frames.first()?;
        match (entry.at_unix_ms, first.at_unix_ms) {
            (Some(at), Some(start)) => Some(at - start),
            _ => Some(
                i64::try_from(i).ok()? * i64::try_from(self.header.interval.as_millis()).ok()?,
            ),
        }
    }

    /// From the first frame to the last.
    #[must_use]
    pub fn duration(&self) -> Duration {
        match self
            .len()
            .checked_sub(1)
            .and_then(|last| self.offset_ms(last))
        {
            Some(ms) => Duration::from_millis(ms.max(0).unsigned_abs()),
            None => Duration::ZERO,
        }
    }

    /// Whether frame `i` is a keyframe (decodes on its own).
    #[must_use]
    pub fn is_keyframe(&self, i: usize) -> bool {
        self.frames.get(i).is_some_and(|f| f.key)
    }

    /// Frame `i`, decoded. Reading frames in order costs one decode each; a seek
    /// decodes forward from the nearest keyframe at or before `i`.
    ///
    /// # Errors
    /// [`Error::OutOfRange`] past the end; [`Error::Corrupt`] or [`Error::Io`] if
    /// the frame, or a frame it builds on, cannot be read.
    pub fn frame(&self, i: usize) -> Result<Arc<Snapshot>, Error> {
        let len = self.len();
        if i >= len {
            return Err(Error::OutOfRange { index: i, len });
        }
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, snap)) = &*cache {
            if *at == i {
                return Ok(Arc::clone(snap));
            }
        }
        let (start, mut base) = match &*cache {
            Some((at, snap)) if *at + 1 == i && !self.frames[i].key => (i, Some(Arc::clone(snap))),
            _ => (self.keyframe_before(i)?, None),
        };
        for k in start..=i {
            let snap = self.decode(k, base.as_deref())?;
            base = Some(Arc::new(snap));
        }
        let snap = base.ok_or_else(|| Error::corrupt("no frame decoded"))?;
        *cache = Some((i, Arc::clone(&snap)));
        Ok(snap)
    }

    /// The nearest keyframe at or before `i`.
    fn keyframe_before(&self, i: usize) -> Result<usize, Error> {
        (0..=i)
            .rev()
            .find(|&k| self.frames[k].key)
            .ok_or_else(|| Error::corrupt(format!("no keyframe before frame {i}")))
    }

    /// Read and decode frame `k` against `prev`.
    fn decode(&self, k: usize, prev: Option<&Snapshot>) -> Result<Snapshot, Error> {
        let entry = self.frames[k];
        let payload = {
            let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
            let (kind, payload) = read_record(&mut file, entry.offset, self.file_len)?
                .ok_or_else(|| Error::corrupt(format!("frame {k} is past the end of the file")))?;
            let expected = if entry.key {
                Kind::KeyFrame
            } else {
                Kind::DeltaFrame
            };
            if kind != expected {
                return Err(Error::corrupt(format!(
                    "frame {k}: expected a {expected:?} record, found {kind:?}"
                )));
            }
            payload
        };
        let Some(body) = payload.get(FRAME_PREFIX_LEN..) else {
            return Err(Error::corrupt(format!(
                "frame {k} is shorter than its prefix"
            )));
        };
        if !entry.key && prev.is_none() {
            return Err(Error::corrupt(format!("frame {k} is a delta with no base")));
        }
        let raw = format::decompress(body)?;
        let body: frame::Body<'_> = postcard::from_bytes(&raw)?;
        frame::decode(body, prev, &self.readers, &self.hardware)
    }
}

/// Read the record at `offset`: its kind and payload. `None` when the file ends
/// before the record does (a recording cut short), or the kind is unknown.
fn read_record(
    file: &mut File,
    offset: u64,
    file_len: u64,
) -> Result<Option<(Kind, Vec<u8>)>, Error> {
    if offset + RECORD_HEADER_LEN > file_len {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset))?;
    let Some((kind, len)) = format::read_record_header(file)? else {
        return Ok(None);
    };
    let Some(kind) = Kind::from_u8(kind) else {
        return Ok(None);
    };
    if len > MAX_PAYLOAD {
        return Err(Error::corrupt(format!(
            "record at {offset} claims {len} bytes"
        )));
    }
    if offset + RECORD_HEADER_LEN + u64::from(len) > file_len {
        return Ok(None);
    }
    let mut payload = vec![0u8; len as usize];
    file.read_exact(&mut payload)?;
    Ok(Some((kind, payload)))
}

/// The index named by the trailer, and the offset of its record, if the file ends
/// with a trailer that points at a plausible index. `None` means scan.
fn read_index(
    file: &mut File,
    body_start: u64,
    file_len: u64,
) -> Result<Option<(Index, u64)>, Error> {
    if file_len < body_start + RECORD_HEADER_LEN + TRAILER_LEN {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(file_len - TRAILER_LEN))?;
    let mut trailer = [0u8; TRAILER_LEN as usize];
    file.read_exact(&mut trailer)?;
    if &trailer[4..] != format::INDEX_MAGIC {
        return Ok(None);
    }
    let index_len = u64::from(u32::from_le_bytes([
        trailer[0], trailer[1], trailer[2], trailer[3],
    ]));
    let Some(index_at) = (file_len - TRAILER_LEN)
        .checked_sub(index_len + RECORD_HEADER_LEN)
        .filter(|&at| at >= body_start)
    else {
        return Ok(None);
    };
    let Some((Kind::Index, raw)) = read_record(file, index_at, file_len)? else {
        return Ok(None);
    };
    let Ok(index) = postcard::from_bytes::<Index>(&raw) else {
        return Ok(None);
    };
    let in_body = |offset: u64| offset >= body_start && offset + RECORD_HEADER_LEN <= index_at;
    let ascending = index.frames.windows(2).all(|w| w[0].offset < w[1].offset);
    let plausible = ascending
        && index.frames.iter().all(|f| in_body(f.offset))
        && index.tables.iter().all(|&t| in_body(t))
        && index.frames.first().is_none_or(|f| f.key);
    Ok(plausible.then_some((index, index_at)))
}

/// Load every table record the index names. `false` if any is not where the index
/// says, in which case the caller scans instead.
fn load_tables(file: &mut File, index: &Index, index_at: u64, readers: &mut Readers) -> bool {
    for &offset in &index.tables {
        let Ok(Some((kind, payload))) = read_record(file, offset, index_at) else {
            return false;
        };
        if !kind.is_table() {
            return false;
        }
        let Ok(raw) = format::decompress(&payload) else {
            return false;
        };
        if readers.load(kind, &raw).is_err() {
            return false;
        }
    }
    true
}

/// Walk the records from `body_start`, loading tables and listing frames, until the
/// file ends or a record is incomplete.
fn scan(
    file: &mut File,
    body_start: u64,
    file_len: u64,
    readers: &mut Readers,
) -> Result<Vec<FrameEntry>, Error> {
    let mut frames = Vec::new();
    file.seek(SeekFrom::Start(body_start))?;
    let mut r = BufReader::with_capacity(256 << 10, file);
    let mut pos = body_start;
    while let Some((kind, len)) = format::read_record_header(&mut r)? {
        let offset = pos;
        pos += RECORD_HEADER_LEN;
        let end = pos + u64::from(len);
        if len > MAX_PAYLOAD || end > file_len {
            break;
        }
        let Some(kind) = Kind::from_u8(kind) else {
            break;
        };
        if kind.is_frame() {
            let mut prefix = [0u8; FRAME_PREFIX_LEN];
            if (len as usize) < FRAME_PREFIX_LEN {
                break;
            }
            r.read_exact(&mut prefix)?;
            let Some((tick, at_unix_ms)) = format::parse_frame_prefix(&prefix) else {
                break;
            };
            frames.push(FrameEntry {
                offset,
                key: kind == Kind::KeyFrame,
                tick,
                at_unix_ms,
            });
            r.seek_relative(i64::try_from(len as usize - FRAME_PREFIX_LEN).unwrap_or(i64::MAX))?;
        } else if kind.is_table() {
            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload)?;
            let raw = format::decompress(&payload)?;
            readers.load(kind, &raw)?;
        } else {
            // The index, or a second header: nothing of use follows.
            break;
        }
        pos = end;
    }
    // A delta frame with nothing before it cannot be decoded; a file cannot start
    // that way, so this only trims garbage.
    if let Some(first_key) = frames.iter().position(|f| f.key) {
        frames.drain(..first_key);
    } else {
        frames.clear();
    }
    Ok(frames)
}
