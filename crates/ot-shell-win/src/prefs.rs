//! The user's settings, kept in the registry, and the system settings the view
//! follows.
//!
//! Settings live under `HKCU\Software\open-task`, one `REG_DWORD` each
//! (`AnimateRows`, `SmoothCharts`, `CheckForUpdates`, `DownloadUpdates`,
//! `InstallUpdates`, and the numbers `UsageDecayPercent`, `HistoryMinutes` and
//! `ChartDrawing`, 0 GPU, 1 CPU, 2 Direct2D) plus
//! `Layout`, a `REG_SZ` with the page, arrangement, sort and columns the last
//! session ended on, per user like every other per-user preference on Windows. A missing key or value means the
//! default. Nothing here is fatal: a value that cannot be read or written is
//! logged and the app carries on with what it has.

use std::ffi::c_void;

use ot_ui::{ChartDrawing, Settings, ViewLayout};
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

const KEY: PCWSTR = w!(r"Software\open-task");
const ANIMATE_ROWS: PCWSTR = w!("AnimateRows");
const SMOOTH_CHARTS: PCWSTR = w!("SmoothCharts");
const CHECK_UPDATES: PCWSTR = w!("CheckForUpdates");
const DOWNLOAD_UPDATES: PCWSTR = w!("DownloadUpdates");
const INSTALL_UPDATES: PCWSTR = w!("InstallUpdates");
const USAGE_DECAY: PCWSTR = w!("UsageDecayPercent");
const HISTORY_MINUTES: PCWSTR = w!("HistoryMinutes");
const ALWAYS_ON_TOP: PCWSTR = w!("AlwaysOnTop");
const HIDE_WHEN_MINIMIZED: PCWSTR = w!("HideWhenMinimized");
const MINIMIZE_ON_USE: PCWSTR = w!("MinimizeOnUse");
const RESOURCE_PERCENT: PCWSTR = w!("ResourceValuesAsPercent");
const UPDATE_INTERVAL: PCWSTR = w!("UpdateIntervalMs");
const CHART_DRAWING: PCWSTR = w!("ChartDrawing");
const LAYOUT: PCWSTR = w!("Layout");

/// The settings as last saved, defaults for anything never saved.
pub fn load() -> Settings {
    let defaults = Settings::default();
    let flag = |name, default| read_dword(name).map_or(default, |v| v != 0);
    let settings = Settings {
        animate_rows: read_dword(ANIMATE_ROWS).map(|v| v != 0),
        smooth_charts: flag(SMOOTH_CHARTS, defaults.smooth_charts),
        check_updates: flag(CHECK_UPDATES, defaults.check_updates),
        download_updates: flag(DOWNLOAD_UPDATES, defaults.download_updates),
        install_updates: flag(INSTALL_UPDATES, defaults.install_updates),
        always_on_top: flag(ALWAYS_ON_TOP, defaults.always_on_top),
        hide_when_minimized: flag(HIDE_WHEN_MINIMIZED, defaults.hide_when_minimized),
        minimize_on_use: flag(MINIMIZE_ON_USE, defaults.minimize_on_use),
        resource_percent: flag(RESOURCE_PERCENT, defaults.resource_percent),
        chart_drawing: read_dword(CHART_DRAWING)
            .map_or(defaults.chart_drawing, ChartDrawing::from_index),
        ..defaults
    };
    let settings = match read_dword(UPDATE_INTERVAL) {
        Some(ms) => settings.with_interval(ms),
        None => settings,
    };
    let settings = match read_dword(USAGE_DECAY) {
        Some(percent) => settings.with_usage_decay(percent),
        None => settings,
    };
    match read_dword(HISTORY_MINUTES) {
        Some(minutes) => settings.with_history_minutes(minutes),
        None => settings,
    }
}

/// Store the settings for the next start. A setting still following the system
/// (never chosen) is not written.
pub fn save(s: &Settings) {
    let values = [
        (ANIMATE_ROWS, s.animate_rows.map(u32::from)),
        (SMOOTH_CHARTS, Some(u32::from(s.smooth_charts))),
        (CHECK_UPDATES, Some(u32::from(s.check_updates))),
        (DOWNLOAD_UPDATES, Some(u32::from(s.download_updates))),
        (INSTALL_UPDATES, Some(u32::from(s.install_updates))),
        (USAGE_DECAY, Some(u32::from(s.usage_decay_percent))),
        (HISTORY_MINUTES, Some(s.history_minutes)),
        (ALWAYS_ON_TOP, Some(u32::from(s.always_on_top))),
        (HIDE_WHEN_MINIMIZED, Some(u32::from(s.hide_when_minimized))),
        (MINIMIZE_ON_USE, Some(u32::from(s.minimize_on_use))),
        (RESOURCE_PERCENT, Some(u32::from(s.resource_percent))),
        (UPDATE_INTERVAL, Some(s.update_interval_ms)),
        (CHART_DRAWING, Some(s.chart_drawing.index())),
    ];
    let Some(key) = open_for_writing() else {
        return;
    };
    for (name, value) in values {
        let Some(value) = value else {
            continue;
        };
        let data = value.to_le_bytes();
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

/// The page, arrangement, sort and columns the last session ended on, if a
/// session saved them.
pub fn load_layout() -> Option<ViewLayout> {
    ViewLayout::decode(&read_string(LAYOUT)?)
}

/// Store the layout for the next start.
pub fn save_layout(layout: &ViewLayout) {
    let Some(key) = open_for_writing() else {
        return;
    };
    let text: Vec<u16> = layout.encode().encode_utf16().chain(Some(0)).collect();
    let data: Vec<u8> = text.iter().flat_map(|c| c.to_le_bytes()).collect();
    // SAFETY: `key` was just opened with KEY_SET_VALUE; the data is a
    // NUL-terminated UTF-16 string that outlives the call.
    let status = unsafe { RegSetValueExW(key, LAYOUT, None, REG_SZ, Some(&data)) };
    if status.is_err() {
        tracing::warn!(?status, "could not save the layout");
    }
    // SAFETY: opened above, closed once.
    unsafe {
        let _ = RegCloseKey(key);
    }
}

/// The settings key, created if needed, open for writing. The caller closes it.
fn open_for_writing() -> Option<HKEY> {
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
        return None;
    }
    Some(key)
}

fn read_string(name: PCWSTR) -> Option<String> {
    let mut size: u32 = 0;
    // SAFETY: with no buffer, RegGetValueW writes the needed size to `size`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
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
            KEY,
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
