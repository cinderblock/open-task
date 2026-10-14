//! The user's settings, kept in the registry, and the system settings the view
//! follows.
//!
//! Settings live under `HKCU\Software\open-task`, one `REG_DWORD` each
//! (`AnimateRows`, `SmoothCharts`, `CheckForUpdates`, `DownloadUpdates`,
//! `InstallUpdates`, and the numbers `UsageDecayPercent`, `HistoryMinutes`,
//! `ChartKneeSamples`, `ChartDrawing`, 0 GPU, 1 CPU, 2 Direct2D, and
//! `ChartLegend`, 0 in the bands, 1 floating, 2 beside) plus
//! `Layout`, a `REG_SZ` with the page, arrangement, sort and columns the last
//! session ended on, per user like every other per-user preference on Windows.
//! The subkey `Placements` holds where the window was on each monitor layout, one
//! `REG_SZ` per layout (see `placement`). A missing key or value means the
//! default. Nothing here is fatal: a value that cannot be read or written is
//! logged and the app carries on with what it has.

use std::ffi::c_void;

use ot_shell_core::settings_store;
use ot_ui::{Settings, ViewLayout};
use windows::core::{w, BOOL, HSTRING, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

const KEY: PCWSTR = w!(r"Software\open-task");
const PLACEMENTS_KEY: PCWSTR = w!(r"Software\open-task\Placements");
/// [`settings_store::LAYOUT`].
const LAYOUT: PCWSTR = w!("Layout");

/// The settings in the registry, for `ot_shell_core`.
#[derive(Debug)]
pub(crate) struct WinPrefs;

impl ot_shell_core::Prefs for WinPrefs {
    fn load(&self) -> Settings {
        load()
    }

    fn save(&self, settings: &Settings) {
        save(settings);
    }

    fn load_layout(&self) -> Option<ViewLayout> {
        load_layout()
    }

    fn save_layout(&self, layout: &ViewLayout) {
        save_layout(layout);
    }
}

/// The settings as last saved, defaults for anything never saved.
pub fn load() -> Settings {
    settings_store::decode(read_dword)
}

/// Store the settings for the next start. A setting still following the system
/// (never chosen) is not written.
pub fn save(s: &Settings) {
    let Some(key) = open_for_writing(KEY) else {
        return;
    };
    for (name, value) in settings_store::encode(s) {
        let data = value.to_le_bytes();
        let name = HSTRING::from(name);
        // SAFETY: `key` was just opened with KEY_SET_VALUE; the name and data
        // outlive the call.
        let status = unsafe { RegSetValueExW(key, &name, None, REG_DWORD, Some(&data)) };
        if status.is_err() {
            tracing::warn!(?status, "could not save a setting");
        }
    }
    // SAFETY: opened above, closed once.
    unsafe {
        let _ = RegCloseKey(key);
    }
}

/// The page, arrangement, sort and columns the last session ended on, if a
/// session saved them.
pub fn load_layout() -> Option<ViewLayout> {
    ViewLayout::decode(&read_string(KEY, LAYOUT)?)
}

/// Store the layout for the next start.
pub fn save_layout(layout: &ViewLayout) {
    write_string(KEY, LAYOUT, &layout.encode());
}

/// Where the window was last left on the monitor layout `layout`, as
/// `placement` wrote it.
pub fn load_placement(layout: &str) -> Option<String> {
    let name = HSTRING::from(layout);
    read_string(PLACEMENTS_KEY, PCWSTR(name.as_ptr()))
}

/// Store where the window is on the monitor layout `layout`.
pub fn save_placement(layout: &str, placement: &str) {
    let name = HSTRING::from(layout);
    write_string(PLACEMENTS_KEY, PCWSTR(name.as_ptr()), placement);
}

/// Write one `REG_SZ` value under `path`, logging a failure.
fn write_string(path: PCWSTR, name: PCWSTR, text: &str) {
    let Some(key) = open_for_writing(path) else {
        return;
    };
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let data: Vec<u8> = wide.iter().flat_map(|c| c.to_le_bytes()).collect();
    // SAFETY: `key` was just opened with KEY_SET_VALUE; the name and data are
    // NUL-terminated UTF-16 strings that outlive the call.
    let status = unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(&data)) };
    if status.is_err() {
        tracing::warn!(?status, "could not save a setting");
    }
    // SAFETY: opened above, closed once.
    unsafe {
        let _ = RegCloseKey(key);
    }
}

/// A settings key, created if needed, open for writing. The caller closes it.
fn open_for_writing(path: PCWSTR) -> Option<HKEY> {
    let mut key = HKEY::default();
    // SAFETY: the out-pointer is a local; the strings are static.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path,
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
        return None;
    }
    Some(key)
}

fn read_string(path: PCWSTR, name: PCWSTR) -> Option<String> {
    let mut size: u32 = 0;
    // SAFETY: with no buffer, RegGetValueW writes the needed size to `size`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            path,
            name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&raw mut size),
        )
    };
    if status.is_err() || size < 2 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: the buffer holds `size` bytes, as asked for above; `size` is updated
    // to what was written.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            path,
            name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

fn read_dword(name: &str) -> Option<u32> {
    let name = HSTRING::from(name);
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the out-pointers reference locals that outlive the call; `size` matches.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            &name,
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
