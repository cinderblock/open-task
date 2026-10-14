//! The settings as named numbers, for whatever store a platform keeps them in:
//! `REG_DWORD`s under `HKCU\Software\open-task` on Windows, lines of a file on
//! Linux. One list of names, so the two stay alike.

use ot_ui::{ChartDrawing, ChartLegend, Settings};

pub const ANIMATE_ROWS: &str = "AnimateRows";
pub const SMOOTH_CHARTS: &str = "SmoothCharts";
pub const CHECK_UPDATES: &str = "CheckForUpdates";
pub const DOWNLOAD_UPDATES: &str = "DownloadUpdates";
pub const INSTALL_UPDATES: &str = "InstallUpdates";
pub const USAGE_DECAY: &str = "UsageDecayPercent";
pub const HISTORY_MINUTES: &str = "HistoryMinutes";
pub const ALWAYS_ON_TOP: &str = "AlwaysOnTop";
pub const HIDE_WHEN_MINIMIZED: &str = "HideWhenMinimized";
pub const MINIMIZE_ON_USE: &str = "MinimizeOnUse";
pub const RESOURCE_PERCENT: &str = "ResourceValuesAsPercent";
pub const UPDATE_INTERVAL: &str = "UpdateIntervalMs";
pub const CHART_DRAWING: &str = "ChartDrawing";
pub const CHART_KNEE: &str = "ChartKneeSamples";
pub const CHART_LEGEND: &str = "ChartLegend";
/// A string, not a number: the page, arrangement, sort and columns the last
/// session ended on ([`ot_ui::ViewLayout::encode`]).
pub const LAYOUT: &str = "Layout";

/// The settings from a store, defaults for anything never saved. `read` gives a
/// stored number by name.
pub fn decode(read: impl Fn(&str) -> Option<u32>) -> Settings {
    let defaults = Settings::default();
    let flag = |name, default| read(name).map_or(default, |v| v != 0);
    let settings = Settings {
        animate_rows: read(ANIMATE_ROWS).map(|v| v != 0),
        smooth_charts: flag(SMOOTH_CHARTS, defaults.smooth_charts),
        check_updates: flag(CHECK_UPDATES, defaults.check_updates),
        download_updates: flag(DOWNLOAD_UPDATES, defaults.download_updates),
        install_updates: flag(INSTALL_UPDATES, defaults.install_updates),
        always_on_top: flag(ALWAYS_ON_TOP, defaults.always_on_top),
        hide_when_minimized: flag(HIDE_WHEN_MINIMIZED, defaults.hide_when_minimized),
        minimize_on_use: flag(MINIMIZE_ON_USE, defaults.minimize_on_use),
        resource_percent: flag(RESOURCE_PERCENT, defaults.resource_percent),
        chart_drawing: read(CHART_DRAWING).map_or(defaults.chart_drawing, ChartDrawing::from_index),
        chart_legend: read(CHART_LEGEND).map_or(defaults.chart_legend, ChartLegend::from_index),
        ..defaults
    };
    let settings = match read(UPDATE_INTERVAL) {
        Some(ms) => settings.with_interval(ms),
        None => settings,
    };
    let settings = match read(CHART_KNEE) {
        Some(samples) => settings.with_knee_samples(samples),
        None => settings,
    };
    let settings = match read(USAGE_DECAY) {
        Some(percent) => settings.with_usage_decay(percent),
        None => settings,
    };
    match read(HISTORY_MINUTES) {
        Some(minutes) => settings.with_history_minutes(minutes),
        None => settings,
    }
}

/// The settings as named numbers to store. A setting still following the system
/// (never chosen) is absent.
#[must_use]
pub fn encode(s: &Settings) -> Vec<(&'static str, u32)> {
    [
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
        (CHART_KNEE, Some(u32::from(s.chart_knee_samples))),
        (CHART_LEGEND, Some(s.chart_legend.index())),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some((name, value?)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn settings_survive_the_round_trip() {
        let mut s = Settings::default();
        s.always_on_top = !s.always_on_top;
        s.animate_rows = Some(false);
        let s = s.with_interval(500).with_history_minutes(30);
        let stored: HashMap<&str, u32> = encode(&s).into_iter().collect();
        let back = decode(|name| stored.get(name).copied());
        assert_eq!(back, s);
    }

    #[test]
    fn nothing_stored_is_the_defaults() {
        assert_eq!(decode(|_| None), Settings::default());
        assert!(
            !encode(&Settings::default())
                .iter()
                .any(|(name, _)| *name == ANIMATE_ROWS),
            "a setting that follows the system is not stored"
        );
    }
}
