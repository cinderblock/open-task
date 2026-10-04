//! Writing a synthetic recording and reading it back.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
use ot_model::device::{AdapterInfo, AdapterSample, DiskInfo, DiskSample, LinkKind};
use ot_model::gpu::{EngineSample, GpuInfo, GpuSample};
use ot_model::hardware::Hardware;
use ot_model::memory::MemorySample;
use ot_model::process::{ProcessSample, ProcessStatic, WindowInfo};
use ot_model::service::{ServiceEntry, ServiceInfo, ServiceState, StartType};
use ot_model::snapshot::Snapshot;
use ot_model::thread::{ServiceTag, ThreadSample, ThreadState, WaitReason};
use ot_model::{Bytes, Capabilities, Hertz, Percent, ProcessKey, Tick};
use ot_record::{Error, RecordHeader, Recorder, Recording};

/// A file under the target directory, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join("ot-record-tests");
        fs::create_dir_all(&dir).unwrap();
        Self(dir.join(format!("{name}-{}.otrec", std::process::id())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn header() -> RecordHeader {
    RecordHeader {
        app_version: "0.8.0-test".to_owned(),
        hardware: Hardware {
            cpu_name: Some("Test CPU".to_owned()),
            logical_processors: 4,
            physical_cores: 2,
            sockets: 1,
            base_frequency: Some(Hertz::from_mhz(3000)),
            ..Hardware::default()
        },
        capabilities: Capabilities {
            per_process_cpu: true,
            threads: true,
            ..Capabilities::default()
        },
        started: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        interval: Duration::from_secs(1),
    }
}

/// The shared values a synthetic session hands out, the way the probe does: one
/// `Arc` per value, reused across frames.
struct World {
    statics: Vec<Arc<ProcessStatic>>,
    window: Arc<WindowInfo>,
    host_services: Arc<[ServiceInfo]>,
    no_services: Arc<[ServiceInfo]>,
    disk: Arc<DiskInfo>,
    adapter: Arc<AdapterInfo>,
    gpu: Arc<GpuInfo>,
    services: Arc<[ServiceEntry]>,
}

impl World {
    fn new(processes: usize) -> Self {
        Self {
            statics: (0..processes)
                .map(|i| {
                    Arc::new(ProcessStatic {
                        key: ProcessKey::new(100 + i as u32 * 4, 1_000_000 + i as u64),
                        name: format!("proc{i}.exe"),
                        image_path: Some(format!(r"C:\Program Files\proc{i}\proc{i}.exe")),
                        command_line: Some(format!("proc{i}.exe --flag {i}")),
                        user: Some(r"DESKTOP\user".to_owned()),
                        session_id: 1,
                        ..ProcessStatic::default()
                    })
                })
                .collect(),
            window: Arc::new(WindowInfo {
                handle: 0xABCD,
                title: "A window".to_owned(),
                hung: false,
            }),
            host_services: vec![
                ServiceInfo {
                    name: "Dhcp".into(),
                    display_name: "DHCP Client".into(),
                    state: ServiceState::Running,
                    dll: Some("dhcpcore.dll".into()),
                },
                ServiceInfo {
                    name: "EventLog".into(),
                    display_name: "Windows Event Log".into(),
                    state: ServiceState::Running,
                    dll: None,
                },
            ]
            .into(),
            no_services: Vec::new().into(),
            disk: Arc::new(DiskInfo {
                number: 0,
                name: "Disk 0 (C:)".to_owned(),
                model: Some("Test NVMe".to_owned()),
                ssd: Some(true),
                capacity: Some(Bytes(1 << 40)),
                removable: false,
                bus: Some("NVMe".to_owned()),
            }),
            adapter: Arc::new(AdapterInfo {
                id: 7,
                name: "Ethernet".to_owned(),
                adapter: "Test NIC".to_owned(),
                kind: LinkKind::Ethernet,
                hardware: true,
                addresses: vec!["192.168.1.2".parse().unwrap(), "fe80::1".parse().unwrap()],
                dns_suffix: None,
                mac: Some("00-11-22-33-44-55".to_owned()),
            }),
            gpu: Arc::new(GpuInfo {
                id: 1,
                name: "GPU 0".to_owned(),
                adapter: "Test GPU".to_owned(),
                dedicated_total: Some(Bytes(8 << 30)),
                ..GpuInfo::default()
            }),
            services: vec![ServiceEntry {
                name: "Dhcp".into(),
                display_name: "DHCP Client".into(),
                description: Some("Registers and updates IP addresses".into()),
                state: ServiceState::Running,
                start: StartType::Automatic,
                pid: Some(104),
                group: Some("LocalServiceNetworkRestricted".into()),
                can_stop: true,
            }]
            .into(),
        }
    }

    /// Frame `n`. Process `i` has `i % 3 + 1` threads; even processes are busy, odd
    /// ones idle. Process 2 is a window owner, process 1 a service host. From
    /// frame 3 on, the last process is gone and a new one has appeared.
    fn process(&self, i: usize, n: u64, threads: &mut Vec<ThreadSample>) -> ProcessSample {
        let busy = i.is_multiple_of(2);
        let thread_first = threads.len() as u32;
        let rows = i % 3 + 1;
        for t in 0..rows {
            threads.push(ThreadSample {
                tid: 1000 + i as u32 * 10 + t as u32,
                birth: 5_000_000 + (i * 10 + t) as u64,
                cpu: Percent(if busy { n as f32 * 0.5 + t as f32 } else { 0.0 }),
                state: if busy && t == 0 {
                    ThreadState::Running
                } else {
                    ThreadState::Waiting
                },
                wait_reason: WaitReason(if busy { 0 } else { 4 }),
                service: if i == 1 {
                    ServiceTag::Service(t as u16)
                } else {
                    ServiceTag::Unknown
                },
                started_unix_ms: Some(1_700_000_000_000 + i64::try_from(i).unwrap()),
            });
        }
        ProcessSample {
            statics: Arc::clone(&self.statics[i]),
            cpu: Percent(if busy { n as f32 * 1.5 } else { 0.0 }),
            cpu_time: Duration::from_millis(if busy { n * 100 + i as u64 } else { 7 }),
            cycles: if busy { n * 1_000_000 + i as u64 } else { 42 },
            working_set: Bytes(10_000_000 + i as u64 * 4096 + if busy { n * 4096 } else { 0 }),
            private_bytes: Bytes(5_000_000 + i as u64 * 4096),
            threads: rows as u32,
            handles: 100 + i as u32,
            window: (i == 2).then(|| Arc::clone(&self.window)),
            services: if i == 1 {
                Arc::clone(&self.host_services)
            } else {
                Arc::clone(&self.no_services)
            },
            gpu_engine: (i == 4).then(|| Arc::from("GPU 0 - 3D")),
            thread_first,
            thread_rows: rows as u32,
            ..ProcessSample::default()
        }
    }

    fn snapshot(&self, n: u64) -> Snapshot {
        let mut processes = Vec::new();
        let mut threads = Vec::new();
        let count = self.statics.len();
        for i in 0..count {
            if (n >= 3 && i == count - 1) || (n < 3 && i == count - 2) {
                continue;
            }
            processes.push(self.process(i, n, &mut threads));
        }
        Snapshot {
            tick: Tick(n + 10),
            taken_at: Some(UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + n * 1000)),
            interval: Duration::from_millis(if n == 0 { 0 } else { 1000 + n }),
            probe_cost: Duration::from_micros(2500 + n),
            cpu: CpuSample {
                total: Percent(n as f32 * 3.0),
                cores: (0..4)
                    .map(|c| LogicalCore {
                        index: c,
                        physical: c / 2,
                        kind: if c < 2 {
                            CoreKind::Performance
                        } else {
                            CoreKind::Efficiency
                        },
                        usage: Percent(n as f32 + c as f32),
                        frequency: Some(Hertz::from_mhz(3000 + n * 10)),
                    })
                    .collect(),
                package_power: None,
                hotspot_celsius: Some(55.5),
            },
            memory: MemorySample {
                total: Bytes(32 << 30),
                available: Bytes((16 << 30) - n * 1024),
                ..MemorySample::default()
            },
            processes,
            threads,
            disks: vec![DiskSample {
                info: Arc::clone(&self.disk),
                active: Percent(n as f32),
                read_per_sec: Bytes(n * 1000),
                write_per_sec: Bytes(0),
                response_ms: Some(0.3),
            }],
            adapters: vec![AdapterSample {
                info: Arc::clone(&self.adapter),
                rx_per_sec: Bytes(n * 10),
                tx_per_sec: Bytes(n),
                link_bps: Some(1_000_000_000),
            }],
            gpus: vec![GpuSample {
                info: Arc::clone(&self.gpu),
                utilization: Percent(n as f32 * 2.0),
                engines: vec![EngineSample {
                    name: "3D".into(),
                    usage: Percent(n as f32 * 2.0),
                }],
                dedicated_used: Bytes(1 << 30),
                shared_used: Bytes(0),
            }],
            battery: None,
            volumes: Vec::new(),
            sessions: Vec::new(),
            services: Arc::clone(&self.services),
            capabilities: header().capabilities,
            hardware: Arc::new(Hardware::default()),
        }
    }
}

/// `expected` and `got` describe the same system, ignoring the hardware the
/// reader takes from the header.
fn assert_same(expected: &Snapshot, got: &Snapshot, what: &str) {
    let mut expected = expected.clone();
    expected.hardware = Arc::clone(&got.hardware);
    assert_eq!(&expected, got, "{what}");
}

fn record(path: &TempFile, world: &World, frames: u64, keyframe_every: u32) -> Vec<Snapshot> {
    let mut rec = Recorder::create_with_keyframes(&path.0, &header(), keyframe_every).unwrap();
    let snaps: Vec<Snapshot> = (0..frames).map(|n| world.snapshot(n)).collect();
    for s in &snaps {
        rec.write(s).unwrap();
    }
    let stats = rec.finish().unwrap();
    assert_eq!(stats.frames, frames);
    assert_eq!(stats.bytes, fs::metadata(&path.0).unwrap().len());
    snaps
}

#[test]
fn round_trip_shares_one_arc_per_value_across_frames() {
    let path = TempFile::new("roundtrip");
    let world = World::new(12);
    let snaps = record(&path, &world, 8, 4);

    let rec = Recording::open(&path.0).unwrap();
    assert!(rec.had_index());
    assert_eq!(rec.len(), 8);
    assert_eq!(rec.header(), &header());
    assert_eq!(rec.duration(), Duration::from_secs(7));
    assert_eq!(rec.tick_at(3), Some(Tick(13)));
    assert_eq!(rec.time_at(2), snaps[2].taken_at);
    assert_eq!(rec.tick_at(8), None);
    assert!(rec.is_keyframe(0) && rec.is_keyframe(4));
    assert!(!rec.is_keyframe(1) && !rec.is_keyframe(7));
    // 12 processes, 11 in each frame, every one seen at some point.
    assert_eq!(rec.processes_seen(), 12);

    let frames: Vec<Arc<Snapshot>> = (0..8).map(|i| rec.frame(i).unwrap()).collect();
    for (i, (expected, got)) in snaps.iter().zip(&frames).enumerate() {
        assert_same(expected, got, &format!("frame {i}"));
        assert!(Arc::ptr_eq(&got.hardware, rec.hardware()));
        assert_eq!(got.hardware.as_ref(), &header().hardware);
    }

    // Every shared value is one Arc across every frame that has it.
    let first = &frames[0];
    for got in &frames[1..] {
        for p in &got.processes {
            if let Some(q) = first.processes.iter().find(|q| q.key() == p.key()) {
                assert!(
                    Arc::ptr_eq(&p.statics, &q.statics),
                    "statics of {}",
                    p.name()
                );
                assert!(
                    Arc::ptr_eq(&p.services, &q.services),
                    "services of {}",
                    p.name()
                );
                match (&p.window, &q.window) {
                    (Some(a), Some(b)) => assert!(Arc::ptr_eq(a, b)),
                    (None, None) => {}
                    _ => panic!("window of {} differs", p.name()),
                }
            }
        }
        assert!(Arc::ptr_eq(&got.disks[0].info, &first.disks[0].info));
        assert!(Arc::ptr_eq(&got.adapters[0].info, &first.adapters[0].info));
        assert!(Arc::ptr_eq(&got.gpus[0].info, &first.gpus[0].info));
        assert!(Arc::ptr_eq(&got.services, &first.services));
    }
    // Processes without services all share the one empty list.
    let empties: Vec<&ProcessSample> = first
        .processes
        .iter()
        .filter(|p| p.services.is_empty())
        .collect();
    assert!(empties.len() > 2);
    assert!(empties
        .windows(2)
        .all(|w| Arc::ptr_eq(&w[0].services, &w[1].services)));
}

#[test]
fn changed_statics_get_a_new_arc_and_unchanged_keep_theirs() {
    let path = TempFile::new("generation");
    let mut world = World::new(6);
    let mut rec = Recorder::create_with_keyframes(&path.0, &header(), 100).unwrap();
    let a = world.snapshot(0);
    rec.write(&a).unwrap();
    rec.write(&world.snapshot(1)).unwrap();
    // Process 0 learns its description: the probe allocates a new ProcessStatic.
    world.statics[0] = Arc::new(ProcessStatic {
        description: Some("Process Zero".to_owned()),
        ..(*world.statics[0]).clone()
    });
    let c = world.snapshot(2);
    rec.write(&c).unwrap();
    rec.write(&world.snapshot(3)).unwrap();
    rec.finish().unwrap();

    let rec = Recording::open(&path.0).unwrap();
    assert_eq!(rec.processes_seen(), 7);
    let f1 = rec.frame(1).unwrap();
    let f2 = rec.frame(2).unwrap();
    let f3 = rec.frame(3).unwrap();
    assert_same(&c, &f2, "frame 2");
    // Frames 1 and 3 do not hold the same set of processes, so look up by key.
    let p = |s: &Snapshot, i: usize| {
        let key = world.statics[i].key;
        Arc::clone(&s.processes.iter().find(|p| p.key() == key).unwrap().statics)
    };
    assert!(!Arc::ptr_eq(&p(&f1, 0), &p(&f2, 0)));
    assert!(Arc::ptr_eq(&p(&f2, 0), &p(&f3, 0)));
    assert_eq!(p(&f1, 0).description, None);
    assert_eq!(p(&f2, 0).description.as_deref(), Some("Process Zero"));
    for i in 1..4 {
        assert!(Arc::ptr_eq(&p(&f1, i), &p(&f3, i)), "process {i}");
    }
}

#[test]
fn seeking_decodes_from_the_nearest_keyframe() {
    let path = TempFile::new("seek");
    let world = World::new(10);
    let snaps = record(&path, &world, 10, 3);
    let rec = Recording::open(&path.0).unwrap();
    // Random order: forward over a keyframe, back, far ahead, the same again.
    for i in [7usize, 2, 9, 9, 0, 5, 4, 8, 1, 6, 3] {
        let got = rec.frame(i).unwrap();
        assert_same(&snaps[i], &got, &format!("frame {i}"));
    }
    assert!(matches!(
        rec.frame(10),
        Err(Error::OutOfRange { index: 10, len: 10 })
    ));
    // Sequential after a seek reuses the cache (same content either way).
    for (i, s) in snaps.iter().enumerate() {
        assert_same(s, &rec.frame(i).unwrap(), &format!("frame {i}"));
    }
}

#[test]
fn a_file_cut_short_yields_every_complete_frame() {
    let path = TempFile::new("truncated");
    let world = World::new(8);
    let snaps = record(&path, &world, 6, 2);
    let whole = fs::read(&path.0).unwrap();
    let full_len = whole.len();

    // Without its trailer (and some of the index): scan finds all six frames.
    fs::write(&path.0, &whole[..full_len - 12]).unwrap();
    let rec = Recording::open(&path.0).unwrap();
    assert!(!rec.had_index());
    assert_eq!(rec.len(), 6);
    for (i, s) in snaps.iter().enumerate() {
        assert_same(s, &rec.frame(i).unwrap(), &format!("frame {i}"));
    }

    // Cut at every length from the end down to nothing: a file with its header
    // always opens, every frame it lists decodes, and the count never grows. Only
    // a cut inside the header itself is an error.
    let mut last_len = 6;
    let mut errors = 0;
    for cut in (0..full_len).rev().step_by(37) {
        fs::write(&path.0, &whole[..cut]).unwrap();
        match Recording::open(&path.0) {
            Ok(rec) => {
                assert!(rec.len() <= last_len, "cut at {cut}");
                last_len = rec.len();
                for (i, s) in snaps.iter().enumerate().take(rec.len()) {
                    assert_same(s, &rec.frame(i).unwrap(), &format!("cut {cut}, frame {i}"));
                }
            }
            Err(Error::Corrupt(_) | Error::NotARecording) => {
                assert_eq!(last_len, 0, "cut at {cut} lost the header with frames left");
                errors += 1;
            }
            Err(e) => panic!("cut at {cut}: {e}"),
        }
    }
    assert_eq!(last_len, 0);
    assert!(errors > 0);

    // The whole file again, with the trailer: the index is used.
    fs::write(&path.0, &whole).unwrap();
    assert!(Recording::open(&path.0).unwrap().had_index());
}

#[test]
fn dropping_an_unfinished_recorder_finishes_it() {
    let path = TempFile::new("drop");
    let world = World::new(4);
    {
        let mut rec = Recorder::create(&path.0, &header()).unwrap();
        rec.write(&world.snapshot(0)).unwrap();
        rec.write(&world.snapshot(1)).unwrap();
    }
    let rec = Recording::open(&path.0).unwrap();
    assert!(rec.had_index());
    assert_eq!(rec.len(), 2);
    assert_same(&world.snapshot(1), &rec.frame(1).unwrap(), "frame 1");
}

#[test]
fn an_empty_recording_opens() {
    let path = TempFile::new("empty");
    Recorder::create(&path.0, &header())
        .unwrap()
        .finish()
        .unwrap();
    let rec = Recording::open(&path.0).unwrap();
    assert_eq!(rec.len(), 0);
    assert!(rec.is_empty());
    assert_eq!(rec.duration(), Duration::ZERO);
    assert!(rec.had_index());
    assert!(matches!(rec.frame(0), Err(Error::OutOfRange { .. })));
}

#[test]
fn other_files_are_refused() {
    let path = TempFile::new("other");
    fs::write(&path.0, b"hello").unwrap();
    assert!(matches!(
        Recording::open(&path.0),
        Err(Error::NotARecording)
    ));
    fs::write(&path.0, b"OTREC\x63\x00").unwrap();
    assert!(matches!(
        Recording::open(&path.0),
        Err(Error::UnsupportedVersion(99))
    ));
    fs::write(&path.0, b"OTREC\x01\x00").unwrap();
    assert!(matches!(Recording::open(&path.0), Err(Error::Corrupt(_))));
    assert!(matches!(
        Recording::open(path.0.join("missing")),
        Err(Error::Io(_))
    ));
}

#[test]
fn write_after_finish_is_refused() {
    let path = TempFile::new("finished");
    let world = World::new(2);
    let mut rec = Recorder::create(&path.0, &header()).unwrap();
    rec.write(&world.snapshot(0)).unwrap();
    let stats = rec.stats();
    assert_eq!(stats.frames, 1);
    assert!(stats.table_bytes > 0 && stats.frame_bytes > 0);
    rec.finish().unwrap();
    // `finish` consumed it; a second recorder on the same path starts over.
    let mut again = Recorder::create(&path.0, &header()).unwrap();
    again.write(&world.snapshot(5)).unwrap();
    drop(again);
    let rec = Recording::open(&path.0).unwrap();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec.tick_at(0), Some(Tick(15)));
}

#[test]
fn frames_without_wall_time_are_paced_by_the_interval() {
    let path = TempFile::new("notime");
    let world = World::new(2);
    let mut rec = Recorder::create(&path.0, &header()).unwrap();
    for n in 0..3 {
        let mut s = world.snapshot(n);
        s.taken_at = None;
        rec.write(&s).unwrap();
    }
    rec.finish().unwrap();
    let rec = Recording::open(&path.0).unwrap();
    assert_eq!(rec.time_at(1), None);
    assert_eq!(rec.offset_ms(2), Some(2000));
    assert_eq!(rec.duration(), Duration::from_secs(2));
    let got = rec.frame(2).unwrap();
    assert_eq!(got.taken_at, None);
    assert_eq!(got.tick, Tick(12));
}
