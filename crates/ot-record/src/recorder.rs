//! Writing a recording.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use ot_model::snapshot::Snapshot;
use serde::Serialize;

use crate::format::{self, FrameEntry, Index, Kind, RecordHeader, FORMAT_VERSION, MAGIC};
use crate::frame::{self, Tables};
use crate::Error;

/// How many frames apart keyframes are unless [`Recorder::create_with_keyframes`]
/// says otherwise. A seek decodes forward from the nearest keyframe, so this bounds
/// the work of a seek at this many frames (each well under a millisecond), while
/// the keyframe's full cost is spread over the frames that follow it.
pub const DEFAULT_KEYFRAME_INTERVAL: u32 = 60;

/// How many frames between sweeps of the tables for values nobody holds any more.
const SWEEP_EVERY: u64 = 64;

/// What a recorder has written so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecorderStats {
    /// Frames written.
    pub frames: u64,
    /// Of which keyframes.
    pub keyframes: u64,
    /// Bytes in the file so far, header and tables included.
    pub bytes: u64,
    /// Bytes of frame records.
    pub frame_bytes: u64,
    /// Bytes of table records: the shared values, written once each.
    pub table_bytes: u64,
}

impl RecorderStats {
    /// Average file bytes per frame, everything included.
    #[must_use]
    pub fn bytes_per_frame(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.bytes as f64 / self.frames as f64
        }
    }
}

/// Writes snapshots to a file as they arrive.
///
/// Create one, [`write`](Recorder::write) each snapshot, then
/// [`finish`](Recorder::finish) to append the index. Dropping an unfinished
/// recorder finishes it too, ignoring any error; a file left without its index
/// (the process died) still opens, by scanning.
#[derive(Debug)]
pub struct Recorder {
    out: BufWriter<File>,
    /// Bytes written so far: the offset of the next record.
    pos: u64,
    tables: Tables,
    index: Index,
    /// The frame before, which the next delta frame is encoded against.
    prev: Option<Arc<Snapshot>>,
    /// Frames since the last keyframe, counting it.
    since_key: u32,
    keyframe_every: u32,
    stats: RecorderStats,
    finished: bool,
}

impl Recorder {
    /// Create (or truncate) `path` and write the preamble and `header`.
    ///
    /// # Errors
    /// If the file cannot be created or written.
    pub fn create(path: impl AsRef<Path>, header: &RecordHeader) -> Result<Self, Error> {
        Self::create_with_keyframes(path, header, DEFAULT_KEYFRAME_INTERVAL)
    }

    /// [`create`](Recorder::create) with a keyframe every `keyframe_every` frames
    /// (at least 1: every frame a keyframe).
    ///
    /// # Errors
    /// If the file cannot be created or written.
    pub fn create_with_keyframes(
        path: impl AsRef<Path>,
        header: &RecordHeader,
        keyframe_every: u32,
    ) -> Result<Self, Error> {
        let file = File::create(path)?;
        let mut out = BufWriter::with_capacity(256 << 10, file);
        out.write_all(MAGIC)?;
        out.write_all(&FORMAT_VERSION.to_le_bytes())?;
        let mut pos = format::PREAMBLE_LEN;
        let header_bytes = postcard::to_allocvec(header)?;
        pos += format::write_record(&mut out, Kind::Header, &[&header_bytes])?;
        Ok(Self {
            out,
            pos,
            tables: Tables::default(),
            index: Index::default(),
            prev: None,
            since_key: 0,
            keyframe_every: keyframe_every.max(1),
            stats: RecorderStats {
                bytes: pos,
                ..RecorderStats::default()
            },
            finished: false,
        })
    }

    /// Append one frame. Clones the snapshot to keep as the base of the next delta;
    /// [`write_shared`](Recorder::write_shared) avoids that when an `Arc` is at hand.
    ///
    /// # Errors
    /// If the file cannot be written, or the recorder is finished.
    pub fn write(&mut self, snap: &Snapshot) -> Result<(), Error> {
        self.write_shared(&Arc::new(snap.clone()))
    }

    /// Append one frame.
    ///
    /// # Errors
    /// If the file cannot be written, or the recorder is finished.
    pub fn write_shared(&mut self, snap: &Arc<Snapshot>) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Finished);
        }
        let key = self.prev.is_none() || self.since_key >= self.keyframe_every;
        let raw = {
            let prev = if key { None } else { self.prev.as_deref() };
            let body = frame::encode(snap, prev, &mut self.tables);
            postcard::to_allocvec(&body)?
        };
        // Tables first: a frame's ids must already be defined when a reader
        // scanning the file reaches it.
        self.flush_tables()?;

        let offset = self.pos;
        let prefix = format::frame_prefix(snap.tick.0, format::unix_ms(snap.taken_at));
        let packed = format::compress(&raw);
        let kind = if key {
            Kind::KeyFrame
        } else {
            Kind::DeltaFrame
        };
        let n = format::write_record(&mut self.out, kind, &[&prefix, &packed])?;
        self.pos += n;
        self.stats.bytes += n;
        self.stats.frame_bytes += n;
        self.stats.frames += 1;
        self.stats.keyframes += u64::from(key);
        self.index.frames.push(FrameEntry {
            offset,
            key,
            tick: snap.tick.0,
            at_unix_ms: format::unix_ms(snap.taken_at),
        });

        self.prev = Some(Arc::clone(snap));
        self.since_key = if key { 1 } else { self.since_key + 1 };
        if self.stats.frames.is_multiple_of(SWEEP_EVERY) {
            self.tables.sweep();
        }
        Ok(())
    }

    /// What has been written so far.
    #[must_use]
    pub fn stats(&self) -> RecorderStats {
        self.stats
    }

    /// Write the index and trailer and flush. After this the file is complete.
    ///
    /// # Errors
    /// If the file cannot be written.
    pub fn finish(mut self) -> Result<RecorderStats, Error> {
        self.finish_inner()?;
        Ok(self.stats)
    }

    fn finish_inner(&mut self) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        // Whatever happens below, do not try again from `drop`.
        self.finished = true;
        let index = postcard::to_allocvec(&self.index)?;
        let index_len = u32::try_from(index.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "index exceeds 4 GiB")
        })?;
        let n = format::write_record(&mut self.out, Kind::Index, &[&index])?;
        self.out.write_all(&index_len.to_le_bytes())?;
        self.out.write_all(format::INDEX_MAGIC)?;
        self.out.flush()?;
        self.pos += n + format::TRAILER_LEN;
        self.stats.bytes = self.pos;
        Ok(())
    }

    /// Write the values numbered since the last frame, one record per table.
    fn flush_tables(&mut self) -> Result<(), Error> {
        let Tables {
            statics,
            windows,
            process_services,
            disks,
            adapters,
            gpus,
            services,
        } = &mut self.tables;
        let records = [
            (Kind::Statics, postcard_pending(&statics.take_pending())?),
            (Kind::Windows, postcard_pending(&windows.take_pending())?),
            (
                Kind::ProcessServices,
                postcard_pending(&process_services.take_pending())?,
            ),
            (Kind::Disks, postcard_pending(&disks.take_pending())?),
            (Kind::Adapters, postcard_pending(&adapters.take_pending())?),
            (Kind::Gpus, postcard_pending(&gpus.take_pending())?),
            (Kind::Services, postcard_pending(&services.take_pending())?),
        ];
        for (kind, raw) in records {
            let Some(raw) = raw else { continue };
            let packed = format::compress(&raw);
            self.index.tables.push(self.pos);
            let n = format::write_record(&mut self.out, kind, &[&packed])?;
            self.pos += n;
            self.stats.bytes += n;
            self.stats.table_bytes += n;
        }
        Ok(())
    }
}

/// Serialize a table's pending values, or `None` when there are none.
fn postcard_pending<T: ?Sized + Serialize>(
    pending: &[(u32, Arc<T>)],
) -> Result<Option<Vec<u8>>, Error> {
    if pending.is_empty() {
        return Ok(None);
    }
    Ok(Some(postcard::to_allocvec(pending)?))
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // A clean drop finishes the file; an error here has nowhere to go, and the
        // file is still readable by scanning.
        let _ = self.finish_inner();
    }
}
