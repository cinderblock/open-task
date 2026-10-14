//! The user's settings, in `$XDG_CONFIG_HOME/open-task/settings` (else
//! `~/.config/open-task/settings`): one `Name=value` line each, under the names
//! the Windows build uses in the registry (`ot_shell_core::settings_store`). A
//! missing file or line means the default. Nothing here is fatal: a file that
//! cannot be read or written is logged and the app carries on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ot_shell_core::settings_store::{self, LAYOUT};
use ot_ui::{Settings, ViewLayout};

/// The settings file, for `ot_shell_core`.
#[derive(Debug)]
pub struct FilePrefs {
    path: Option<PathBuf>,
}

impl FilePrefs {
    /// The file in the user's configuration directory.
    #[must_use]
    pub fn standard() -> Self {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        Self {
            path: dir.map(|d| d.join("open-task").join("settings")),
        }
    }

    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn read(&self) -> BTreeMap<String, String> {
        let Some(path) = &self.path else {
            return BTreeMap::new();
        };
        match std::fs::read_to_string(path) {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "could not read the settings");
                BTreeMap::new()
            }
        }
    }

    /// Change some values and keep the rest, written whole through a temporary
    /// file so a crash never leaves half a file.
    fn update(&self, change: impl FnOnce(&mut BTreeMap<String, String>)) {
        let Some(path) = &self.path else {
            return;
        };
        let mut values = self.read();
        change(&mut values);
        if let Err(e) = write(path, &values) {
            tracing::warn!(path = %path.display(), error = %e, "could not save the settings");
        }
    }
}

impl ot_shell_core::Prefs for FilePrefs {
    fn load(&self) -> Settings {
        let values = self.read();
        settings_store::decode(|name| values.get(name)?.trim().parse().ok())
    }

    fn save(&self, settings: &Settings) {
        self.update(|values| {
            for (name, value) in settings_store::encode(settings) {
                values.insert(name.to_owned(), value.to_string());
            }
        });
    }

    fn load_layout(&self) -> Option<ViewLayout> {
        ViewLayout::decode(self.read().get(LAYOUT)?)
    }

    fn save_layout(&self, layout: &ViewLayout) {
        let encoded = layout.encode();
        self.update(|values| {
            values.insert(LAYOUT.to_owned(), encoded);
        });
    }
}

fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

fn write(path: &Path, values: &BTreeMap<String, String>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = String::from("# open-task settings. Written by the app; edits are kept.\n");
    for (k, v) in values {
        // A value never holds a line break; one that did would split the line.
        text.push_str(k);
        text.push('=');
        text.push_str(&v.replace(['\n', '\r'], " "));
        text.push('\n');
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_shell_core::Prefs as _;

    #[test]
    fn settings_and_layout_share_the_file() {
        let dir = std::env::temp_dir().join(format!("ot-prefs-{}", std::process::id()));
        let prefs = FilePrefs::at(dir.join("open-task").join("settings"));
        assert_eq!(prefs.load(), Settings::default());
        let s = Settings {
            always_on_top: true,
            ..Settings::default()
        };
        prefs.save(&s);
        let layout = ot_ui::App::new(ot_ui::Theme::dark()).view_layout();
        prefs.save_layout(&layout);
        assert_eq!(prefs.load(), s, "the layout did not overwrite the settings");
        assert_eq!(prefs.load_layout(), Some(layout));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hand_edits_parse() {
        let v = parse("# comment\n AlwaysOnTop = 1 \n\nbroken line\nLayout=a=b\n");
        assert_eq!(v.get("AlwaysOnTop").map(String::as_str), Some("1"));
        assert_eq!(v.get("Layout").map(String::as_str), Some("a=b"));
        assert_eq!(v.len(), 2);
    }
}
