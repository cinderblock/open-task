//! The battery, the processor's temperature and its package power.
//!
//! - Battery: `/sys/class/power_supply`, the system's batteries (not a mouse's)
//!   added together, so a laptop with two reads as one.
//! - Temperature: the `coretemp` driver's "Package id 0" on Intel, `k10temp`'s
//!   Tctl on AMD, or the `x86_pkg_temp` thermal zone.
//! - Package power: RAPL's energy counter under `/sys/class/powercap`. Most
//!   kernels let only root read it.

use std::time::{Duration, Instant};

use ot_model::battery::{BatterySample, BatteryState};
use ot_model::cpu::ThermalSensor;
use ot_model::units::Watts;

use super::fs;

const SUPPLIES: &str = "/sys/class/power_supply";

/// The system's batteries and AC adapters, found once.
#[derive(Debug, Default)]
pub(super) struct BatteryProbe {
    batteries: Vec<String>,
    mains: Vec<String>,
}

impl BatteryProbe {
    pub(super) fn new() -> Self {
        let mut p = Self::default();
        let Ok(dir) = std::fs::read_dir(SUPPLIES) else {
            return p;
        };
        for e in dir.flatten() {
            let path = e.path().to_string_lossy().into_owned();
            match fs::read_trimmed(format!("{path}/type")).as_deref() {
                // A wireless mouse's battery has scope "Device"; the system's has none.
                Some("Battery")
                    if fs::read_trimmed(format!("{path}/scope")).as_deref() != Some("Device") =>
                {
                    p.batteries.push(path);
                }
                Some("Mains") => p.mains.push(path),
                _ => {}
            }
        }
        p.batteries.sort();
        p
    }

    pub(super) fn sample(&self) -> Option<BatterySample> {
        if self.batteries.is_empty() {
            return None;
        }
        let ac_power = (!self.mains.is_empty()).then(|| {
            self.mains
                .iter()
                .any(|m| fs::read_u64(format!("{m}/online")) == Some(1))
        });
        let mut total = Reading::default();
        let mut states = Vec::new();
        let mut first = None;
        for b in &self.batteries {
            let r = Reading::read(b);
            states.push(fs::read_trimmed(format!("{b}/status")).unwrap_or_default());
            total.add(&r);
            first.get_or_insert_with(|| b.clone());
        }
        let first = first?;
        let state = if states.iter().any(|s| s == "Charging") {
            BatteryState::Charging
        } else if states.iter().any(|s| s == "Discharging") {
            BatteryState::Discharging
        } else if states.iter().any(|s| s == "Full" || s == "Not charging") {
            BatteryState::Idle
        } else {
            BatteryState::Unknown
        };
        let charge = match (total.now_uwh, total.full_uwh) {
            (Some(now), Some(full)) if full > 0 => {
                Some((now as f32 / full as f32 * 100.0).min(100.0))
            }
            _ => fs::read_u64(format!("{first}/capacity")).map(|c| c as f32),
        };
        let rate = total
            .power_uw
            .filter(|&p| p > 0)
            .map(|p| Watts(p as f32 / 1e6));
        let time_left = match (state, total.power_uw, total.now_uwh, total.full_uwh) {
            (BatteryState::Discharging, Some(p), Some(now), _) if p > 0 => {
                Some(Duration::from_secs_f64(now as f64 / p as f64 * 3600.0))
            }
            (BatteryState::Charging, Some(p), Some(now), Some(full)) if p > 0 => Some(
                Duration::from_secs_f64(full.saturating_sub(now) as f64 / p as f64 * 3600.0),
            ),
            _ => None,
        };
        let mwh = |uwh: Option<u64>| uwh.map(|v| u32::try_from(v / 1000).unwrap_or(u32::MAX));
        Some(BatterySample {
            charge,
            state,
            rate,
            time_left,
            ac_power,
            full_capacity_mwh: mwh(total.full_uwh),
            design_capacity_mwh: mwh(total.design_uwh),
            cycle_count: fs::read_u64(format!("{first}/cycle_count"))
                .filter(|&c| c > 0)
                .map(|c| c as u32),
            manufacturer: fs::read_trimmed(format!("{first}/manufacturer")),
            chemistry: fs::read_trimmed(format!("{first}/technology")),
        })
    }
}

/// One battery's energy, in microwatt-hours, and power, in microwatts. A battery
/// that reports charge (µAh) and current (µA) instead is converted at its design
/// voltage, or its present voltage for power.
#[derive(Debug, Default, Clone, Copy)]
struct Reading {
    now_uwh: Option<u64>,
    full_uwh: Option<u64>,
    design_uwh: Option<u64>,
    power_uw: Option<u64>,
}

impl Reading {
    fn read(dir: &str) -> Self {
        let get = |f: &str| fs::read_u64(format!("{dir}/{f}"));
        let volts = get("voltage_min_design").or_else(|| get("voltage_now"));
        let from_charge = |f: &str| {
            get(f)
                .zip(volts)
                .map(|(uah, uv)| (u128::from(uah) * u128::from(uv) / 1_000_000) as u64)
        };
        let energy = |e: &str, c: &str| get(e).or_else(|| from_charge(c));
        let power_uw = get("power_now").or_else(|| {
            get("current_now")
                .zip(get("voltage_now"))
                .map(|(ua, uv)| (u128::from(ua) * u128::from(uv) / 1_000_000) as u64)
        });
        Self {
            now_uwh: energy("energy_now", "charge_now"),
            full_uwh: energy("energy_full", "charge_full"),
            design_uwh: energy("energy_full_design", "charge_full_design"),
            power_uw,
        }
    }

    fn add(&mut self, o: &Self) {
        let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        self.now_uwh = sum(self.now_uwh, o.now_uwh);
        self.full_uwh = sum(self.full_uwh, o.full_uwh);
        self.design_uwh = sum(self.design_uwh, o.design_uwh);
        self.power_uw = sum(self.power_uw, o.power_uw);
    }
}

/// Where the processor temperature is read, found once.
#[derive(Debug)]
pub(super) struct Thermal {
    path: String,
    sensor: ThermalSensor,
}

impl Thermal {
    pub(super) fn find() -> Option<Self> {
        let hwmon = std::fs::read_dir("/sys/class/hwmon").ok()?;
        for e in hwmon.flatten() {
            let dir = e.path().to_string_lossy().into_owned();
            let (want, sensor) = match fs::read_trimmed(format!("{dir}/name")).as_deref() {
                Some("coretemp") => ("Package id 0", ThermalSensor::Package),
                Some("k10temp" | "zenpower") => ("Tctl", ThermalSensor::Tctl),
                _ => continue,
            };
            for i in 1..64 {
                let Some(label) = fs::read_trimmed(format!("{dir}/temp{i}_label")) else {
                    continue;
                };
                if label == want {
                    return Some(Self {
                        path: format!("{dir}/temp{i}_input"),
                        sensor,
                    });
                }
            }
        }
        let zones = std::fs::read_dir("/sys/class/thermal").ok()?;
        for e in zones.flatten() {
            let dir = e.path().to_string_lossy().into_owned();
            if fs::read_trimmed(format!("{dir}/type")).as_deref() == Some("x86_pkg_temp") {
                return Some(Self {
                    path: format!("{dir}/temp"),
                    sensor: ThermalSensor::Package,
                });
            }
        }
        None
    }

    pub(super) fn sensor(&self) -> ThermalSensor {
        self.sensor
    }

    /// Degrees Celsius; the files hold millidegrees.
    pub(super) fn read(&self) -> Option<f32> {
        fs::read_i64(&self.path).map(|m| m as f32 / 1000.0)
    }
}

/// The package's RAPL energy counter, when this user may read it.
#[derive(Debug)]
pub(super) struct Rapl {
    path: String,
    /// Where the counter wraps, in microjoules.
    range_uj: u64,
    prev: Option<(u64, Instant)>,
}

impl Rapl {
    pub(super) fn open() -> Option<Self> {
        let dir = "/sys/class/powercap/intel-rapl:0";
        let path = format!("{dir}/energy_uj");
        fs::read_u64(&path)?;
        Some(Self {
            path,
            range_uj: fs::read_u64(format!("{dir}/max_energy_range_uj")).unwrap_or(u64::MAX),
            prev: None,
        })
    }

    /// Average package power since the previous call.
    pub(super) fn sample(&mut self) -> Option<Watts> {
        let now = (fs::read_u64(&self.path)?, Instant::now());
        let prev = self.prev.replace(now)?;
        let secs = now.1.duration_since(prev.1).as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        let used = if now.0 >= prev.0 {
            now.0 - prev.0
        } else {
            self.range_uj.saturating_sub(prev.0) + now.0
        };
        Some(Watts((used as f64 / 1e6 / secs) as f32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake power supply directory holding `files`.
    fn supply(name: &str, files: &[(&str, &str)]) -> String {
        let dir =
            std::env::temp_dir().join(format!("ot-probe-power-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (f, v) in files {
            std::fs::write(
                dir.join(f),
                format!(
                    "{v}
"
                ),
            )
            .unwrap();
        }
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn a_discharging_energy_battery() {
        let bat = supply(
            "energy",
            &[
                ("status", "Discharging"),
                ("energy_now", "25000000"),
                ("energy_full", "50000000"),
                ("energy_full_design", "57000000"),
                ("power_now", "10000000"),
                ("cycle_count", "120"),
                ("manufacturer", "SMP"),
                ("technology", "Li-poly"),
            ],
        );
        let ac = supply("ac", &[("online", "0")]);
        let p = BatteryProbe {
            batteries: vec![bat],
            mains: vec![ac],
        };
        let b = p.sample().unwrap();
        assert_eq!(b.state, BatteryState::Discharging);
        assert_eq!(b.charge, Some(50.0));
        assert_eq!(b.rate, Some(Watts(10.0)));
        // 25 Wh at 10 W: two and a half hours.
        assert_eq!(b.time_left, Some(Duration::from_secs(9000)));
        assert_eq!(b.ac_power, Some(false));
        assert_eq!(b.full_capacity_mwh, Some(50_000));
        assert_eq!(b.design_capacity_mwh, Some(57_000));
        assert_eq!(b.cycle_count, Some(120));
        assert_eq!(b.manufacturer.as_deref(), Some("SMP"));
    }

    #[test]
    fn a_charge_battery_is_converted_at_its_design_voltage() {
        let bat = supply(
            "charge",
            &[
                ("status", "Charging"),
                ("charge_now", "1000000"),
                ("charge_full", "4000000"),
                ("voltage_min_design", "12000000"),
                ("voltage_now", "12000000"),
                ("current_now", "1000000"),
            ],
        );
        let p = BatteryProbe {
            batteries: vec![bat],
            mains: Vec::new(),
        };
        let b = p.sample().unwrap();
        assert_eq!(b.state, BatteryState::Charging);
        assert_eq!(b.charge, Some(25.0));
        assert_eq!(b.full_capacity_mwh, Some(48_000));
        assert_eq!(b.rate, Some(Watts(12.0)));
        // 36 Wh to go at 12 W: three hours.
        assert_eq!(b.time_left, Some(Duration::from_secs(3 * 3600)));
        assert_eq!(b.ac_power, None);
    }

    #[test]
    fn no_battery_is_none() {
        assert_eq!(BatteryProbe::default().sample(), None);
    }
}
