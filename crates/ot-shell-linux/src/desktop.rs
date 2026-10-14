//! What the desktop does for an app, through its standard commands and D-Bus
//! interfaces: open a URL or a folder (`xdg-open`), reveal a file or show its
//! properties in the file manager (`org.freedesktop.FileManager1`), and start
//! programs. None of it needs the toolkit, so both plugins share it.

use std::fmt::Write as _;
use std::process::{Command, Stdio};

/// Start a program and forget it: a thread reaps it when it ends, so it never
/// lingers as a zombie, and its output goes nowhere.
fn spawn_detached(mut cmd: Command) -> Result<(), String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let _ = std::thread::Builder::new()
        .name("ot-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// Open `target` (a URL, a file or a folder) in the user's default program.
///
/// # Errors
/// `xdg-open` could not be started.
pub fn open(target: &str) -> Result<(), String> {
    let mut cmd = Command::new("xdg-open");
    cmd.arg(target);
    spawn_detached(cmd).map_err(|e| format!("xdg-open: {e}"))
}

/// A `file://` URI for a path, percent-encoding what a URI cannot hold.
fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            uri.push(char::from(b));
        } else {
            let _ = write!(uri, "%{b:02X}");
        }
    }
    uri
}

/// Call a `org.freedesktop.FileManager1` method on one file.
fn file_manager(method: &str, path: &str) -> Result<(), String> {
    let status = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
        ])
        .arg(format!("org.freedesktop.FileManager1.{method}"))
        .arg(format!("array:string:{}", file_uri(path)))
        .arg("string:")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("dbus-send: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the file manager did not answer {method}"))
    }
}

/// The file manager with `path` selected; its folder, if the file manager has
/// no D-Bus interface.
///
/// # Errors
/// Neither the file manager nor `xdg-open` could be reached.
pub fn reveal(path: &str) -> Result<(), String> {
    file_manager("ShowItems", path).or_else(|_| {
        let dir = std::path::Path::new(path)
            .parent()
            .map_or_else(|| path.to_owned(), |d| d.to_string_lossy().into_owned());
        open(&dir)
    })
}

/// The file manager's properties for `path`.
///
/// # Errors
/// No file manager answered on D-Bus.
pub fn properties(path: &str) -> Result<(), String> {
    file_manager("ShowItemProperties", path)
}

/// Split a command line into words: whitespace separates them, and single or
/// double quotes or a backslash keep spaces in one. What a shell does, without
/// expanding anything.
#[must_use]
pub fn split(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                    in_word = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Start `command_line` as a new process, in `directory` when given, with this
/// process's rights; no shell is involved. For Restart and uninstallers.
///
/// # Errors
/// The command line is empty, or the program could not be started.
pub fn launch(command_line: &str, directory: Option<&str>) -> Result<(), String> {
    let words = split(command_line);
    let Some((program, args)) = words.split_first() else {
        return Err("the command line is empty".to_owned());
    };
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = directory {
        cmd.current_dir(dir);
    }
    spawn_detached(cmd)
}

/// The Run dialog's command: through `sh -c`, since what a user types into a
/// run box is a shell command (`firefox &`, `FOO=1 app`). As administrator,
/// through `pkexec`, which asks for the password the desktop's way.
///
/// # Errors
/// `sh` or `pkexec` could not be started.
pub fn run_command(command: &str, elevated: bool) -> Result<(), String> {
    let mut cmd = if elevated {
        let mut c = Command::new("pkexec");
        c.args(["sh", "-c", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    if let Some(home) = std::env::var_os("HOME") {
        cmd.current_dir(home);
    }
    spawn_detached(cmd)
}

/// Whether the desktop prefers a dark theme: the XDG portal's
/// `org.freedesktop.appearance` `color-scheme` (1 dark, 2 light, 0 no
/// preference), which GNOME, KDE and others publish; else a `GTK_THEME` ending
/// in `:dark`.
#[must_use]
pub fn prefers_dark() -> bool {
    let portal = Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply=literal",
            "--reply-timeout=500",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings.ReadOne",
            "string:org.freedesktop.appearance",
            "string:color-scheme",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    if let Ok(out) = portal {
        if out.status.success() {
            if let Some(scheme) = portal_scheme(&String::from_utf8_lossy(&out.stdout)) {
                return scheme == 1;
            }
        }
    }
    std::env::var("GTK_THEME").is_ok_and(|t| t.to_ascii_lowercase().ends_with(":dark"))
}

/// The number in `dbus-send`'s literal reply to `ReadOne` (`variant uint32 1`,
/// or nested in another variant on older portals).
fn portal_scheme(reply: &str) -> Option<u32> {
    reply
        .split_whitespace()
        .skip_while(|w| *w != "uint32")
        .nth(1)?
        .parse()
        .ok()
}

/// Starting programs, for `ot_shell_core`'s Restart and uninstallers.
#[derive(Debug)]
pub struct LinuxPlatform;

impl ot_shell_core::Platform for LinuxPlatform {
    fn launch(&self, command_line: &str, directory: Option<&str>) -> Result<(), String> {
        launch(command_line, directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_like_a_shell() {
        assert_eq!(split("ls -l  /tmp"), ["ls", "-l", "/tmp"]);
        assert_eq!(
            split(r#"app "two words" 'and more' back\ slash"#),
            ["app", "two words", "and more", "back slash"]
        );
        assert_eq!(split(r#"say "a \"quote\"""#), ["say", r#"a "quote""#]);
        assert_eq!(split(r#"empty "" arg"#), ["empty", "", "arg"]);
        assert_eq!(split("   "), Vec::<String>::new());
    }

    #[test]
    fn the_portal_reply_reads_either_way() {
        assert_eq!(portal_scheme("   variant       uint32 1\n"), Some(1));
        assert_eq!(portal_scheme("variant variant uint32 2"), Some(2));
        assert_eq!(portal_scheme("nothing here"), None);
    }

    #[test]
    fn file_uris_escape_what_they_must() {
        assert_eq!(
            file_uri("/home/me/a b#.txt"),
            "file:///home/me/a%20b%23.txt"
        );
    }
}
