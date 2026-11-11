use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ot_model::battery::BatterySample;
use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
use ot_model::device::{AdapterInfo, AdapterSample, DiskInfo, DiskSample};
use ot_model::gpu::{GpuInfo, GpuSample};
use ot_model::memory::MemorySample;
use ot_model::process::{Architecture, Integrity, ProcessSample, ProcessStatic};
use ot_model::{Bytes, Percent, ProcessKey, Tick, Watts};

use super::*;
use crate::snapshot::Snapshot;
use crate::usage::Frame;

/// A billion cycles.
const G: u64 = 1_000_000_000;
/// The session's start: 2026-11-10 12:00 PM PST (20:00Z).
const T0: u64 = 1_794_340_800_000;

/// A process with every fact filled in, so the layout test covers the inner
/// types of every field.
fn statics(pid: u32, name: &str) -> Arc<ProcessStatic> {
    Arc::new(ProcessStatic {
        key: ProcessKey::new(pid, 1000 + u64::from(pid)),
        parent: (pid > 1).then(|| ProcessKey::new(1, 1001)),
        name: name.to_owned(),
        image_path: Some(format!(r"C:\Apps\{name}")),
        command_line: Some(format!("{name} --flag")),
        user: Some("HOST\\someone".into()),
        integrity: Integrity::High,
        started_unix_ms: Some(i64::try_from(T0).unwrap() - 60_000),
        session_id: 1,
        architecture: Architecture::X64,
        description: Some("A program".into()),
        company: Some("A company".into()),
        package: Some("Some.Package_1.0_x64__abc".into()),
        parent_pid: Some(1),
    })
}

/// The machine `step` samples (of `interval_ms` each) into the session, with
/// tick `tick`. Process 3 exits after step 280, so a session past that holds a
/// ghost.
fn machine(step: u64, tick: u64, interval_ms: u64) -> Snapshot {
    let at = T0 + step * 1000;
    let load = (step % 7) as f32 * 10.0;
    let procs = [(1, "system.exe", 1), (2, "app.exe", 3), (3, "brief.exe", 5)]
        .into_iter()
        .filter(|&(pid, _, _)| pid != 3 || step <= 280)
        .map(|(pid, name, rate)| ProcessSample {
            statics: statics(pid, name),
            cpu: Percent(load),
            cpu_time: Duration::from_millis(step * rate * 10),
            cycles: 10 * G + step * rate * G,
            private_working_set: Bytes(u64::from(pid) << 20),
            gpu_share: Percent(f32::from(pid as u8)),
            gpu_dedicated: Bytes(u64::from(pid) << 10),
            gpu_shared: Bytes(u64::from(pid) << 8),
            ..ProcessSample::default()
        })
        .collect();
    Snapshot {
        tick: Tick(tick),
        taken_at: Some(UNIX_EPOCH + Duration::from_millis(at)),
        interval: Duration::from_millis(interval_ms),
        cpu: CpuSample {
            total: Percent(load),
            cores: (0..4)
                .map(|i| LogicalCore {
                    index: i,
                    physical: i / 2,
                    kind: CoreKind::Unknown,
                    usage: Percent(load + i as f32),
                    frequency: None,
                })
                .collect(),
            ..CpuSample::default()
        },
        memory: MemorySample {
            total: Bytes(16 << 30),
            available: Bytes((8 << 30) + step * 4096),
            ..MemorySample::default()
        },
        processes: procs,
        disks: vec![DiskSample {
            info: Arc::new(DiskInfo {
                number: 0,
                ..DiskInfo::default()
            }),
            active: Percent(load / 2.0),
            read_per_sec: Bytes(step * 100),
            write_per_sec: Bytes(step * 200),
            response_ms: None,
        }],
        adapters: vec![AdapterSample {
            info: Arc::new(AdapterInfo {
                id: 77,
                ..AdapterInfo::default()
            }),
            rx_per_sec: Bytes(step * 5),
            tx_per_sec: Bytes(step * 6),
            link_bps: None,
        }],
        gpus: vec![GpuSample {
            info: Arc::new(GpuInfo {
                id: 9,
                ..GpuInfo::default()
            }),
            utilization: Percent(load),
            engines: Vec::new(),
            dedicated_used: Bytes(1 << 30),
            shared_used: Bytes(0),
        }],
        battery: Some(BatterySample {
            charge: Some(80.0 - step as f32 / 10.0),
            rate: Some(Watts(-12.0)),
            ..BatterySample::default()
        }),
        ..Snapshot::default()
    }
}

/// Fine enough tiers that a short test fills them: raw samples, then 10 s and
/// 60 s buckets.
fn retention() -> Retention {
    Retention {
        raw: 30,
        tiers: vec![
            crate::timeline::Resolution {
                bucket_ms: 10_000,
                capacity: 20,
            },
            crate::timeline::Resolution {
                bucket_ms: 60_000,
                capacity: 10,
            },
        ],
    }
}

/// A session `steps` seconds long, with history short enough that frames fold
/// into steps and fade out.
fn session(steps: u64) -> (Timeline, Usage) {
    let mut timeline = Timeline::new(retention());
    let mut usage = Usage::new(0.05);
    usage.set_history_span(120_000);
    for step in 0..=steps {
        let interval = if step == 0 { 0 } else { 1000 };
        let snap = machine(step, step + 1, interval);
        timeline.observe(&snap);
        usage.observe(&snap);
    }
    (timeline, usage)
}

fn history(series: &crate::timeline::Series) -> Vec<crate::timeline::Bucket> {
    series.history().collect()
}

fn frames(usage: &Usage) -> Vec<String> {
    usage
        .frames()
        .map(|f: Frame<'_>| format!("{f:?}"))
        .collect()
}

fn totals(usage: &Usage) -> Vec<(ProcessKey, u64, bool)> {
    let mut t: Vec<_> = usage
        .iter()
        .map(|(k, u)| (k, u.used.to_bits(), u.alive))
        .collect();
    t.sort_unstable();
    t
}

fn assert_same(a: &(Timeline, Usage), b: &(Timeline, Usage)) {
    assert_eq!(history(&a.0.cpu_total), history(&b.0.cpu_total));
    assert_eq!(a.0.cores.len(), b.0.cores.len());
    for (x, y) in a.0.cores.iter().zip(&b.0.cores) {
        assert_eq!(history(x), history(y));
    }
    assert_eq!(history(&a.0.mem_in_use), history(&b.0.mem_in_use));
    let (da, db) = (a.0.disk(0).unwrap(), b.0.disk(0).unwrap());
    assert_eq!(history(&da.write), history(&db.write));
    let (na, nb) = (a.0.adapter(77).unwrap(), b.0.adapter(77).unwrap());
    assert_eq!(history(&na.rx), history(&nb.rx));
    let (ga, gb) = (a.0.gpu(9).unwrap(), b.0.gpu(9).unwrap());
    assert_eq!(history(&ga.utilization), history(&gb.utilization));
    assert_eq!(history(&a.0.battery_charge), history(&b.0.battery_charge));
    assert_eq!(frames(&a.1), frames(&b.1));
    assert_eq!(totals(&a.1), totals(&b.1));
    assert_eq!(a.1.faded_out(), b.1.faded_out());
    assert_eq!(a.1.programs(), b.1.programs());
}

fn round_trip(timeline: &Timeline, usage: &Usage) -> (Timeline, Usage) {
    let bytes = encode(timeline, usage, "1.2.3", 42, 7);
    let (meta, t, u) = decode(&bytes, &retention()).expect("decodes");
    assert_eq!(
        meta,
        Meta {
            format: FORMAT,
            app_version: "1.2.3".into(),
            written_unix_ms: 42,
            pid: 7,
        }
    );
    (t, u)
}

#[test]
fn a_round_trip_keeps_everything() {
    let (timeline, usage) = session(300);
    assert!(
        usage.faded_out().iter().any(|&t| t > 0.0),
        "some history faded out"
    );
    assert!(
        usage.iter().any(|(_, u)| !u.alive),
        "the history holds a ghost"
    );
    let restored = round_trip(&timeline, &usage);
    assert_same(&(timeline.clone(), usage.clone()), &restored);
    assert_eq!(
        payload(&timeline, &usage),
        payload(&restored.0, &restored.1),
        "it writes the same bytes again"
    );
}

#[test]
fn the_successor_carries_on_as_if_it_had_never_stopped() {
    let (mut timeline, mut usage) = session(200);
    let mut next = round_trip(&timeline, &usage);
    // The update takes 15 s. The old instance would have gone on to see the
    // samples after the gap; the new one first takes a pass with no rates (its
    // own tick 1, which the old one also used), then the same samples.
    let first_pass = machine(214, 1, 0);
    next.0.observe(&first_pass);
    next.1.observe(&first_pass);
    for (i, step) in (215..=260).enumerate() {
        let old = machine(step, step + 1, 1000);
        timeline.observe(&old);
        usage.observe(&old);
        let new = machine(step, i as u64 + 2, 1000);
        next.0.observe(&new);
        next.1.observe(&new);
    }
    assert_same(&(timeline, usage), &next);
}

#[test]
fn the_cycles_used_across_the_gap_are_counted_and_the_cpu_is_not_called_idle() {
    let (_, usage) = session(100);
    let (_, mut next) = round_trip(&Timeline::new(retention()), &usage);
    let before = next.frames().count();
    // A first pass 13 s later: nothing recorded yet.
    next.observe(&machine(113, 1, 0));
    assert_eq!(next.frames().count(), before);
    // The first pass with rates, 250 ms after it.
    let mut rated = machine(113, 2, 250);
    rated.taken_at = rated.taken_at.map(|t| t + Duration::from_millis(250));
    next.observe(&rated);
    let gap = next.frames().next().expect("a frame across the gap");
    assert_eq!(gap.span_ms, 13_250);
    // Process 2 uses 3 G a step; 13 steps passed.
    let app = next.program("app.exe").unwrap();
    let used = gap.cycles.iter().find(|c| c.0 == app).unwrap().1;
    assert!((f64::from(used) - 39.0 * G as f64).abs() < 1e-3 * 39.0 * G as f64);
    assert!(gap.machine.cpu_busy_ms > 0.0, "{:?}", gap.machine);
}

#[test]
fn the_readers_retention_decides_what_is_kept() {
    let (timeline, usage) = session(300);
    let bytes = encode(&timeline, &usage, "1.2.3", 0, 0);
    let small = Retention {
        raw: 5,
        tiers: vec![crate::timeline::Resolution {
            bucket_ms: 10_000,
            capacity: 3,
        }],
    };
    // A tier the writer did not have starts empty.
    let other = Retention {
        raw: 5,
        tiers: vec![crate::timeline::Resolution {
            bucket_ms: 30_000,
            capacity: 3,
        }],
    };
    // The same as changing the setting in the instance that wrote it.
    for r in [small, other] {
        let (_, t, _) = decode(&bytes, &r).unwrap();
        let mut changed = timeline.clone();
        changed.set_retention(r.clone());
        assert_eq!(t.retention(), &r);
        assert_eq!(history(&t.cpu_total), history(&changed.cpu_total));
        assert_eq!(history(&t.cores[3]), history(&changed.cores[3]));
    }
    let (_, t, _) = decode(&bytes, &retention()).unwrap();
    assert_eq!(history(&t.cpu_total), history(&timeline.cpu_total));
}

#[test]
fn a_newer_or_older_format_is_refused_and_named() {
    let (timeline, usage) = session(30);
    let mut bytes = encode(&timeline, &usage, "9.9.9", 5, 6);
    bytes[8..12].copy_from_slice(&(FORMAT + 1).to_le_bytes());
    assert_eq!(meta(&bytes).unwrap().format, FORMAT + 1);
    let err = decode(&bytes, &retention()).unwrap_err();
    assert_eq!(
        err,
        Error::UnknownFormat {
            format: FORMAT + 1,
            app_version: "9.9.9".into()
        }
    );
    assert!(err.to_string().contains("v9.9.9"), "{err}");
}

#[test]
fn damage_is_caught_and_never_panics() {
    let (timeline, usage) = session(60);
    let bytes = encode(&timeline, &usage, "1.2.3", 0, 0);
    assert_eq!(
        decode(b"MZ\x90\x00 not ours", &retention()).unwrap_err(),
        Error::NotHandoff
    );
    for len in 0..bytes.len() {
        assert!(decode(&bytes[..len], &retention()).is_err(), "cut at {len}");
    }
    let mut flipped = bytes.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x40;
    assert!(matches!(
        decode(&flipped, &retention()),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn a_payload_damaged_past_the_checksum_is_refused_or_harmless() {
    // Damage the payload itself and frame it with a good checksum: postcard and
    // the checks after it must stop anything that would trip the app.
    let (timeline, usage) = session(60);
    let good = payload(&timeline, &usage);
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..3000 {
        let mut damaged = good.clone();
        for _ in 0..=next() % 4 {
            let at = (next() % damaged.len() as u64) as usize;
            damaged[at] = next() as u8;
        }
        if let Ok((_, mut t, mut u)) = decode(&frame(&damaged, "x", 0, 0), &retention()) {
            // Whatever got through must work as a live one does.
            let snap = machine(400, 1, 1000);
            t.observe(&snap);
            u.observe(&snap);
            let _ = u.frames().count();
            let _ = t.cpu_total.history().count();
        }
    }
}

/// A file with `history` as its payload.
fn forged(history: &HistoryV1) -> Vec<u8> {
    frame(&postcard::to_allocvec(history).unwrap(), "x", 0, 0)
}

fn dto() -> HistoryV1 {
    let (timeline, usage) = session(60);
    HistoryV1 {
        timeline: timeline.to_handoff(),
        usage: usage.to_handoff(),
    }
}

fn refused(change: impl FnOnce(&mut HistoryV1)) -> String {
    let mut h = dto();
    change(&mut h);
    match decode(&forged(&h), &retention()) {
        Err(Error::Invalid(why)) => why,
        other => panic!("accepted: {:?}", other.map(|(m, _, _)| m)),
    }
}

#[test]
fn what_would_trip_the_app_is_refused() {
    assert!(decode(&forged(&dto()), &retention()).is_ok());
    refused(|h| h.usage.procs[0].program = 99);
    refused(|h| h.usage.seen[0].program = 99);
    refused(|h| h.usage.raw[0].lists[0].push((99, 1.0)));
    refused(|h| h.usage.raw[0].lists[0].reverse());
    refused(|h| {
        let p = h.usage.programs[0].clone();
        h.usage.programs.push(p);
    });
    refused(|h| h.usage.raw.swap(0, 1));
    refused(|h| h.usage.now_ms = 0);
    refused(|h| h.usage.procs[0].total = f64::NAN);
    refused(|h| h.usage.faded_out.resize(99, 0.0));
    refused(|h| {
        let p = &h.usage.procs[0];
        let again = ProcV1 {
            statics: p.statics.clone(),
            ..*p
        };
        h.usage.procs.push(again);
    });
    refused(|h| h.timeline.cpu_total.raw.reverse());
    refused(|h| h.timeline.cpu_total.tiers[0].bucket_ms = 0);
    refused(|h| h.timeline.cpu_total.tiers[0].closed.reverse());
    refused(|h| h.timeline.cores = (0..5000).map(|_| SeriesV1::empty()).collect());
    refused(|h| {
        let d = DiskSeriesV1 {
            number: 0,
            active: SeriesV1::empty(),
            read: SeriesV1::empty(),
            write: SeriesV1::empty(),
        };
        h.timeline.disks.push(d);
    });
}

impl SeriesV1 {
    fn empty() -> Self {
        Self {
            raw: Vec::new(),
            tiers: Vec::new(),
        }
    }
}

// The layout pin. `testdata/handoff-v<FORMAT>.bin` is a file of the current
// format; `OT_BLESS=1 cargo test -p ot-core handoff` writes it. Older ones stay,
// to prove they are refused.

fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name)
}

#[test]
fn the_payload_layout_is_pinned() {
    let (timeline, usage) = session(150);
    let path = testdata(&format!("handoff-v{FORMAT}.bin"));
    if std::env::var_os("OT_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, encode(&timeline, &usage, "0.0.0-test", 0, 0)).unwrap();
    }
    let pinned = std::fs::read(&path).expect("the pinned file; OT_BLESS=1 writes it");
    let (meta, body) = split(&pinned).unwrap();
    assert_eq!(meta.format, FORMAT);
    assert!(
        unpack(&body).unwrap() == payload(&timeline, &usage),
        "The handoff payload's layout changed. Bump handoff::FORMAT, say what \
         changed in its docs, and run `OT_BLESS=1 cargo test -p ot-core handoff` \
         to pin the new one; keep the old file, which must now be refused."
    );
    decode(&pinned, &retention()).expect("the pinned file decodes");
}

#[test]
fn every_older_format_is_refused() {
    let current = format!("handoff-v{FORMAT}.bin");
    for entry in std::fs::read_dir(testdata("")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("handoff-v") || name == current {
            continue;
        }
        let bytes = std::fs::read(entry.path()).unwrap();
        assert!(
            matches!(
                decode(&bytes, &retention()),
                Err(Error::UnknownFormat { .. })
            ),
            "{name}"
        );
    }
}
