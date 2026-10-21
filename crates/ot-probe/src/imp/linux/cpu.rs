//! Processor load, topology and clocks, and the machine's static facts.
//!
//! Load comes from `/proc/stat`, which counts the clock ticks each logical
//! processor spent in each state since boot; a pass differences them. Topology,
//! hybrid core classes, caches and clocks come from `/sys/devices/system/cpu`.

use std::collections::{HashMap, HashSet};
use std::io;

use ot_model::cpu::{CoreKind, CpuSample, LogicalCore};
use ot_model::hardware::Hardware;
use ot_model::units::{Bytes, Hertz, Percent};

use super::fs;

const CPU_DIR: &str = "/sys/devices/system/cpu";

/// Busy and total ticks of one logical processor (or all of them).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Times {
    busy: u64,
    total: u64,
}

impl Times {
    /// Parse the numbers after `cpu` / `cpuN` on a `/proc/stat` line: user, nice,
    /// system, idle, iowait, irq, softirq, steal, then `guest` and `guest_nice`, which
    /// are already counted in user and nice. Idle and iowait are not busy.
    fn parse(fields: &[u64]) -> Self {
        let total: u64 = fields.iter().take(8).sum();
        let idle = fields.get(3).copied().unwrap_or(0) + fields.get(4).copied().unwrap_or(0);
        Self {
            busy: total.saturating_sub(idle),
            total,
        }
    }

    /// Share of the interval since `prev` spent busy.
    fn usage_since(self, prev: Self) -> Percent {
        let total = self.total.saturating_sub(prev.total);
        if total == 0 {
            return Percent::ZERO;
        }
        let busy = self.busy.saturating_sub(prev.busy);
        Percent::from_ratio(busy as f32 / total as f32).clamped(100.0)
    }
}

/// What is known about one logical processor, read once.
#[derive(Debug, Clone)]
struct Core {
    physical: u32,
    kind: CoreKind,
    /// `scaling_cur_freq`, when this processor has a cpufreq driver.
    freq_path: Option<String>,
}

#[derive(Debug)]
pub(super) struct CpuProbe {
    /// Indexed by logical processor number; `None` for a number never seen online.
    cores: Vec<Option<Core>>,
    prev: Vec<Times>,
    prev_total: Times,
    buf: Vec<u8>,
    fields: Vec<u64>,
}

impl CpuProbe {
    pub(super) fn new() -> Self {
        let online = fs::read_trimmed(format!("{CPU_DIR}/online"))
            .map(|s| fs::parse_cpu_list(&s))
            .unwrap_or_default();
        let max = online.iter().copied().max().map_or(0, |m| m as usize + 1);
        let kinds = hybrid_kinds();
        let mut physical_ids: HashMap<(i64, i64), u32> = HashMap::new();
        let mut cores = vec![None; max];
        for &cpu in &online {
            let topo = format!("{CPU_DIR}/cpu{cpu}/topology");
            let package = fs::read_i64(format!("{topo}/physical_package_id")).unwrap_or(0);
            let core = fs::read_i64(format!("{topo}/core_id")).unwrap_or(i64::from(cpu));
            let next = physical_ids.len() as u32;
            let physical = *physical_ids.entry((package, core)).or_insert(next);
            let freq_path = format!("{CPU_DIR}/cpu{cpu}/cpufreq/scaling_cur_freq");
            let freq_path = std::path::Path::new(&freq_path)
                .exists()
                .then_some(freq_path);
            cores[cpu as usize] = Some(Core {
                physical,
                kind: kinds.get(&cpu).copied().unwrap_or_default(),
                freq_path,
            });
        }
        Self {
            prev: vec![Times::default(); max],
            cores,
            prev_total: Times::default(),
            buf: Vec::with_capacity(4096),
            fields: Vec::with_capacity(10),
        }
    }

    /// Whether any processor's clock can be read.
    pub(super) fn has_frequency(&self) -> bool {
        self.cores.iter().flatten().any(|c| c.freq_path.is_some())
    }

    /// Whether this is a hybrid processor with known core classes.
    pub(super) fn hybrid(&self) -> bool {
        self.cores
            .iter()
            .flatten()
            .any(|c| c.kind != CoreKind::Unknown)
    }

    pub(super) fn sample(&mut self, out: &mut CpuSample) -> io::Result<()> {
        fs::read_into("/proc/stat", &mut self.buf)?;
        out.cores.clear();
        for line in self.buf.split(|&b| b == b'\n') {
            let Some(rest) = line.strip_prefix(b"cpu") else {
                // The cpu lines come first; nothing after them is needed.
                if out.cores.is_empty() {
                    continue;
                }
                break;
            };
            // `cpu  …` is the total; `cpuN …` one processor, its number glued on.
            let total_line = rest.first() == Some(&b' ');
            let mut parts = rest.split(|&b| b == b' ').filter(|p| !p.is_empty());
            let index = if total_line {
                None
            } else {
                let Some(n) = parts.next() else { continue };
                Some(fs::num(n) as usize)
            };
            self.fields.clear();
            self.fields.extend(parts.map(fs::num));
            let now = Times::parse(&self.fields);
            match index {
                None => {
                    out.total = now.usage_since(self.prev_total);
                    self.prev_total = now;
                }
                Some(i) => {
                    if i >= self.prev.len() {
                        // A processor brought online since start.
                        self.prev.resize(i + 1, Times::default());
                        self.cores.resize(i + 1, None);
                    }
                    let usage = now.usage_since(self.prev[i]);
                    self.prev[i] = now;
                    let core = self.cores[i].as_ref();
                    out.cores.push(LogicalCore {
                        index: i as u32,
                        physical: core.map_or(i as u32, |c| c.physical),
                        kind: core.map_or(CoreKind::Unknown, |c| c.kind),
                        usage,
                        frequency: core
                            .and_then(|c| c.freq_path.as_deref())
                            .and_then(fs::read_u64)
                            .map(|khz| Hertz(khz * 1000)),
                    });
                }
            }
        }
        Ok(())
    }
}

/// On an Intel hybrid processor the kernel lists the P-cores under
/// `/sys/devices/cpu_core` and the E-cores under `/sys/devices/cpu_atom`.
fn hybrid_kinds() -> HashMap<u32, CoreKind> {
    let mut kinds = HashMap::new();
    for (dir, kind) in [
        ("cpu_core", CoreKind::Performance),
        ("cpu_atom", CoreKind::Efficiency),
    ] {
        if let Some(list) = fs::read_trimmed(format!("/sys/devices/{dir}/cpus")) {
            for cpu in fs::parse_cpu_list(&list) {
                kinds.insert(cpu, kind);
            }
        }
    }
    kinds
}

/// Facts that do not change while the machine runs.
pub(super) fn hardware() -> Hardware {
    let mut hw = Hardware::default();
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut flags: HashSet<&str> = HashSet::new();
    let mut measured_mhz = None;
    for line in cpuinfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "model name" if hw.cpu_name.is_none() => hw.cpu_name = Some(value.to_owned()),
            "flags" if flags.is_empty() => flags = value.split(' ').collect(),
            "cpu MHz" if measured_mhz.is_none() => measured_mhz = value.parse::<f64>().ok(),
            _ => {}
        }
    }
    if cfg!(any(target_arch = "x86", target_arch = "x86_64")) && !flags.is_empty() {
        // The kernel clears vmx/svm when firmware has virtualization turned off.
        hw.virtualization = Some(flags.contains("vmx") || flags.contains("svm"));
        hw.hypervisor = Some(flags.contains("hypervisor"));
    }

    let online = fs::read_trimmed(format!("{CPU_DIR}/online"))
        .map(|s| fs::parse_cpu_list(&s))
        .unwrap_or_default();
    hw.logical_processors = online.len() as u32;
    let mut packages = HashSet::new();
    let mut physical = HashSet::new();
    for &cpu in &online {
        let topo = format!("{CPU_DIR}/cpu{cpu}/topology");
        let package = fs::read_i64(format!("{topo}/physical_package_id")).unwrap_or(0);
        let core = fs::read_i64(format!("{topo}/core_id")).unwrap_or(i64::from(cpu));
        packages.insert(package);
        physical.insert((package, core));
    }
    hw.sockets = packages.len() as u32;
    hw.physical_cores = physical.len() as u32;

    let (l1, l2, l3) = caches(&online);
    hw.cache_l1 = l1;
    hw.cache_l2 = l2;
    hw.cache_l3 = l3;

    hw.base_frequency = rated_clock(
        fs::read_u64(format!("{CPU_DIR}/cpu0/cpufreq/base_frequency")),
        hw.cpu_name.as_deref(),
        fs::read_u64(format!("{CPU_DIR}/cpu0/cpufreq/cpuinfo_max_freq")),
        measured_mhz,
    );

    hw.boot_unix_ms = boot_time_secs().map(|s| s.cast_signed() * 1000);
    hw
}

/// The processor's rated clock, from the best source there is: what `intel_pstate`
/// and `amd-pstate` publish (`base_frequency`, kHz); the clock an Intel model name
/// states (`... CPU @ 3.60GHz`); the top of the cpufreq range (`cpuinfo_max_freq`,
/// kHz), the rated clock under acpi-cpufreq and on most ARM parts; and last, the
/// clock `/proc/cpuinfo` says it measured (MHz), which in a virtual machine is the
/// host's timestamp counter rate.
///
/// Process cycles are counted at this rate (see `process.rs`), as Windows counts
/// them at the timestamp counter's, which runs at the rated clock.
fn rated_clock(
    base_khz: Option<u64>,
    model: Option<&str>,
    max_khz: Option<u64>,
    measured_mhz: Option<f64>,
) -> Option<Hertz> {
    let khz = |k: u64| (k > 0).then(|| Hertz(k * 1000));
    base_khz
        .and_then(khz)
        .or_else(|| model.and_then(model_clock))
        .or_else(|| max_khz.and_then(khz))
        .or_else(|| {
            measured_mhz
                .filter(|&m| m.is_finite() && m >= 1.0)
                .map(|m| Hertz((m * 1e6).round() as u64))
        })
}

/// The clock a model name states: `Intel(R) Core(TM) i7-7700 CPU @ 3.60GHz`.
fn model_clock(model: &str) -> Option<Hertz> {
    let (_, rest) = model.rsplit_once('@')?;
    let rest = rest.trim();
    let (number, scale) = if let Some(n) = rest.strip_suffix("GHz") {
        (n, 1e9)
    } else {
        (rest.strip_suffix("MHz")?, 1e6)
    };
    let v: f64 = number.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then(|| Hertz((v * scale).round() as u64))
}

/// Totals of each cache level across the machine. Each cache is counted once,
/// however many processors share it.
fn caches(online: &[u32]) -> (Option<Bytes>, Option<Bytes>, Option<Bytes>) {
    let mut seen = HashSet::new();
    let mut totals = [0u64; 3];
    for &cpu in online {
        for index in 0.. {
            let dir = format!("{CPU_DIR}/cpu{cpu}/cache/index{index}");
            let Some(level) = fs::read_u64(format!("{dir}/level")) else {
                break;
            };
            let kind = fs::read_trimmed(format!("{dir}/type")).unwrap_or_default();
            let shared = fs::read_trimmed(format!("{dir}/shared_cpu_list")).unwrap_or_default();
            let Some(size) =
                fs::read_trimmed(format!("{dir}/size")).and_then(|s| fs::parse_size(&s))
            else {
                continue;
            };
            if !(1..=3).contains(&level) || !seen.insert((level, kind, shared)) {
                continue;
            }
            totals[level as usize - 1] += size;
        }
    }
    let some = |n: u64| (n > 0).then_some(Bytes(n));
    (some(totals[0]), some(totals[1]), some(totals[2]))
}

/// When the machine booted, seconds since the Unix epoch (`btime` in `/proc/stat`).
pub(super) fn boot_time_secs() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    stat.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_and_iowait_are_not_busy() {
        // user nice system idle iowait irq softirq steal guest guest_nice
        let t = Times::parse(&[100, 0, 50, 800, 50, 0, 0, 0, 30, 0]);
        assert_eq!(t.total, 1000);
        assert_eq!(t.busy, 150);
    }

    #[test]
    fn usage_is_the_busy_share_of_the_interval() {
        let a = Times {
            busy: 100,
            total: 1000,
        };
        let b = Times {
            busy: 150,
            total: 1100,
        };
        assert!((b.usage_since(a).get() - 50.0).abs() < 1e-3);
        // A counter that went backwards (a processor taken offline and back) reads
        // zero, not garbage.
        assert_eq!(a.usage_since(b), Percent::ZERO);
    }

    #[test]
    fn a_pass_reads_every_online_processor() {
        let mut probe = CpuProbe::new();
        let mut out = CpuSample::default();
        probe.sample(&mut out).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        probe.sample(&mut out).unwrap();
        let online = std::thread::available_parallelism().map_or(1, usize::from);
        assert!(out.cores.len() >= online, "{} < {online}", out.cores.len());
        assert!((0.0..=100.0).contains(&out.total.get()));
    }

    #[test]
    fn the_rated_clock_comes_from_the_best_source_there_is() {
        let ghz = |g: f64| Some(Hertz((g * 1e9) as u64));
        let intel = Some("Intel(R) Core(TM) i7-7700 CPU @ 3.60GHz");
        let amd = Some("AMD Ryzen 9 7950X 16-Core Processor");
        assert_eq!(
            rated_clock(Some(4_500_000), intel, Some(4_200_000), Some(800.0)),
            ghz(4.5)
        );
        assert_eq!(
            rated_clock(None, intel, Some(4_200_000), Some(800.0)),
            ghz(3.6)
        );
        assert_eq!(
            rated_clock(None, amd, Some(4_500_000), Some(800.0)),
            ghz(4.5)
        );
        assert_eq!(
            rated_clock(None, amd, None, Some(2_994.374)),
            Some(Hertz(2_994_374_000))
        );
        assert_eq!(rated_clock(Some(0), amd, Some(0), None), None);
        assert_eq!(model_clock("Some CPU @ 2400MHz"), ghz(2.4));
        assert_eq!(model_clock("Some CPU @ fast"), None);
    }

    #[test]
    fn hardware_counts_processors() {
        let hw = hardware();
        assert!(hw.logical_processors >= 1);
        assert!(hw.physical_cores >= 1 && hw.physical_cores <= hw.logical_processors);
        assert!(hw.sockets >= 1);
        assert!(hw.boot_unix_ms.is_some());
    }
}
