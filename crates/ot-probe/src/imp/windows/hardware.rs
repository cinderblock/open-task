//! Static hardware facts, read once at startup.
//!
//! - Topology and caches: `GetLogicalProcessorInformationEx(RelationAll)`, one walk.
//! - Processor name: `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0`,
//!   `ProcessorNameString`, which Windows fills from CPUID at boot.
//! - Base clock: `CallNtPowerInformation(ProcessorInformation)`, `MaxMhz`. That is
//!   the rated frequency Task Manager calls base speed. The same call's
//!   `CurrentMhz` is not used anywhere: on modern Windows it stays at the base
//!   value while the cores boost, so it would be quietly wrong.
//! - Boot time: now minus `GetTickCount64`.

use std::mem::size_of;
use std::time::{SystemTime, UNIX_EPOCH};

use ot_model::hardware::Hardware;
use ot_model::{Bytes, Hertz};
use windows::core::w;
use windows::Win32::Foundation::STATUS_SUCCESS;
use windows::Win32::System::Power::{
    CallNtPowerInformation, ProcessorInformation, PROCESSOR_POWER_INFORMATION,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetTickCount64, RelationAll, RelationCache,
    RelationProcessorCore, RelationProcessorPackage, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

use super::AlignedBuf;

/// Everything this module can learn. Never fails as a whole: each fact that cannot
/// be read is left empty.
pub(super) fn read(logical_processors: u32) -> Hardware {
    let mut hw = Hardware {
        logical_processors,
        cpu_name: cpu_name(),
        base_frequency: base_frequency(logical_processors),
        boot_unix_ms: boot_unix_ms(),
        ..Hardware::default()
    };
    topology(&mut hw);
    hw
}

fn cpu_name() -> Option<String> {
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: the buffer and its byte size agree; the strings are static.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"),
            w!("ProcessorNameString"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    let end = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
    // Intel pads the name with spaces on both sides.
    let name = String::from_utf16_lossy(&buf[..end]).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

fn base_frequency(logical: u32) -> Option<Hertz> {
    let mut info = vec![PROCESSOR_POWER_INFORMATION::default(); logical.max(1) as usize];
    let bytes = (info.len() * size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    // SAFETY: the output buffer holds one entry per logical processor, as required.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            None,
            0,
            Some(info.as_mut_ptr().cast()),
            bytes,
        )
    };
    if status != STATUS_SUCCESS {
        return None;
    }
    let mhz = info.iter().map(|p| p.MaxMhz).max().unwrap_or(0);
    (mhz > 0).then(|| Hertz::from_mhz(u64::from(mhz)))
}

fn boot_unix_ms() -> Option<i64> {
    // SAFETY: no arguments.
    let up_ms = unsafe { GetTickCount64() };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let now_ms = i64::try_from(now.as_millis()).ok()?;
    Some(now_ms - i64::try_from(up_ms).ok()?)
}

/// Sockets, cores and cache sizes from one `RelationAll` walk.
fn topology(hw: &mut Hardware) {
    let mut len = 0u32;
    // SAFETY: a null buffer with zero length is the documented size query.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationAll, None, &raw mut len) };
    if len == 0 {
        return;
    }
    let mut buf = AlignedBuf::default();
    buf.resize_bytes(len as usize);
    // SAFETY: the buffer is `len` bytes and 8-byte aligned, as asked for.
    let ok = unsafe {
        GetLogicalProcessorInformationEx(RelationAll, Some(buf.as_mut_ptr().cast()), &raw mut len)
    }
    .is_ok();
    if !ok {
        return;
    }

    let mut cache = [0u64; 4];
    let base = buf.as_ptr();
    let mut offset = 0usize;
    while offset + size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() <= len as usize {
        // SAFETY: within the bytes the API wrote; entries are 8-byte aligned.
        let info = unsafe {
            &*base
                .byte_add(offset)
                .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>()
        };
        match info.Relationship {
            r if r == RelationProcessorPackage => hw.sockets += 1,
            r if r == RelationProcessorCore => hw.physical_cores += 1,
            r if r == RelationCache => {
                // SAFETY: Relationship says the Cache member of the union is active.
                let c = unsafe { info.Anonymous.Cache };
                if let Some(slot) = cache.get_mut(usize::from(c.Level)) {
                    *slot += u64::from(c.CacheSize);
                }
            }
            _ => {}
        }
        if info.Size == 0 {
            break;
        }
        offset += info.Size as usize;
    }
    let level = |n: usize| (cache[n] > 0).then_some(Bytes(cache[n]));
    hw.cache_l1 = level(1);
    hw.cache_l2 = level(2);
    hw.cache_l3 = level(3);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_reports_sensible_hardware() {
        let hw = read(1);
        assert!(hw.sockets >= 1, "{hw:?}");
        assert!(hw.physical_cores >= hw.sockets, "{hw:?}");
        assert!(
            hw.cpu_name.as_deref().is_some_and(|n| !n.is_empty()),
            "{hw:?}"
        );
        assert!(
            hw.cache_l1.is_some(),
            "every x86 and Arm core has an L1: {hw:?}"
        );
        let boot = hw.boot_unix_ms.expect("boot time");
        assert!(boot > 1_500_000_000_000, "{boot}");
    }
}
