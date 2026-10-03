//! The battery, on a machine that has one.
//!
//! Two sources. `GetSystemPowerStatus` is one cheap call and gives what the tray
//! icon shows: on mains or not, a percentage, a time estimate, and whether there
//! is a battery at all. The battery device gives the rest: `IOCTL_BATTERY_QUERY_
//! INFORMATION` for the full and design capacities, cycle count, chemistry and
//! maker, and `IOCTL_BATTERY_QUERY_STATUS` for the charge and the rate in
//! milliwatts, which is how the power draw is known.
//!
//! The device is found through the battery device interface class
//! (`GUID_DEVCLASS_BATTERY`, which doubles as the interface GUID): `SetupDiGetClass
//! DevsW` with `DIGCF_DEVICEINTERFACE`, enumerate, fetch each path, open it with
//! `CreateFileW`. Every battery IOCTL takes a *tag* from `IOCTL_BATTERY_QUERY_TAG`
//! that identifies the physical battery in that bay; a bay with no battery answers
//! with the invalid tag and is skipped. The handle and tag are kept; an IOCTL that
//! fails with `ERROR_NO_SUCH_DEVICE` (the battery was pulled, or the tag changed
//! because another was fitted) drops them and the next pass looks again.
//!
//! A desktop reports "no battery" from `GetSystemPowerStatus`; that answer is
//! remembered for [`RECHECK_ABSENT`] so a sampling pass costs nothing there. The
//! device path is also retried no more often than that when it fails while the
//! system says a battery exists, in which case the system-level sample is still
//! returned.
//!
//! Capacities are in milliwatt-hours unless the battery reports
//! `BATTERY_CAPACITY_RELATIVE`, in which case they are in units of its own and are
//! not reported; the charge percentage is still a ratio of two of them.

use std::mem::size_of;
use std::time::{Duration, Instant};

use ot_model::battery::{BatterySample, BatteryState};
use ot_model::Watts;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, GUID_DEVCLASS_BATTERY,
    HDEVINFO, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Foundation::{ERROR_NO_SUCH_DEVICE, GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Power::{
    BatteryInformation, BatteryManufactureName, GetSystemPowerStatus, BATTERY_CAPACITY_RELATIVE,
    BATTERY_CHARGING, BATTERY_DISCHARGING, BATTERY_INFORMATION, BATTERY_POWER_ON_LINE,
    BATTERY_QUERY_INFORMATION, BATTERY_STATUS, BATTERY_TAG_INVALID, BATTERY_UNKNOWN_CAPACITY,
    BATTERY_UNKNOWN_RATE, BATTERY_WAIT_STATUS, IOCTL_BATTERY_QUERY_INFORMATION,
    IOCTL_BATTERY_QUERY_STATUS, IOCTL_BATTERY_QUERY_TAG, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::IO::DeviceIoControl;

use super::tags::OwnedHandle;
use super::AlignedBuf;

/// How long "no battery" (or "no battery device") is believed before asking again.
const RECHECK_ABSENT: Duration = Duration::from_secs(30);

/// `SYSTEM_POWER_STATUS.BatteryFlag` bits.
const FLAG_CHARGING: u8 = 8;
const FLAG_NO_BATTERY: u8 = 128;
/// `BatteryLifePercent` and `ACLineStatus` when unknown.
const UNKNOWN_U8: u8 = 255;
/// `BATTERY_STATUS.Rate` when unknown: the SDK's constant, which is the most
/// negative i32 written as a u32.
const UNKNOWN_RATE: i32 = BATTERY_UNKNOWN_RATE.cast_signed();

/// An open battery device with the tag its IOCTLs need, and the facts that do not
/// change while it stays fitted.
#[derive(Debug)]
struct BatteryDevice {
    handle: OwnedHandle,
    tag: u32,
    info: BATTERY_INFORMATION,
    manufacturer: Option<String>,
}

/// Where the battery stands.
#[derive(Debug, Default)]
pub(super) struct BatteryProbe {
    device: Option<BatteryDevice>,
    /// When the system last said there is no battery, or the device could not be
    /// found; nothing is asked again until [`RECHECK_ABSENT`] has passed.
    absent_since: Option<Instant>,
    device_missing_since: Option<Instant>,
    /// How many times the operating system has been asked anything; tests use it
    /// to show the absent answer is cached.
    checks: u32,
}

impl BatteryProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// The battery now, or `None` on a machine without one.
    pub fn sample(&mut self) -> Option<BatterySample> {
        let now = Instant::now();
        if self
            .absent_since
            .is_some_and(|t| now.duration_since(t) < RECHECK_ABSENT)
        {
            return None;
        }
        self.checks += 1;
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: `status` is a valid out-pointer.
        let ok = unsafe { GetSystemPowerStatus(&raw mut status) }.is_ok();
        if !ok || status.BatteryFlag & FLAG_NO_BATTERY != 0 {
            self.absent_since = Some(now);
            self.device = None;
            return None;
        }
        self.absent_since = None;
        let mut sample = system_sample(&status);

        if self.device.is_none()
            && self
                .device_missing_since
                .is_none_or(|t| now.duration_since(t) >= RECHECK_ABSENT)
        {
            self.device = open_device();
            self.device_missing_since = self.device.is_none().then_some(now);
            if self.device.is_none() {
                tracing::warn!("a battery is present but its device could not be opened");
            }
        }
        if let Some(dev) = &self.device {
            match dev.status() {
                Ok(s) => dev.apply(&s, &mut sample),
                Err(code) => {
                    // Pulled, or swapped: look again next pass.
                    tracing::debug!(code, "battery status failed; reopening");
                    self.device = None;
                    self.device_missing_since = None;
                }
            }
        }
        Some(sample)
    }

    /// How many times the operating system has been asked, for tests.
    #[cfg(test)]
    fn checks(&self) -> u32 {
        self.checks
    }
}

/// What `GetSystemPowerStatus` alone can say.
fn system_sample(s: &SYSTEM_POWER_STATUS) -> BatterySample {
    let ac_power = match s.ACLineStatus {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    let charge = (s.BatteryLifePercent != UNKNOWN_U8).then(|| f32::from(s.BatteryLifePercent));
    let time_left =
        (s.BatteryLifeTime != u32::MAX).then(|| Duration::from_secs(u64::from(s.BatteryLifeTime)));
    let state = if s.BatteryFlag & FLAG_CHARGING != 0 {
        BatteryState::Charging
    } else {
        match ac_power {
            Some(false) => BatteryState::Discharging,
            Some(true) => BatteryState::Idle,
            None => BatteryState::Unknown,
        }
    };
    BatterySample {
        charge,
        state,
        time_left,
        ac_power,
        ..BatterySample::default()
    }
}

impl BatteryDevice {
    /// `IOCTL_BATTERY_QUERY_STATUS` now. The error is the Win32 code.
    fn status(&self) -> Result<BATTERY_STATUS, u32> {
        let wait = BATTERY_WAIT_STATUS {
            BatteryTag: self.tag,
            // Zero: answer now, do not wait for a change.
            Timeout: 0,
            PowerState: 0,
            LowCapacity: 0,
            HighCapacity: 0,
        };
        let mut status = BATTERY_STATUS {
            PowerState: 0,
            Capacity: 0,
            Voltage: 0,
            Rate: 0,
        };
        // SAFETY: the input and output structures outlive the call; sizes match.
        ioctl(
            &self.handle,
            IOCTL_BATTERY_QUERY_STATUS,
            (&raw const wait).cast(),
            size_of::<BATTERY_WAIT_STATUS>(),
            (&raw mut status).cast(),
            size_of::<BATTERY_STATUS>(),
        )?;
        Ok(status)
    }

    /// Add what the device knows to the system-level sample.
    fn apply(&self, s: &BATTERY_STATUS, out: &mut BatterySample) {
        let relative = self.info.Capabilities & BATTERY_CAPACITY_RELATIVE != 0;
        let full = (self.info.FullChargedCapacity != BATTERY_UNKNOWN_CAPACITY)
            .then_some(self.info.FullChargedCapacity);
        let design = (self.info.DesignedCapacity != BATTERY_UNKNOWN_CAPACITY)
            .then_some(self.info.DesignedCapacity);
        if !relative {
            out.full_capacity_mwh = full;
            out.design_capacity_mwh = design;
        }
        out.cycle_count = (self.info.CycleCount > 0).then_some(self.info.CycleCount);
        out.manufacturer.clone_from(&self.manufacturer);
        out.chemistry = chemistry(self.info.Chemistry);

        if s.PowerState & BATTERY_CHARGING != 0 {
            out.state = BatteryState::Charging;
        } else if s.PowerState & BATTERY_DISCHARGING != 0 {
            out.state = BatteryState::Discharging;
        } else if s.PowerState & BATTERY_POWER_ON_LINE != 0 {
            out.state = BatteryState::Idle;
        }
        if s.Capacity != BATTERY_UNKNOWN_CAPACITY {
            if let Some(full) = full.filter(|&f| f > 0) {
                out.charge = Some((s.Capacity as f32 / full as f32 * 100.0).clamp(0.0, 100.0));
            }
        }
        // Milliwatts, negative while discharging; `BATTERY_UNKNOWN_RATE` is the
        // most negative i32.
        if s.Rate != UNKNOWN_RATE {
            out.rate = Some(Watts(s.Rate.unsigned_abs() as f32 / 1000.0));
        }
    }
}

/// Find the first fitted battery and open it.
fn open_device() -> Option<BatteryDevice> {
    let class = GUID_DEVCLASS_BATTERY;
    // SAFETY: the GUID outlives the call; the set is destroyed by the guard below.
    let set = unsafe {
        SetupDiGetClassDevsW(
            Some(&raw const class),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }
    .ok()?;
    let set = DeviceSet(set);
    let mut buf = AlignedBuf::default();
    for index in 0..8u32 {
        let mut data = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: `data` has its size set, as the API requires.
        let found = unsafe {
            SetupDiEnumDeviceInterfaces(set.0, None, &raw const class, index, &raw mut data)
        }
        .is_ok();
        if !found {
            break;
        }
        let Some(path) = interface_path(&set, &data, &mut buf) else {
            continue;
        };
        if let Some(dev) = open_path(&path) {
            return Some(dev);
        }
    }
    None
}

/// Destroys the device information set on drop.
struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: the set came from SetupDiGetClassDevsW and is destroyed once.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

/// The device path of one interface: a size query, then the fetch.
fn interface_path(
    set: &DeviceSet,
    data: &SP_DEVICE_INTERFACE_DATA,
    buf: &mut AlignedBuf,
) -> Option<Vec<u16>> {
    let mut required = 0u32;
    // SAFETY: a null detail buffer with zero size is the documented size query.
    let _ = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set.0, data, None, 0, Some(&raw mut required), None)
    };
    if required as usize <= size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
        return None;
    }
    buf.resize_bytes(required as usize);
    let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    // SAFETY: the buffer is `required` bytes and the header's size field is the
    // fixed part's size, which is what the API checks.
    unsafe {
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(set.0, data, Some(detail), required, None, None)
    }
    .ok()?;
    let units = (required as usize - size_of::<u32>()) / 2;
    // SAFETY: the path's characters follow the size field inside the buffer the
    // API filled.
    let path = unsafe {
        std::slice::from_raw_parts((&raw const (*detail).DevicePath).cast::<u16>(), units)
    };
    let end = path.iter().position(|&c| c == 0)?;
    Some(path[..=end].to_vec())
}

/// Open the battery at `path` (NUL-terminated) and read its tag and facts. `None`
/// for a bay with no battery in it.
fn open_path(path: &[u16]) -> Option<BatteryDevice> {
    // SAFETY: the path is NUL-terminated and outlives the call; the handle is owned
    // by the guard.
    let h = unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .ok()?;
    let handle = OwnedHandle(h);

    // The tag, without waiting for a battery to appear.
    let wait_ms = 0u32;
    let mut tag = BATTERY_TAG_INVALID;
    // SAFETY: a u32 in, a u32 out, sizes as stated.
    ioctl(
        &handle,
        IOCTL_BATTERY_QUERY_TAG,
        (&raw const wait_ms).cast(),
        size_of::<u32>(),
        (&raw mut tag).cast(),
        size_of::<u32>(),
    )
    .ok()?;
    if tag == BATTERY_TAG_INVALID {
        return None;
    }

    let query = BATTERY_QUERY_INFORMATION {
        BatteryTag: tag,
        InformationLevel: BatteryInformation,
        AtRate: 0,
    };
    let mut info = BATTERY_INFORMATION::default();
    // SAFETY: the query and the output structure outlive the call; sizes match.
    ioctl(
        &handle,
        IOCTL_BATTERY_QUERY_INFORMATION,
        (&raw const query).cast(),
        size_of::<BATTERY_QUERY_INFORMATION>(),
        (&raw mut info).cast(),
        size_of::<BATTERY_INFORMATION>(),
    )
    .ok()?;

    let query = BATTERY_QUERY_INFORMATION {
        BatteryTag: tag,
        InformationLevel: BatteryManufactureName,
        AtRate: 0,
    };
    let mut name = [0u16; 128];
    // SAFETY: the output is a wide buffer passed with its byte size.
    let manufacturer = ioctl(
        &handle,
        IOCTL_BATTERY_QUERY_INFORMATION,
        (&raw const query).cast(),
        size_of::<BATTERY_QUERY_INFORMATION>(),
        name.as_mut_ptr().cast(),
        size_of::<[u16; 128]>(),
    )
    .ok()
    .and_then(|()| {
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let s = String::from_utf16_lossy(&name[..end]).trim().to_owned();
        (!s.is_empty()).then_some(s)
    });

    Some(BatteryDevice {
        handle,
        tag,
        info,
        manufacturer,
    })
}

/// One `DeviceIoControl` on a battery handle. The error is the Win32 code, so
/// callers can tell `ERROR_NO_SUCH_DEVICE` from the rest.
fn ioctl(
    handle: &OwnedHandle,
    code: u32,
    input: *const std::ffi::c_void,
    input_len: usize,
    output: *mut std::ffi::c_void,
    output_len: usize,
) -> Result<(), u32> {
    let mut returned = 0u32;
    // SAFETY: the caller passes buffers that are valid for the stated lengths and
    // outlive the call.
    unsafe {
        DeviceIoControl(
            handle.0,
            code,
            Some(input),
            input_len as u32,
            Some(output),
            output_len as u32,
            Some(&raw mut returned),
            None,
        )
    }
    .map_err(|e| {
        let code = e.code().0 as u32 & 0xFFFF;
        if code == ERROR_NO_SUCH_DEVICE.0 {
            tracing::debug!("battery device gone");
        }
        code
    })
}

/// The four-character chemistry code (`LION`, `LiP`, `NiMH`), trimmed.
fn chemistry(code: [u8; 4]) -> Option<String> {
    let end = code.iter().position(|&b| b == 0).unwrap_or(code.len());
    let s = String::from_utf8_lossy(&code[..end]).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_has_no_battery_and_is_not_asked_twice() {
        let mut probe = BatteryProbe::new();
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: `status` is a valid out-pointer.
        unsafe { GetSystemPowerStatus(&raw mut status) }.expect("GetSystemPowerStatus");
        let first = probe.sample();
        if status.BatteryFlag & FLAG_NO_BATTERY != 0 {
            assert_eq!(first, None);
            assert_eq!(probe.checks(), 1);
            assert_eq!(probe.sample(), None);
            assert_eq!(probe.checks(), 1, "the absent answer is cached");
        } else {
            // A laptop: the sample exists and, through the device, says more.
            let s = first.expect("a battery sample");
            assert!(
                s.charge.is_some_and(|c| (0.0..=100.0).contains(&c)),
                "{s:?}"
            );
            assert!(s.ac_power.is_some(), "{s:?}");
            assert_eq!(probe.checks(), 1);
            probe.sample();
            assert_eq!(probe.checks(), 2, "a present battery is sampled every pass");
        }
    }

    #[test]
    fn the_system_status_alone_is_enough_for_a_sample() {
        let s = system_sample(&SYSTEM_POWER_STATUS {
            ACLineStatus: 0,
            BatteryFlag: 1,
            BatteryLifePercent: 87,
            SystemStatusFlag: 0,
            BatteryLifeTime: 3600,
            BatteryFullLifeTime: u32::MAX,
        });
        assert_eq!(s.charge, Some(87.0));
        assert_eq!(s.state, BatteryState::Discharging);
        assert_eq!(s.time_left, Some(Duration::from_secs(3600)));
        assert_eq!(s.ac_power, Some(false));
        assert_eq!(s.rate, None);

        let s = system_sample(&SYSTEM_POWER_STATUS {
            ACLineStatus: 1,
            BatteryFlag: FLAG_CHARGING | 1,
            BatteryLifePercent: UNKNOWN_U8,
            SystemStatusFlag: 0,
            BatteryLifeTime: u32::MAX,
            BatteryFullLifeTime: u32::MAX,
        });
        assert_eq!(s.charge, None);
        assert_eq!(s.state, BatteryState::Charging);
        assert_eq!(s.time_left, None);
        assert_eq!(s.ac_power, Some(true));
    }

    #[test]
    fn the_device_fills_in_rate_capacities_and_charge() {
        let dev = BatteryDevice {
            handle: OwnedHandle(windows::Win32::Foundation::HANDLE::default()),
            tag: 1,
            info: BATTERY_INFORMATION {
                Capabilities: 0,
                Technology: 1,
                Reserved: [0; 3],
                Chemistry: *b"LION",
                DesignedCapacity: 60_000,
                FullChargedCapacity: 50_000,
                DefaultAlert1: 0,
                DefaultAlert2: 0,
                CriticalBias: 0,
                CycleCount: 123,
            },
            manufacturer: Some("SMP".into()),
        };
        let mut out = BatterySample::default();
        dev.apply(
            &BATTERY_STATUS {
                PowerState: BATTERY_DISCHARGING,
                Capacity: 25_000,
                Voltage: 11_400,
                Rate: -12_345,
            },
            &mut out,
        );
        assert_eq!(out.charge, Some(50.0));
        assert_eq!(out.state, BatteryState::Discharging);
        assert_eq!(out.rate, Some(Watts(12.345)));
        assert_eq!(out.full_capacity_mwh, Some(50_000));
        assert_eq!(out.design_capacity_mwh, Some(60_000));
        assert_eq!(out.cycle_count, Some(123));
        assert_eq!(out.chemistry.as_deref(), Some("LION"));
        assert_eq!(out.manufacturer.as_deref(), Some("SMP"));

        // Relative capacities are not milliwatt-hours; unknown rate stays unknown.
        let mut dev = dev;
        dev.info.Capabilities = BATTERY_CAPACITY_RELATIVE;
        let mut out = BatterySample::default();
        dev.apply(
            &BATTERY_STATUS {
                PowerState: BATTERY_POWER_ON_LINE,
                Capacity: 50_000,
                Voltage: 0,
                Rate: UNKNOWN_RATE,
            },
            &mut out,
        );
        assert_eq!(out.charge, Some(100.0));
        assert_eq!(out.state, BatteryState::Idle);
        assert_eq!(out.rate, None);
        assert_eq!(out.full_capacity_mwh, None);
        assert_eq!(out.design_capacity_mwh, None);
        // The handle is a null placeholder; forget it rather than close it.
        std::mem::forget(dev);
    }
}
