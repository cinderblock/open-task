//! Installed programs, from the registry's uninstall lists.
//!
//! Settings > Apps and Control Panel's Programs and Features read the same three
//! keys: `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall` for machine-wide
//! 64-bit programs, the 32-bit view of the same path (`WOW6432Node`) for 32-bit ones,
//! and the path under `HKCU` for programs installed for the current user only. Each
//! subkey is one entry, named by its `DisplayName`. Entries flagged `SystemComponent`,
//! entries naming a `ParentKeyName` (patches of another entry) and entries whose
//! `ReleaseType` calls them an update are hidden, as Programs and Features hides them.
//!
//! Each subkey is opened once and its values read through that handle with
//! `RegGetValueW`, which expands `REG_EXPAND_SZ` on the way out; one scratch buffer
//! serves every string read. The subkey's last-write time comes back from the
//! enumeration itself and stands in for the install date when the installer did not
//! record one, which is what Programs and Features shows too.
//!
//! The same product often appears under both the 64-bit and 32-bit keys with the same
//! name and version; the 64-bit entry wins. The list is sorted by name, ignoring case,
//! then by version.
//!
//! Cost: 30 to 45 ms for 761 subkeys (243 programs listed) on the development
//! machine, a Windows 11 desktop, measured by the ignored `listing_cost` test while
//! other builds were running. Enumerating and opening the subkeys is about 9 ms of
//! that; each value read is 4 to 5 µs, and a listed program takes about eleven of
//! them while a hidden entry stops after one to three. The file-existence check
//! behind `DisplayIcon` runs only for entries with no `InstallLocation`.

use std::cmp::Ordering;
use std::path::Path;

use ot_model::apps::InstalledApp;
use ot_model::Bytes;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME, SYSTEMTIME,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS,
    RRF_RT_REG_DWORD, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Time::FileTimeToSystemTime;

/// The uninstall list, relative to its hive. The 32-bit list is the same path seen
/// through the `WOW6432Node` redirection.
const UNINSTALL: PCWSTR = w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall");

/// Registry key names are at most 255 characters; one more for the terminator.
const MAX_KEY_NAME: usize = 256;

/// Upper bound on the string scratch buffer, in UTF-16 units. A value longer than
/// this is not a program name or command; give up rather than keep growing.
const MAX_STRING_UNITS: usize = 1 << 20;

/// One of the three lists and how its entries are flagged.
#[derive(Debug, Clone, Copy)]
struct Hive {
    root: HKEY,
    /// Which registry view to open the list in.
    view: REG_SAM_FLAGS,
    per_user: bool,
    x86: bool,
}

/// In the order Programs and Features merges them; the first copy of a duplicate wins.
const HIVES: [Hive; 3] = [
    Hive {
        root: HKEY_LOCAL_MACHINE,
        view: KEY_WOW64_64KEY,
        per_user: false,
        x86: false,
    },
    Hive {
        root: HKEY_LOCAL_MACHINE,
        view: KEY_WOW64_32KEY,
        per_user: false,
        x86: true,
    },
    Hive {
        root: HKEY_CURRENT_USER,
        view: KEY_WOW64_64KEY,
        per_user: true,
        x86: false,
    },
];

/// The programs the system's uninstall lists know about, sorted by name ignoring
/// case, then by version. Programs hidden from Programs and Features are left out.
pub(super) fn installed_apps() -> Vec<InstalledApp> {
    let mut apps = Vec::with_capacity(512);
    let mut scratch = Scratch::default();
    for hive in HIVES {
        scan_hive(hive, &mut scratch, &mut apps);
    }
    // Stable, so of two entries with the same name and version the earlier hive's
    // copy stays in front and survives the dedupe.
    apps.sort_by(order);
    apps.dedup_by(|later, earlier| later.name == earlier.name && later.version == earlier.version);
    apps
}

/// Name ignoring case, then version: the order the list is published in.
fn order(a: &InstalledApp, b: &InstalledApp) -> Ordering {
    let la = a.name.chars().flat_map(char::to_lowercase);
    let lb = b.name.chars().flat_map(char::to_lowercase);
    la.cmp(lb).then_with(|| a.version.cmp(&b.version))
}

/// Append every listed program under one hive's uninstall key.
fn scan_hive(hive: Hive, scratch: &mut Scratch, apps: &mut Vec<InstalledApp>) {
    let sam = KEY_READ | hive.view;
    let Some(list) = Key::open(hive.root, UNINSTALL, sam) else {
        // HKCU has no uninstall key until something installs per-user; not an error.
        return;
    };
    let mut name = [0u16; MAX_KEY_NAME];
    for index in 0u32.. {
        let mut len = name.len() as u32;
        let mut written = FILETIME::default();
        // SAFETY: `name` holds `len` units and the API writes at most that many plus
        // the terminator it accounts for; the out-pointers are valid locals.
        let status = unsafe {
            RegEnumKeyExW(
                list.0,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &raw mut len,
                None,
                None,
                None,
                Some(&raw mut written),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status == ERROR_MORE_DATA {
            // A name longer than the registry allows; skip it rather than stop.
            continue;
        }
        if status != ERROR_SUCCESS {
            tracing::debug!(
                index,
                code = status.0,
                "RegEnumKeyExW failed; list cut short"
            );
            break;
        }
        let len = (len as usize).min(name.len() - 1);
        name[len] = 0;
        let Some(key) = Key::open(list.0, PCWSTR(name.as_ptr()), sam) else {
            continue;
        };
        if let Some(app) = read_app(&key, hive, written, scratch) {
            apps.push(app);
        }
    }
}

/// One uninstall subkey as a listed program, or `None` when it is hidden or has no
/// name.
fn read_app(key: &Key, hive: Hive, written: FILETIME, s: &mut Scratch) -> Option<InstalledApp> {
    let name = s.string(key, w!("DisplayName")).filter(|n| !n.is_empty())?;
    if Key::dword(key, w!("SystemComponent")) == Some(1) {
        return None;
    }
    if s.string(key, w!("ParentKeyName")).is_some() {
        return None;
    }
    if s.string(key, w!("ReleaseType"))
        .is_some_and(|t| is_update(&t))
    {
        return None;
    }
    let publisher = s.string(key, w!("Publisher")).filter(|p| !p.is_empty());
    let version = s
        .string(key, w!("DisplayVersion"))
        .filter(|v| !v.is_empty());
    let installed_on = match s.string(key, w!("InstallDate")) {
        Some(recorded) => install_date(&recorded),
        None => filetime_date(written),
    };
    let size = Key::dword(key, w!("EstimatedSize"))
        .filter(|&kib| kib > 0)
        .map(|kib| Bytes::from_kib(u64::from(kib)));
    let location = s
        .string(key, w!("InstallLocation"))
        .map(|l| unquote(&l).to_owned())
        .filter(|l| !l.is_empty())
        .or_else(|| {
            let icon = s.string(key, w!("DisplayIcon"))?;
            let file = icon_file(&icon)?;
            Path::new(file).is_file().then(|| icon_dir(&icon)).flatten()
        });
    let uninstall = s
        .string(key, w!("QuietUninstallString"))
        .filter(|u| !u.is_empty())
        .or_else(|| s.string(key, w!("UninstallString")))
        .filter(|u| !u.is_empty());
    Some(InstalledApp {
        name,
        publisher,
        version,
        installed_on,
        size,
        location,
        uninstall,
        per_user: hive.per_user,
        x86: hive.x86,
    })
}

/// `ReleaseType` values that mark a patch of another product rather than a product.
fn is_update(release_type: &str) -> bool {
    ["Update", "Hotfix", "Security Update"]
        .iter()
        .any(|kind| release_type.eq_ignore_ascii_case(kind))
}

/// `InstallDate` as `YYYY-MM-DD`. Installers write `YYYYMMDD`; a few write the
/// dashed form already. Anything else is `None` rather than a guess.
fn install_date(recorded: &str) -> Option<String> {
    let recorded = recorded.trim();
    if recorded.len() == 8 && recorded.bytes().all(|b| b.is_ascii_digit()) {
        return Some(format!(
            "{}-{}-{}",
            &recorded[..4],
            &recorded[4..6],
            &recorded[6..]
        ));
    }
    is_iso_date(recorded).then(|| recorded.to_owned())
}

/// Exactly `YYYY-MM-DD` with digits in every other position.
fn is_iso_date(s: &str) -> bool {
    s.len() == 10
        && s.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        })
}

/// A key's last-write time as `YYYY-MM-DD`, or `None` for a time that cannot be real.
fn filetime_date(written: FILETIME) -> Option<String> {
    let mut st = SYSTEMTIME::default();
    // SAFETY: both pointers are to valid locals of the right types.
    unsafe { FileTimeToSystemTime(&raw const written, &raw mut st) }.ok()?;
    (st.wYear >= 1970).then(|| format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay))
}

/// Strip surrounding quotes and whitespace from a path value.
fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"').trim()
}

/// The file a `DisplayIcon` value names: quotes and a trailing `,index` removed.
fn icon_file(display_icon: &str) -> Option<&str> {
    let mut path = unquote(display_icon);
    if let Some((file, index)) = path.rsplit_once(',') {
        if index.trim().parse::<i32>().is_ok() {
            path = unquote(file);
        }
    }
    (!path.is_empty()).then_some(path)
}

/// The directory of the file a `DisplayIcon` value names.
fn icon_dir(display_icon: &str) -> Option<String> {
    let file = icon_file(display_icon)?;
    let dir = Path::new(file).parent()?.to_str()?;
    (!dir.is_empty()).then(|| dir.to_owned())
}

/// An open registry key, closed on drop.
#[derive(Debug)]
struct Key(HKEY);

impl Key {
    fn open(root: HKEY, path: PCWSTR, sam: REG_SAM_FLAGS) -> Option<Self> {
        let mut key = HKEY(std::ptr::null_mut());
        // SAFETY: `path` is NUL-terminated (a literal or a buffer terminated by the
        // caller) and the out-pointer is a valid local.
        let status = unsafe { RegOpenKeyExW(root, path, None, sam, &raw mut key) };
        (status == ERROR_SUCCESS).then_some(Self(key))
    }

    /// A `REG_DWORD` value of this key.
    fn dword(key: &Self, name: PCWSTR) -> Option<u32> {
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: the buffer is a u32 and `size` says so; a null subkey reads the
        // key itself.
        let status = unsafe {
            RegGetValueW(
                key.0,
                PCWSTR::null(),
                name,
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut value).cast()),
                Some(&raw mut size),
            )
        };
        (status == ERROR_SUCCESS).then_some(value)
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW and is closed once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Reusable buffer for string value reads.
#[derive(Debug, Default)]
struct Scratch(Vec<u16>);

impl Scratch {
    /// A `REG_SZ` or `REG_EXPAND_SZ` value of `key`, expanded and trimmed. `Some("")`
    /// when the value exists but is empty; `None` when it is missing or another type.
    fn string(&mut self, key: &Key, name: PCWSTR) -> Option<String> {
        if self.0.len() < 512 {
            self.0.resize(512, 0);
        }
        loop {
            let mut size = (self.0.len() * 2) as u32;
            // SAFETY: the buffer and its byte size agree; a null subkey reads the
            // key itself; the value name is a NUL-terminated literal.
            let status = unsafe {
                RegGetValueW(
                    key.0,
                    PCWSTR::null(),
                    name,
                    RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                    None,
                    Some(self.0.as_mut_ptr().cast()),
                    Some(&raw mut size),
                )
            };
            if status == ERROR_SUCCESS {
                let units = (size as usize / 2).min(self.0.len());
                let end = self.0[..units]
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(units);
                return Some(String::from_utf16_lossy(&self.0[..end]).trim().to_owned());
            }
            if status != ERROR_MORE_DATA || self.0.len() >= MAX_STRING_UNITS {
                return None;
            }
            // `size` is what the value needs; expansion can make it larger than the
            // stored data, so take the bigger of that and a doubling.
            let needed = (size as usize).div_ceil(2) + 1;
            self.0.resize(needed.max(self.0.len() * 2), 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn the_uninstall_keys_list_real_programs() {
        let apps = installed_apps();
        assert!(apps.len() > 10, "only {} programs listed", apps.len());
        assert!(
            apps.iter().all(|a| !a.name.is_empty()),
            "an entry has no name"
        );
        for pair in apps.windows(2) {
            assert_ne!(
                order(&pair[0], &pair[1]),
                Ordering::Greater,
                "{:?} sorts after {:?}",
                pair[0].name,
                pair[1].name
            );
        }
        let mut seen = HashSet::new();
        for a in &apps {
            assert!(
                seen.insert((a.name.as_str(), a.version.as_deref())),
                "{:?} {:?} listed twice",
                a.name,
                a.version
            );
        }
        for a in &apps {
            if let Some(d) = &a.installed_on {
                assert!(is_iso_date(d), "{:?} has install date {d:?}", a.name);
            }
        }
        // A 64-bit Windows with a real set of programs has 32-bit ones, and most
        // users have something installed just for themselves.
        assert!(
            apps.iter().any(|a| a.x86 || a.per_user),
            "no 32-bit or per-user program among {} entries",
            apps.len()
        );
        // The populated fields look like what the registry holds.
        assert!(apps.iter().any(|a| a.publisher.is_some()));
        assert!(apps.iter().any(|a| a.version.is_some()));
        assert!(apps.iter().any(|a| a.uninstall.is_some()));
        assert!(apps.iter().any(|a| a.location.is_some()));
        assert!(apps.iter().any(|a| a.size.is_some_and(|s| s.get() > 0)));
    }

    #[test]
    fn display_icon_paths_reduce_to_their_directory() {
        assert_eq!(
            icon_dir(r#""C:\Program Files\X\x.exe",0"#).as_deref(),
            Some(r"C:\Program Files\X")
        );
        assert_eq!(
            icon_dir(r"C:\Program Files\X\x.exe,-101").as_deref(),
            Some(r"C:\Program Files\X")
        );
        assert_eq!(
            icon_dir(r"C:\Program Files\X\app.ico").as_deref(),
            Some(r"C:\Program Files\X")
        );
        assert_eq!(
            icon_dir(r#" "C:\Tools\t.exe" "#).as_deref(),
            Some(r"C:\Tools")
        );
        // A comma that is not an icon index stays part of the path.
        assert_eq!(
            icon_dir(r"C:\Odd, Inc\x.exe").as_deref(),
            Some(r"C:\Odd, Inc")
        );
        assert_eq!(icon_dir("x.exe"), None);
        assert_eq!(icon_dir(""), None);
        assert_eq!(icon_dir(",0"), None);
    }

    #[test]
    fn install_dates_are_iso_or_absent() {
        assert_eq!(install_date("20240115").as_deref(), Some("2024-01-15"));
        assert_eq!(install_date(" 20240115 ").as_deref(), Some("2024-01-15"));
        assert_eq!(install_date("2024-01-15").as_deref(), Some("2024-01-15"));
        assert_eq!(install_date("2024"), None);
        assert_eq!(install_date("01/15/2024"), None);
        assert_eq!(install_date(""), None);
        assert_eq!(install_date("2024011x"), None);
    }

    #[test]
    fn release_types_that_are_patches_are_hidden() {
        assert!(is_update("Update"));
        assert!(is_update("Security Update"));
        assert!(is_update("hotfix"));
        assert!(!is_update("Service Pack"));
        assert!(!is_update(""));
    }

    /// `cargo test -p ot-probe installed -- --ignored --nocapture` prints the cost;
    /// the module doc carries the last measurement.
    #[test]
    #[ignore = "timing; prints the cost of one listing"]
    fn listing_cost() {
        let mut best = Duration::MAX;
        let mut worst = Duration::ZERO;
        let mut apps = Vec::new();
        for _ in 0..10 {
            let t = Instant::now();
            apps = installed_apps();
            let took = t.elapsed();
            best = best.min(took);
            worst = worst.max(took);
        }
        let count = |f: fn(&InstalledApp) -> bool| apps.iter().filter(|a| f(a)).count();
        println!(
            "installed_apps: {} programs in {best:?} best, {worst:?} worst of 10; \
             {} x86, {} per-user, {} with a date, {} with a size, {} with a location, \
             {} with an uninstall command",
            apps.len(),
            count(|a| a.x86),
            count(|a| a.per_user),
            count(|a| a.installed_on.is_some()),
            count(|a| a.size.is_some()),
            count(|a| a.location.is_some()),
            count(|a| a.uninstall.is_some()),
        );
        for a in apps.iter().step_by(apps.len().max(1) / 8) {
            println!("  {a:?}");
        }
        assert!(best < Duration::from_millis(100), "{best:?}");
    }
}
