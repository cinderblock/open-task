//! Writing a recording.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use ot_model::snapshot::Snapshot;
use serde::Serialize;

use crate::format::{self, FrameEntry, Index, Kind, RecordHeader, FORMAT_VERSION, MAGIC};
use crate::frame::{self, AdapterInfoV3, Tables};
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
    /// The format written: [`FORMAT_VERSION`], or an older one in tests of
    /// reading old files.
    version: u16,
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
        Self::create_as(path, header, keyframe_every, FORMAT_VERSION)
    }

    /// [`create_with_keyframes`](Recorder::create_with_keyframes), writing format
    /// `version`: 1 writes frames without their [`frame::Extra`], 2 without their
    /// [`frame::GpuExtra`], 3 adapters without their parent ([`AdapterInfoV3`]).
    pub(crate) fn create_as(
        path: impl AsRef<Path>,
        header: &RecordHeader,
        keyframe_every: u32,
        version: u16,
    ) -> Result<Self, Error> {
        let file = File::create(path)?;
        let mut out = BufWriter::with_capacity(256 << 10, file);
        out.write_all(MAGIC)?;
        out.write_all(&version.to_le_bytes())?;
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
            version,
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
            let (body, extra, gpu) = frame::encode(snap, prev, &mut self.tables);
            if self.version >= 3 {
                postcard::to_allocvec(&(body, extra, gpu))?
            } else if self.version >= 2 {
                postcard::to_allocvec(&(body, extra))?
            } else {
                postcard::to_allocvec(&body)?
            }
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
            (Kind::Adapters, {
                let pending = adapters.take_pending();
                if self.version >= 4 {
                    postcard_pending(&pending)?
                } else {
                    let old: Vec<_> = pending
                        .iter()
                        .map(|(id, a)| (*id, Arc::new(AdapterInfoV3::from(&**a))))
                        .collect();
                    postcard_pending(&old)?
                }
            }),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, Recording};
    use ot_model::device::{AdapterInfo, AdapterSample};
    use ot_model::hardware::Hardware;
    use ot_model::process::{ProcessSample, ProcessStatic};
    use ot_model::{Bytes, Capabilities, Percent, ProcessKey, Tick};
    use std::time::{Duration, SystemTime};

    /// Two frames of two processes whose private working sets and GPU use are
    /// set, the second a delta of the first, and a switch port of a network card.
    fn frames() -> Vec<Snapshot> {
        let adapter = |id, parent| AdapterSample {
            info: Arc::new(AdapterInfo {
                id,
                name: format!("adapter {id}"),
                parent,
                ..AdapterInfo::default()
            }),
            rx_per_sec: Bytes(id * 1000),
            tx_per_sec: Bytes(id),
            link_bps: None,
        };
        (0..2u64)
            .map(|n| Snapshot {
                adapters: vec![adapter(1, None), adapter(2, Some(1))],
                tick: Tick(n + 1),
                taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + n)),
                interval: Duration::from_secs(1),
                processes: (1..=2u32)
                    .map(|pid| ProcessSample {
                        statics: Arc::new(ProcessStatic {
                            key: ProcessKey::new(pid, 1),
                            name: format!("p{pid}.exe"),
                            ..ProcessStatic::default()
                        }),
                        working_set: Bytes(9 << 20),
                        private_working_set: Bytes((u64::from(pid) + n) << 20),
                        gpu_share: Percent(
                            f32::from(u8::try_from(pid + 3 * n as u32).unwrap()) * 1.25,
                        ),
                        gpu_dedicated: Bytes(u64::from(pid) << 24),
                        gpu_shared: Bytes((u64::from(pid) + 2 * n) << 22),
                        ..ProcessSample::default()
                    })
                    .collect(),
                ..Snapshot::default()
            })
            .collect()
    }

    fn write(path: &Path, version: u16) {
        let header = RecordHeader {
            app_version: "test".to_owned(),
            hardware: Hardware::default(),
            capabilities: Capabilities::default(),
            started: SystemTime::UNIX_EPOCH,
            interval: Duration::from_secs(1),
        };
        let mut rec = Recorder::create_as(path, &header, 8, version).unwrap();
        for snap in &frames() {
            rec.write(snap).unwrap();
        }
        rec.finish().unwrap();
    }

    fn parents(s: &Snapshot) -> Vec<(String, Option<u64>)> {
        s.adapters
            .iter()
            .map(|a| (a.info.name.clone(), a.info.parent))
            .collect()
    }

    #[test]
    fn private_working_sets_and_gpu_use_are_kept_and_older_files_still_read() {
        let dir = std::env::temp_dir().join("ot-record-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("versions-{}.otrec", std::process::id()));
        let want = frames();

        write(&path, FORMAT_VERSION);
        let rec = Recording::open(&path).unwrap();
        assert_eq!(rec.version(), FORMAT_VERSION);
        for (i, want) in want.iter().enumerate() {
            let got = rec.frame(i).unwrap();
            assert_eq!(parents(&got), parents(want), "frame {i}");
            let private = |s: &Snapshot| -> Vec<Bytes> {
                s.processes.iter().map(|p| p.private_working_set).collect()
            };
            assert_eq!(private(&got), private(want), "frame {i}");
            let gpu = |s: &Snapshot| -> Vec<(Percent, Bytes, Bytes)> {
                s.processes
                    .iter()
                    .map(|p| (p.gpu_share, p.gpu_dedicated, p.gpu_shared))
                    .collect()
            };
            assert_eq!(gpu(&got), gpu(want), "frame {i}");
        }

        // A version 3 file has everything but the adapters' parents.
        write(&path, 3);
        let rec = Recording::open(&path).unwrap();
        assert_eq!(rec.version(), 3);
        for (i, want) in want.iter().enumerate() {
            let got = rec.frame(i).unwrap();
            assert_eq!(
                parents(&got),
                [
                    ("adapter 1".to_owned(), None),
                    ("adapter 2".to_owned(), None)
                ],
                "frame {i}"
            );
            let gpu: Vec<_> = got.processes.iter().map(|p| p.gpu_dedicated).collect();
            let gpu_want: Vec<_> = want.processes.iter().map(|p| p.gpu_dedicated).collect();
            assert_eq!(gpu, gpu_want, "frame {i}");
        }

        // A version 2 file has the private working sets but no GPU use.
        write(&path, 2);
        let rec = Recording::open(&path).unwrap();
        assert_eq!(rec.version(), 2);
        for (i, want) in want.iter().enumerate() {
            let got = rec.frame(i).unwrap();
            for (g, w) in got.processes.iter().zip(&want.processes) {
                assert_eq!(g.private_working_set, w.private_working_set, "frame {i}");
                assert_eq!(g.gpu_share, Percent::ZERO);
                assert_eq!(g.gpu_dedicated, Bytes::ZERO);
                assert_eq!(g.gpu_shared, Bytes::ZERO);
            }
        }

        // A version 1 file has no private working sets, and everything else.
        write(&path, 1);
        let rec = Recording::open(&path).unwrap();
        assert_eq!(rec.version(), 1);
        for (i, want) in want.iter().enumerate() {
            let got = rec.frame(i).unwrap();
            assert_eq!(got.processes.len(), want.processes.len());
            for (g, w) in got.processes.iter().zip(&want.processes) {
                assert_eq!(g.private_working_set, Bytes::ZERO);
                assert_eq!(g.working_set, w.working_set);
                assert_eq!(g.statics.name, w.statics.name);
            }
        }

        std::fs::write(&path, b"OTREC\x00\x00").unwrap();
        assert!(matches!(
            Recording::open(&path),
            Err(Error::UnsupportedVersion(0))
        ));
        let _ = std::fs::remove_file(&path);
    }
}
