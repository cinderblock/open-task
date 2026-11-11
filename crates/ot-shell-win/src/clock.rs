//! Local time for logs a person reads, by Windows' own time zone rules.

use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::{
    FileTimeToSystemTime, GetTimeZoneInformation, SystemTimeToFileTime,
    SystemTimeToTzSpecificLocalTime, TIME_ZONE_INFORMATION,
};

/// 100 ns intervals from 1601-01-01 (`FILETIME`'s epoch) to the Unix epoch.
const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

/// `at` in the computer's time zone, with the zone named as its initials
/// (`2026-11-10 4:30:12 PM PST`), the daylight name while daylight time applies
/// on that date. `None` before 1970 or if Windows cannot convert it.
pub(crate) fn local_time(at: SystemTime) -> Option<String> {
    let ticks = u64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_nanos() / 100).ok()?
        + UNIX_EPOCH_TICKS;
    let file = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    let mut zone = TIME_ZONE_INFORMATION::default();
    let mut local_file = FILETIME::default();
    // SAFETY: plain conversions between locals; every pointer outlives its call.
    unsafe {
        FileTimeToSystemTime(&raw const file, &raw mut utc).ok()?;
        GetTimeZoneInformation(&raw mut zone);
        // No zone given: the computer's, with the daylight rules of that year.
        SystemTimeToTzSpecificLocalTime(None, &raw const utc, &raw mut local).ok()?;
        SystemTimeToFileTime(&raw const local, &raw mut local_file).ok()?;
    }
    let local_ticks =
        (u64::from(local_file.dwHighDateTime) << 32) | u64::from(local_file.dwLowDateTime);
    let offset_min =
        i64::try_from((i128::from(local_ticks) - i128::from(ticks)) / 600_000_000).ok()?;
    let standard = -i64::from(zone.Bias + zone.StandardBias);
    let name = if offset_min == standard {
        &zone.StandardName
    } else {
        &zone.DaylightName
    };
    let (hour, pm) = match local.wHour {
        0 => (12, false),
        h @ 1..=11 => (h, false),
        12 => (12, true),
        h => (h - 12, true),
    };
    Some(format!(
        "{}-{:02}-{:02} {hour}:{:02}:{:02} {} {}",
        local.wYear,
        local.wMonth,
        local.wDay,
        local.wMinute,
        local.wSecond,
        if pm { "PM" } else { "AM" },
        abbreviation(name, offset_min)
    ))
}

/// `Pacific Daylight Time` as `PDT`; a name that is not words of letters, or is
/// missing, as its offset, `UTC-07:00`.
fn abbreviation(name: &[u16; 32], offset_min: i64) -> String {
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let name = String::from_utf16_lossy(&name[..end]);
    let words: Vec<&str> = name.split_whitespace().collect();
    let lettered = words.len() >= 2
        && words
            .iter()
            .all(|w| w.chars().all(|c| c.is_ascii_alphabetic() || c == '.'));
    if lettered {
        return words
            .iter()
            .filter_map(|w| w.chars().next())
            .map(|c| c.to_ascii_uppercase())
            .collect();
    }
    let sign = if offset_min < 0 { '-' } else { '+' };
    let m = offset_min.abs();
    format!("UTC{sign}{:02}:{:02}", m / 60, m % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> [u16; 32] {
        let mut a = [0u16; 32];
        for (d, c) in a.iter_mut().zip(s.encode_utf16()) {
            *d = c;
        }
        a
    }

    #[test]
    fn zones_are_named_by_their_initials_or_their_offset() {
        assert_eq!(abbreviation(&name("Pacific Daylight Time"), -420), "PDT");
        assert_eq!(abbreviation(&name("Pacific Standard Time"), -480), "PST");
        assert_eq!(abbreviation(&name(""), 330), "UTC+05:30");
        assert_eq!(abbreviation(&name("Tokyo"), 540), "UTC+09:00");
    }

    #[test]
    fn a_time_converts_and_reads_as_a_clock() {
        let t = local_time(UNIX_EPOCH + std::time::Duration::from_secs(1_794_343_443)).unwrap();
        // 2026-11-10 20:44:03Z, in whatever zone this machine is in.
        assert!(t.contains(":44:03 "), "{t}");
        assert!(t.contains(" AM ") || t.contains(" PM "), "{t}");
    }
}
