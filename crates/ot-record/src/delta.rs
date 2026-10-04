//! Per-process and per-thread deltas against the previous frame.
//!
//! Most of a frame is processes and threads, and most of those are idle from one
//! pass to the next: the same cumulative counters, the same memory, 0 % CPU. A
//! delta frame stores each counter as its change since the previous frame (an
//! `i64`, which postcard writes as a zigzag varint, so zero is one byte) and each
//! float as its bits exclusive-ORed with the previous value's (zero when unchanged).
//! An idle
//! process then costs a few dozen bytes of mostly zeros and an idle thread four,
//! before lz4 finds the runs. Fields that are small or rarely set (`Option`s,
//! enums, bools) are stored as they are.
//!
//! Every field of [`ProcessSample`] and [`ThreadSample`] is destructured by name
//! here, so adding a field to either is a compile error until it is encoded.

use std::sync::Arc;
use std::time::Duration;

use ot_model::process::{IoCounters, Priority, ProcessKind, ProcessSample};
use ot_model::thread::{ServiceTag, ThreadSample, ThreadState, WaitReason};
use ot_model::{Bytes, Percent, Watts};
use serde::{Deserialize, Serialize};

/// A process's change since the previous frame. Shared fields (`statics`, `window`,
/// `services`) are table ids in the frame row, and the thread range is rebuilt on
/// decode, so neither appears here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProcessDelta {
    cpu: u32,
    cpu_time: i64,
    cycles: i64,
    working_set: i64,
    private_bytes: i64,
    disk_read: i64,
    disk_write: i64,
    net_rx: i64,
    net_tx: i64,
    threads: i32,
    handles: i32,
    power: Option<Watts>,
    gpu: Option<Percent>,
    suspended: bool,
    efficiency_mode: Option<bool>,
    kind: ProcessKind,
    priority: Priority,
    page_faults: i32,
    peak_working_set: i64,
    virtual_size: i64,
    paged_pool: i64,
    nonpaged_pool: i64,
    io_reads: i64,
    io_writes: i64,
    io_other: i64,
    io_read_bytes: i64,
    io_write_bytes: i64,
    io_other_bytes: i64,
    gpu_engine: Option<Arc<str>>,
}

/// A thread's change since the previous frame, for a thread whose identity (`tid`,
/// `birth`, start time) is the same as before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ThreadDelta {
    cpu: u32,
    state: Option<ThreadState>,
    wait_reason: Option<WaitReason>,
    service: Option<ServiceTag>,
}

fn d_u64(cur: u64, prev: u64) -> i64 {
    cur.wrapping_sub(prev).cast_signed()
}

fn a_u64(prev: u64, d: i64) -> u64 {
    prev.wrapping_add(d.cast_unsigned())
}

fn d_u32(cur: u32, prev: u32) -> i32 {
    cur.wrapping_sub(prev).cast_signed()
}

fn a_u32(prev: u32, d: i32) -> u32 {
    prev.wrapping_add(d.cast_unsigned())
}

fn d_bytes(cur: Bytes, prev: Bytes) -> i64 {
    d_u64(cur.0, prev.0)
}

fn a_bytes(prev: Bytes, d: i64) -> Bytes {
    Bytes(a_u64(prev.0, d))
}

/// Nanoseconds as `u64`: 584 years, far more than any CPU time.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

fn d_duration(cur: Duration, prev: Duration) -> i64 {
    d_u64(nanos(cur), nanos(prev))
}

fn a_duration(prev: Duration, d: i64) -> Duration {
    Duration::from_nanos(a_u64(nanos(prev), d))
}

fn x_f32(cur: f32, prev: f32) -> u32 {
    cur.to_bits() ^ prev.to_bits()
}

fn u_f32(prev: f32, x: u32) -> f32 {
    f32::from_bits(prev.to_bits() ^ x)
}

fn changed<T: PartialEq + Copy>(cur: T, prev: T) -> Option<T> {
    (cur != prev).then_some(cur)
}

/// `cur` as a change from `prev`, the same process one frame earlier.
pub(crate) fn process(cur: &ProcessSample, prev: &ProcessSample) -> ProcessDelta {
    let ProcessSample {
        statics: _,
        cpu,
        cpu_time,
        cycles,
        working_set,
        private_bytes,
        disk_read,
        disk_write,
        net_rx,
        net_tx,
        threads,
        handles,
        power,
        gpu,
        suspended,
        efficiency_mode,
        window: _,
        kind,
        priority,
        page_faults,
        peak_working_set,
        virtual_size,
        paged_pool,
        nonpaged_pool,
        io,
        gpu_engine,
        services: _,
        thread_first: _,
        thread_rows: _,
    } = cur;
    ProcessDelta {
        cpu: x_f32(cpu.0, prev.cpu.0),
        cpu_time: d_duration(*cpu_time, prev.cpu_time),
        cycles: d_u64(*cycles, prev.cycles),
        working_set: d_bytes(*working_set, prev.working_set),
        private_bytes: d_bytes(*private_bytes, prev.private_bytes),
        disk_read: d_bytes(*disk_read, prev.disk_read),
        disk_write: d_bytes(*disk_write, prev.disk_write),
        net_rx: d_bytes(*net_rx, prev.net_rx),
        net_tx: d_bytes(*net_tx, prev.net_tx),
        threads: d_u32(*threads, prev.threads),
        handles: d_u32(*handles, prev.handles),
        power: *power,
        gpu: *gpu,
        suspended: *suspended,
        efficiency_mode: *efficiency_mode,
        kind: *kind,
        priority: *priority,
        page_faults: d_u32(*page_faults, prev.page_faults),
        peak_working_set: d_bytes(*peak_working_set, prev.peak_working_set),
        virtual_size: d_bytes(*virtual_size, prev.virtual_size),
        paged_pool: d_bytes(*paged_pool, prev.paged_pool),
        nonpaged_pool: d_bytes(*nonpaged_pool, prev.nonpaged_pool),
        io_reads: d_u64(io.reads, prev.io.reads),
        io_writes: d_u64(io.writes, prev.io.writes),
        io_other: d_u64(io.other, prev.io.other),
        io_read_bytes: d_bytes(io.read_bytes, prev.io.read_bytes),
        io_write_bytes: d_bytes(io.write_bytes, prev.io.write_bytes),
        io_other_bytes: d_bytes(io.other_bytes, prev.io.other_bytes),
        gpu_engine: gpu_engine.clone(),
    }
}

/// Undo [`process`]. The shared fields and the thread range come back as their
/// defaults; the caller fills them in.
pub(crate) fn apply_process(prev: &ProcessSample, d: &ProcessDelta) -> ProcessSample {
    let ProcessDelta {
        cpu,
        cpu_time,
        cycles,
        working_set,
        private_bytes,
        disk_read,
        disk_write,
        net_rx,
        net_tx,
        threads,
        handles,
        power,
        gpu,
        suspended,
        efficiency_mode,
        kind,
        priority,
        page_faults,
        peak_working_set,
        virtual_size,
        paged_pool,
        nonpaged_pool,
        io_reads,
        io_writes,
        io_other,
        io_read_bytes,
        io_write_bytes,
        io_other_bytes,
        gpu_engine,
    } = d;
    ProcessSample {
        statics: Arc::default(),
        cpu: Percent(u_f32(prev.cpu.0, *cpu)),
        cpu_time: a_duration(prev.cpu_time, *cpu_time),
        cycles: a_u64(prev.cycles, *cycles),
        working_set: a_bytes(prev.working_set, *working_set),
        private_bytes: a_bytes(prev.private_bytes, *private_bytes),
        disk_read: a_bytes(prev.disk_read, *disk_read),
        disk_write: a_bytes(prev.disk_write, *disk_write),
        net_rx: a_bytes(prev.net_rx, *net_rx),
        net_tx: a_bytes(prev.net_tx, *net_tx),
        threads: a_u32(prev.threads, *threads),
        handles: a_u32(prev.handles, *handles),
        power: *power,
        gpu: *gpu,
        suspended: *suspended,
        efficiency_mode: *efficiency_mode,
        window: None,
        kind: *kind,
        priority: *priority,
        page_faults: a_u32(prev.page_faults, *page_faults),
        peak_working_set: a_bytes(prev.peak_working_set, *peak_working_set),
        virtual_size: a_bytes(prev.virtual_size, *virtual_size),
        paged_pool: a_bytes(prev.paged_pool, *paged_pool),
        nonpaged_pool: a_bytes(prev.nonpaged_pool, *nonpaged_pool),
        io: IoCounters {
            reads: a_u64(prev.io.reads, *io_reads),
            writes: a_u64(prev.io.writes, *io_writes),
            other: a_u64(prev.io.other, *io_other),
            read_bytes: a_bytes(prev.io.read_bytes, *io_read_bytes),
            write_bytes: a_bytes(prev.io.write_bytes, *io_write_bytes),
            other_bytes: a_bytes(prev.io.other_bytes, *io_other_bytes),
        },
        gpu_engine: gpu_engine.clone(),
        services: Arc::default(),
        thread_first: 0,
        thread_rows: 0,
    }
}

/// Whether `cur` and `prev` are the same threads in the same order, so that
/// [`thread`] applies row by row.
pub(crate) fn same_threads(cur: &[ThreadSample], prev: &[ThreadSample]) -> bool {
    cur.len() == prev.len()
        && cur.iter().zip(prev).all(|(c, p)| {
            c.tid == p.tid && c.birth == p.birth && c.started_unix_ms == p.started_unix_ms
        })
}

/// `cur` as a change from `prev`, the same thread one frame earlier.
pub(crate) fn thread(cur: &ThreadSample, prev: &ThreadSample) -> ThreadDelta {
    let ThreadSample {
        tid: _,
        birth: _,
        cpu,
        state,
        wait_reason,
        service,
        started_unix_ms: _,
    } = cur;
    ThreadDelta {
        cpu: x_f32(cpu.0, prev.cpu.0),
        state: changed(*state, prev.state),
        wait_reason: changed(*wait_reason, prev.wait_reason),
        service: changed(*service, prev.service),
    }
}

/// Undo [`thread`].
pub(crate) fn apply_thread(prev: &ThreadSample, d: &ThreadDelta) -> ThreadSample {
    let ThreadDelta {
        cpu,
        state,
        wait_reason,
        service,
    } = d;
    ThreadSample {
        tid: prev.tid,
        birth: prev.birth,
        cpu: Percent(u_f32(prev.cpu.0, *cpu)),
        state: state.unwrap_or(prev.state),
        wait_reason: wait_reason.unwrap_or(prev.wait_reason),
        service: service.unwrap_or(prev.service),
        started_unix_ms: prev.started_unix_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seed: u64) -> ProcessSample {
        let f = |k: u64| {
            seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .rotate_left(k as u32)
        };
        ProcessSample {
            cpu: Percent(f(1) as f32 / 1e15),
            cpu_time: Duration::from_nanos(f(2) >> 4),
            cycles: f(3),
            working_set: Bytes(f(4)),
            private_bytes: Bytes(f(5)),
            disk_read: Bytes(f(6) >> 40),
            disk_write: Bytes(f(7) >> 40),
            net_rx: Bytes(f(8) >> 50),
            net_tx: Bytes(f(9) >> 50),
            threads: f(10) as u32,
            handles: f(11) as u32,
            power: seed.is_multiple_of(2).then_some(Watts(f(12) as f32)),
            gpu: seed.is_multiple_of(3).then_some(Percent(f(13) as f32)),
            suspended: seed.is_multiple_of(5),
            efficiency_mode: (seed % 2 == 1).then_some(seed % 4 == 1),
            kind: ProcessKind::App,
            priority: Priority::High,
            page_faults: f(14) as u32,
            peak_working_set: Bytes(f(15)),
            virtual_size: Bytes(f(16)),
            paged_pool: Bytes(f(17)),
            nonpaged_pool: Bytes(f(18)),
            io: IoCounters {
                reads: f(19),
                writes: f(20),
                other: f(21),
                read_bytes: Bytes(f(22)),
                write_bytes: Bytes(f(23)),
                other_bytes: Bytes(f(24)),
            },
            gpu_engine: seed.is_multiple_of(2).then(|| Arc::from("GPU 0 - 3D")),
            ..ProcessSample::default()
        }
    }

    #[test]
    fn process_delta_round_trips_every_field() {
        for (a, b) in [(1u64, 2u64), (u64::MAX, 0), (12345, 12345), (0, u64::MAX)] {
            let prev = sample(a);
            let cur = sample(b);
            let got = apply_process(&prev, &process(&cur, &prev));
            assert_eq!(got, cur, "seeds {a} -> {b}");
        }
    }

    #[test]
    fn unchanged_process_is_all_zeros() {
        let s = sample(7);
        let d = process(&s, &s);
        let bytes = postcard::to_allocvec(&d).unwrap();
        // Every counter is a one-byte zero; only the enums, options and the engine
        // name carry anything.
        assert!(bytes.len() < 48, "{} bytes: {bytes:?}", bytes.len());
    }

    #[test]
    fn thread_delta_round_trips() {
        let prev = ThreadSample {
            tid: 10,
            birth: 99,
            cpu: Percent(1.5),
            state: ThreadState::Waiting,
            wait_reason: WaitReason(4),
            service: ServiceTag::None,
            started_unix_ms: Some(5),
        };
        let cur = ThreadSample {
            cpu: Percent(f32::NAN),
            state: ThreadState::Running,
            wait_reason: WaitReason(0),
            service: ServiceTag::Service(3),
            ..prev
        };
        let got = apply_thread(&prev, &thread(&cur, &prev));
        assert!(got.cpu.0.is_nan());
        assert_eq!(got.cpu.0.to_bits(), cur.cpu.0.to_bits());
        assert_eq!(
            (got.state, got.wait_reason, got.service),
            (cur.state, cur.wait_reason, cur.service)
        );
        assert_eq!((got.tid, got.birth, got.started_unix_ms), (10, 99, Some(5)));

        let same = thread(&prev, &prev);
        assert_eq!(postcard::to_allocvec(&same).unwrap().len(), 4);
        assert!(same_threads(&[prev], &[prev]));
        assert!(!same_threads(&[prev], &[]));
        assert!(!same_threads(
            &[prev],
            &[ThreadSample { birth: 100, ..prev }]
        ));
    }
}
