//! Recording from a sampler and playing it back.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ot_core::{
    Player, RecordHeader, Recorder, Recording, RecordingProbe, Sampler, SamplerConfig, Snapshot,
};
use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
use ot_model::hardware::Hardware;
use ot_model::memory::MemorySample;
use ot_model::process::{ProcessSample, ProcessStatic};
use ot_model::thread::{ServiceTag, ThreadSample, ThreadState, WaitReason};
use ot_model::{Bytes, Capabilities, Percent, ProcessKey, Tick};
use ot_probe::{ProbeError, ProbeOutput, SystemProbe};

/// A file under the temp directory, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join("ot-core-tests");
        fs::create_dir_all(&dir).unwrap();
        Self(dir.join(format!("{name}-{}.otrec", std::process::id())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A machine with eight processes, two threads each, whose counters advance every
/// pass.
#[derive(Debug)]
struct FakeProbe {
    statics: Vec<Arc<ProcessStatic>>,
    pass: u64,
}

impl FakeProbe {
    fn new() -> Self {
        Self {
            statics: (0..8)
                .map(|i| {
                    Arc::new(ProcessStatic {
                        key: ProcessKey::new(200 + i, 77 + u64::from(i)),
                        name: format!("fake{i}.exe"),
                        ..ProcessStatic::default()
                    })
                })
                .collect(),
            pass: 0,
        }
    }
}

impl SystemProbe for FakeProbe {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            per_process_cpu: true,
            threads: true,
            ..Capabilities::default()
        }
    }

    fn hardware(&self) -> Hardware {
        Hardware {
            cpu_name: Some("Fake CPU".to_owned()),
            logical_processors: 2,
            ..Hardware::default()
        }
    }

    fn sample(&mut self, out: &mut ProbeOutput) -> Result<(), ProbeError> {
        out.clear();
        self.pass += 1;
        let n = self.pass;
        out.cpu = CpuSample {
            total: Percent(n as f32),
            cores: (0..2)
                .map(|c| LogicalCore {
                    index: c,
                    physical: c,
                    kind: CoreKind::Unknown,
                    usage: Percent(n as f32 + c as f32),
                    frequency: None,
                })
                .collect(),
            package_power: None,
            hotspot_celsius: None,
        };
        out.memory = MemorySample {
            total: Bytes(1 << 34),
            available: Bytes((1 << 33) - n),
            ..MemorySample::default()
        };
        for (i, statics) in self.statics.iter().enumerate() {
            let thread_first = out.threads.len() as u32;
            for t in 0..2u32 {
                out.threads.push(ThreadSample {
                    tid: 5000 + i as u32 * 2 + t,
                    birth: 9000 + (i as u64) * 2 + u64::from(t),
                    cpu: Percent(if t == 0 { n as f32 } else { 0.0 }),
                    state: ThreadState::Waiting,
                    wait_reason: WaitReason(4),
                    service: ServiceTag::Unknown,
                    started_unix_ms: Some(1_700_000_000_000),
                });
            }
            out.processes.push(ProcessSample {
                statics: Arc::clone(statics),
                cpu: Percent(n as f32 / 2.0),
                cpu_time: Duration::from_millis(n * 10 + i as u64),
                cycles: n * 1000,
                working_set: Bytes(1 << 20),
                threads: 2,
                thread_first,
                thread_rows: 2,
                ..ProcessSample::default()
            });
        }
        Ok(())
    }
}

fn header(probe: &dyn SystemProbe, interval: Duration) -> RecordHeader {
    RecordHeader {
        app_version: "test".to_owned(),
        hardware: probe.hardware(),
        capabilities: probe.capabilities(),
        started: SystemTime::now(),
        interval,
    }
}

/// Wait until `cond` holds, or fail after `timeout`.
fn wait_for(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The recorded frames are the passes, every tick present once, in order.
fn assert_consecutive(rec: &Recording, at_least: usize) {
    assert!(rec.len() >= at_least, "{} frames", rec.len());
    for i in 1..rec.len() {
        assert_eq!(
            rec.tick_at(i).unwrap().0,
            rec.tick_at(i - 1).unwrap().0 + 1,
            "frame {i}"
        );
    }
}

#[test]
fn sampler_records_what_it_publishes() {
    let path = TempFile::new("sampler");
    let interval = Duration::from_millis(10);
    let probe = FakeProbe::new();
    let hdr = header(&probe, interval);
    let sampler = Sampler::start(Box::new(probe), SamplerConfig { interval });
    assert!(sampler.recording().is_none());
    assert!(sampler.stop_recording().is_none());

    let recorder = Recorder::create(&path.0, &hdr).unwrap();
    assert!(sampler.record_to(recorder).is_none());
    let first_recorded_tick = sampler.latest().tick.0 + 1;
    wait_for("20 passes", Duration::from_secs(10), || {
        sampler.latest().tick.0 >= first_recorded_tick + 20
    });
    let progress = sampler.recording().unwrap();
    assert!(progress.frames > 0 && progress.bytes > 0 && !progress.failed);
    let last_published = sampler.latest();
    let stats = sampler.stop_recording().unwrap().unwrap();
    assert_eq!(stats.dropped, 0);
    assert!(stats.written.frames >= 20);
    assert!(sampler.recording().is_none());

    let rec = Recording::open(&path.0).unwrap();
    assert!(rec.had_index());
    assert_eq!(rec.len() as u64, stats.written.frames);
    assert_consecutive(&rec, 20);
    // The first recorded frame is the first pass after `record_to`, not before.
    assert!(rec.tick_at(0).unwrap().0 >= first_recorded_tick.saturating_sub(1));
    // What was published while recording is in the file, exactly. (A pass may
    // have been published and written after `last_published` was read.)
    let i = (0..rec.len())
        .find(|&i| rec.tick_at(i) == Some(last_published.tick))
        .expect("the last published snapshot was recorded");
    let in_file = rec.frame(i).unwrap();
    assert_eq!(in_file.processes, last_published.processes);
    assert_eq!(in_file.threads, last_published.threads);
    assert_eq!(in_file.cpu, last_published.cpu);
    assert_eq!(in_file.memory, last_published.memory);
    assert_eq!(in_file.taken_at, last_published.taken_at);
    assert_eq!(in_file.interval, last_published.interval);
    assert_eq!(in_file.hardware.as_ref(), &hdr.hardware);
}

#[test]
fn recording_probe_records_every_pass_and_finishes_on_drop() {
    let path = TempFile::new("probe");
    let interval = Duration::from_millis(10);
    let inner = FakeProbe::new();
    let hdr = header(&inner, interval);
    let recorder = Recorder::create(&path.0, &hdr).unwrap();
    let probe = RecordingProbe::new(Box::new(inner), recorder);
    assert_eq!(probe.capabilities(), hdr.capabilities);
    assert_eq!(probe.hardware(), hdr.hardware);

    let sampler = Sampler::start(Box::new(probe), SamplerConfig { interval });
    wait_for("15 passes", Duration::from_secs(10), || {
        sampler.latest().tick.0 >= 15
    });
    let published = sampler.latest();
    drop(sampler);

    let rec = Recording::open(&path.0).unwrap();
    assert!(rec.had_index());
    assert_consecutive(&rec, 15);
    assert_eq!(rec.tick_at(0), Some(Tick(1)));
    // The wrapper numbers passes as the sampler does: the same tick holds the
    // same measurements.
    let i = usize::try_from(published.tick.0 - 1).unwrap();
    let in_file = rec.frame(i).unwrap();
    assert_eq!(in_file.tick, published.tick);
    assert_eq!(in_file.processes, published.processes);
    assert_eq!(in_file.threads, published.threads);
    assert_eq!(in_file.cpu, published.cpu);
    assert_eq!(in_file.capabilities, published.capabilities);
}

/// Five frames, one recorded second apart.
fn five_frames(path: &TempFile) -> Recording {
    let mut probe = FakeProbe::new();
    let hdr = header(&probe, Duration::from_secs(1));
    let mut rec = Recorder::create_with_keyframes(&path.0, &hdr, 2).unwrap();
    let mut out = ProbeOutput::default();
    for n in 0..5u64 {
        probe.sample(&mut out).unwrap();
        let snap = Snapshot {
            tick: Tick(n + 1),
            taken_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + n)),
            interval: Duration::from_secs(1),
            cpu: out.cpu.clone(),
            memory: out.memory,
            processes: out.processes.clone(),
            threads: out.threads.clone(),
            ..Snapshot::default()
        };
        rec.write(&snap).unwrap();
    }
    rec.finish().unwrap();
    Recording::open(&path.0).unwrap()
}

#[test]
fn player_seeks_steps_and_plays_at_speed() {
    let path = TempFile::new("player");
    let recording = five_frames(&path);
    let notified = Arc::new(AtomicUsize::new(0));
    let player = {
        let notified = Arc::clone(&notified);
        Player::with_notify(recording, move || {
            notified.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap()
    };
    assert_eq!(player.len(), 5);
    assert_eq!(player.position(), 0);
    assert!(!player.is_playing());
    assert_eq!(player.latest().tick, Tick(1));
    assert_eq!(player.interval(), Duration::from_secs(1));
    assert_eq!(player.speed(), 1.0);

    player.seek(3);
    wait_for("seek to 3", Duration::from_secs(5), || {
        player.position() == 3
    });
    assert_eq!(player.latest().tick, Tick(4));
    assert!(notified.load(Ordering::SeqCst) >= 1);

    player.seek(99);
    wait_for("seek past the end", Duration::from_secs(5), || {
        player.position() == 4
    });
    player.step(-2);
    wait_for("step back", Duration::from_secs(5), || {
        player.position() == 2
    });
    player.step(10);
    wait_for("step past the end", Duration::from_secs(5), || {
        player.position() == 4
    });

    // Four seconds of recording at 40x: a tenth of a second.
    player.set_speed(40.0);
    assert_eq!(player.speed(), 40.0);
    player.seek(0);
    wait_for("seek to 0", Duration::from_secs(5), || {
        player.position() == 0
    });
    let before = notified.load(Ordering::SeqCst);
    let started = Instant::now();
    player.play();
    assert!(player.is_playing());
    wait_for("play to the end", Duration::from_secs(10), || {
        player.position() == 4 && !player.is_playing()
    });
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(90), "{took:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert_eq!(notified.load(Ordering::SeqCst) - before, 4);
    assert_eq!(player.latest().tick, Tick(5));
    assert!(player.error().is_none());

    // Play at the end starts over.
    player.play();
    wait_for("restart", Duration::from_secs(10), || {
        player.position() == 4 && !player.is_playing()
    });
    player.pause();
    player.toggle();
    assert!(player.is_playing());
    player.toggle();
    assert!(!player.is_playing());
}

#[test]
fn feed_forwards_to_either_side() {
    let path = TempFile::new("feed");
    let recording = five_frames(&path);
    let replay = ot_core::Feed::from(Player::new(recording).unwrap());
    assert!(!replay.is_live());
    assert_eq!(replay.latest().tick, Tick(1));
    assert_eq!(replay.interval(), Duration::from_secs(1));
    replay.set_interval(Duration::from_millis(5));
    assert_eq!(replay.interval(), Duration::from_secs(1));
    assert_eq!(replay.consecutive_errors(), 0);
    assert!(replay.player().is_some() && replay.sampler().is_none());

    let interval = Duration::from_millis(10);
    let live = ot_core::Feed::from(Sampler::start(
        Box::new(FakeProbe::new()),
        SamplerConfig { interval },
    ));
    assert!(live.is_live());
    assert_eq!(live.interval(), interval);
    live.set_interval(Duration::from_millis(20));
    assert_eq!(live.interval(), Duration::from_millis(20));
    assert!(live.sampler().is_some() && live.player().is_none());
    wait_for("a pass", Duration::from_secs(5), || {
        !live.latest().is_empty()
    });
}

/// Records 30 live passes of this machine and prints what a frame costs. Run with
/// `cargo test -p ot-core --test record size_of_live_frames -- --ignored --nocapture`.
#[test]
#[ignore = "records this machine for 30 seconds"]
fn size_of_live_frames() {
    let Ok(probe) = ot_probe::PlatformProbe::new() else {
        eprintln!("no probe on this platform");
        return;
    };
    let interval = Duration::from_secs(1);
    let hdr = header(&probe, interval);
    let sampler = Sampler::start(Box::new(probe), SamplerConfig { interval });
    wait_for("the first pass", Duration::from_secs(10), || {
        !sampler.latest().is_empty() || sampler.consecutive_errors() == u64::MAX
    });
    if sampler.consecutive_errors() == u64::MAX {
        eprintln!("no probe on this platform");
        return;
    }

    let path = TempFile::new("live");
    let start_tick = sampler.latest().tick.0;
    sampler
        .record_to(Recorder::create(&path.0, &hdr).unwrap())
        .map(|r| r.unwrap());
    wait_for("30 passes", Duration::from_secs(90), || {
        sampler.latest().tick.0 >= start_tick + 30
    });
    let stats = sampler.stop_recording().unwrap().unwrap();
    let last = sampler.latest();
    drop(sampler);

    let rec = Recording::open(&path.0).unwrap();
    let frames: Vec<Arc<Snapshot>> = (0..rec.len()).map(|i| rec.frame(i).unwrap()).collect();

    // The same frames again, every one a keyframe, to show what the deltas save.
    let keyed = TempFile::new("live-keyframes");
    let mut all_key = Recorder::create_with_keyframes(&keyed.0, &hdr, 1).unwrap();
    for f in &frames {
        all_key.write_shared(f).unwrap();
    }
    let key_stats = all_key.finish().unwrap();

    let w = stats.written;
    eprintln!(
        "{} processes, {} threads in the last pass; {} frames ({} keyframes), {} dropped",
        last.processes.len(),
        last.threads.len(),
        w.frames,
        w.keyframes,
        stats.dropped
    );
    eprintln!(
        "file {} bytes = {:.0} bytes/frame; frames {} bytes, tables {} bytes",
        w.bytes,
        w.bytes_per_frame(),
        w.frame_bytes,
        w.table_bytes
    );
    let deltas = w.frames.saturating_sub(w.keyframes);
    if deltas > 0 && w.keyframes > 0 {
        // One keyframe; the rest deltas.
        let key_each = key_stats.frame_bytes as f64 / key_stats.frames as f64;
        let delta_each = (w.frame_bytes as f64 - key_each * w.keyframes as f64) / deltas as f64;
        eprintln!(
            "about {key_each:.0} bytes per keyframe and {delta_each:.0} per delta frame; all \
             keyframes would be {:.0} bytes/frame",
            key_stats.bytes_per_frame()
        );
    }
    // Where the delta bytes go: the same frames without their threads, and without
    // their processes, as delta frames only (one keyframe each, subtracted).
    let only = |name: &str, strip: &dyn Fn(&mut Snapshot)| {
        let tmp = TempFile::new(&format!("live-{name}"));
        let mut r = Recorder::create_with_keyframes(&tmp.0, &hdr, 1000).unwrap();
        let mut first = 0;
        for (i, f) in frames.iter().enumerate() {
            let mut s = (**f).clone();
            strip(&mut s);
            r.write(&s).unwrap();
            if i == 0 {
                first = r.stats().frame_bytes;
            }
        }
        let st = r.finish().unwrap();
        let deltas = st.frames.saturating_sub(1).max(1);
        eprintln!(
            "{name}: {:.0} bytes per delta frame",
            (st.frame_bytes - first) as f64 / deltas as f64
        );
    };
    only("everything", &|_| {});
    only("without threads", &|s| {
        s.threads.clear();
        for p in &mut s.processes {
            p.thread_first = 0;
            p.thread_rows = 0;
        }
    });
    only("without processes or threads", &|s| {
        s.threads.clear();
        s.processes.clear();
    });
    assert_eq!(rec.len() as u64, w.frames);
    assert!(w.frames >= 30);
}
