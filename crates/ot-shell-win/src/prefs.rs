//! The user's settings, kept in the registry, and the system settings the view
//! follows.
//!
//! Settings live under `HKCU\Software\open-task`, one `REG_DWORD` each
//! (`AnimateRows`, `CheckForUpdates`, `DownloadUpdates`, `InstallUpdates`), per user
//! like every other per-user preference on Windows. A missing key or value means the
//! default. Nothing here is fatal: a value that cannot be read or written is
//! logged and the app carries on with what it has.

use std::ffi::c_void;

use ot_ui::Settings;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

const KEY: PCWSTR = w!(r"Software\open-task");
const ANIMATE_ROWS: PCWSTR = w!("AnimateRows");
const CHECK_UPDATES: PCWSTR = w!("CheckForUpdates");
const DOWNLOAD_UPDATES: PCWSTR = w!("DownloadUpdates");
const INSTALL_UPDATES: PCWSTR = w!("InstallUpdates");

/// The settings as last saved, defaults for anything never saved.
pub fn load() -> Settings {
    let defaults = Settings::default();
    let flag = |name, default| read_dword(name).map_or(default, |v| v != 0);
    Settings {
        animate_rows: read_dword(ANIMATE_ROWS).map(|v| v != 0),
        check_updates: flag(CHECK_UPDATES, defaults.check_updates),
        download_updates: flag(DOWNLOAD_UPDATES, defaults.download_updates),
        install_updates: flag(INSTALL_UPDATES, defaults.install_updates),
    }
}

/// Store the settings for the next start. A setting still following the system
/// (never chosen) is not written.
pub fn save(s: &Settings) {
    let values = [
        (ANIMATE_ROWS, s.animate_rows),
        (CHECK_UPDATES, Some(s.check_updates)),
        (DOWNLOAD_UPDATES, Some(s.download_updates)),
        (INSTALL_UPDATES, Some(s.install_updates)),
    ];
    let mut key = HKEY::default();
    // SAFETY: the out-pointer is a local; the strings are static.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            KEY,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &raw mut key,
            None,
        )
    };
    if status.is_err() {
        tracing::warn!(?status, "could not open the settings key");
        return;
    }
    for (name, value) in values {
        let Some(value) = value else {
            continue;
        };
        let data = u32::from(value).to_le_bytes();
        // SAFETY: `key` was just opened with KEY_SET_VALUE; the data outlives the call.
        let status = unsafe { RegSetValueExW(key, name, None, REG_DWORD, Some(&data)) };
        if status.is_err() {
            tracing::warn!(?status, "could not save a setting");
        }
    }
    // SAFETY: opened above, closed once.
    unsafe {
        let _ = RegCloseKey(key);
    }
}

fn read_dword(name: PCWSTR) -> Option<u32> {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the out-pointers reference locals that outlive the call; `size` matches.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    status.is_ok().then_some(value)
}

/// Windows' "Animation effects" (Accessibility > Visual effects). Assumed on if it
/// cannot be read, which is Windows' own default.
pub fn system_animations() -> bool {
    let mut on = BOOL(1);
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the pointer.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&raw mut on).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_err() || on.as_bool()
}
