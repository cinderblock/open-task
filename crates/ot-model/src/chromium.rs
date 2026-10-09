//! What a process of a Chromium-based program does, from its command line.
//!
//! Chrome, Edge, `WebView2` and every Electron app (Slack, Discord, VS Code, ...) run
//! as one main process and a crowd of children of the same name. The main process
//! starts each child with `--type=<role>`, and a utility child also with
//! `--utility-sub-type=<service>`, so `chrome.exe --type=gpu-process` is the GPU
//! process and `--type=utility --utility-sub-type=network.mojom.NetworkService` the
//! network service. The main process itself has no `--type`.
//!
//! Which site a renderer is serving is not on its command line, so all of a
//! browser's renderers share the one label.
//!
//! A packaged Electron app has an executable of its own (`slack.exe`), but one run
//! from source runs the generic `electron.exe` from its `node_modules`, so every
//! app in development has the same name. [`electron_app`] finds which app it is.

/// The role a Chromium child process was started for, for display (`GPU`,
/// `Renderer`, `Network`), or `None` for the main process and anything that is not
/// Chromium.
///
/// A `--type=` alone is not enough: other programs take that switch too. The role
/// must be one Chromium starts, or the line must carry the Mojo channel handle every
/// Chromium child but the crash handler is given.
#[must_use]
pub fn role(command_line: &str) -> Option<&str> {
    let mut kind = None;
    let mut sub_type = None;
    let (mut extension, mut mojo) = (false, false);
    for arg in command_line.split_whitespace() {
        let arg = arg.trim_matches('"');
        if let Some(v) = arg.strip_prefix("--type=") {
            kind = Some(v);
        } else if let Some(v) = arg.strip_prefix("--utility-sub-type=") {
            sub_type = Some(v);
        } else if arg == "--extension-process" {
            extension = true;
        } else if arg.starts_with("--mojo-platform-channel-handle=") {
            mojo = true;
        }
    }
    let label = match kind? {
        "renderer" if extension => "Extension",
        "renderer" => "Renderer",
        "gpu-process" => "GPU",
        "crashpad-handler" => "Crash handler",
        "ppapi" => "Plugin",
        "ppapi-broker" => "Plugin broker",
        "utility" => sub_type.map_or("Utility", service),
        other if mojo && !other.is_empty() => other,
        _ => return None,
    };
    Some(label)
}

/// A friendly name for a utility process's service, from its Mojo interface name
/// (`network.mojom.NetworkService`): the common ones by name, any other as the
/// interface less its `Service` suffix (`OnDeviceModel`).
fn service(sub_type: &str) -> &str {
    let interface = sub_type.rsplit('.').next().unwrap_or(sub_type);
    match interface {
        "NetworkService" => "Network",
        "StorageService" => "Storage",
        "AudioService" => "Audio",
        "VideoCaptureService" => "Video capture",
        "DataDecoderService" => "Data decoder",
        "ProxyResolverFactory" => "Proxy resolver",
        "UtilWin" => "Windows utility",
        "PrintCompositor" => "Print compositor",
        "MediaFoundationServiceBroker" => "Media Foundation",
        "OnDeviceModelService" => "On-device AI",
        "CdmServiceBroker" => "Content decryption",
        "Unzipper" => "Unzip",
        // Electron's `utilityProcess`: VS Code's extension host, a shell's pty host.
        "NodeService" => "Node",
        _ => match interface.strip_suffix("Service") {
            Some(s) if !s.is_empty() => s,
            _ => interface,
        },
    }
}

/// Which app a generic `electron.exe` is running, for a process of that name;
/// `None` for any other name, or when nothing names the app.
///
/// In order of trust:
/// - the folder holding the `node_modules` the executable runs from
///   (`...\t3code\node_modules\electron\dist\electron.exe` is `t3code`). It is the
///   same for the main process, every child, and the scripts the app runs with
///   Electron as Node, so all of an app's processes agree. A global install
///   (`npm\node_modules`) names no app and is passed over;
/// - the last folder of `--user-data-dir=`, which Electron names after the app;
/// - the last folder of `--app-path=` (a renderer's) that is not a build folder
///   such as `dist` or `out`.
#[must_use]
pub fn electron_app<'a>(
    name: &str,
    image_path: Option<&'a str>,
    command_line: Option<&'a str>,
) -> Option<&'a str> {
    if !(name.eq_ignore_ascii_case("electron.exe") || name.eq_ignore_ascii_case("electron")) {
        return None;
    }
    let project = image_path.and_then(|path| {
        let mut prev: Option<&str> = None;
        for part in path.split(['\\', '/']) {
            if part.eq_ignore_ascii_case("node_modules") {
                return prev.filter(|&p| !global_folder(p));
            }
            prev = Some(part);
        }
        None
    });
    let cl = command_line.unwrap_or("");
    project
        .or_else(|| switch(cl, "--user-data-dir=").and_then(|p| folders(p).next()))
        .or_else(|| switch(cl, "--app-path=").and_then(|p| folders(p).find(|f| !build_folder(f))))
}

/// A path's folder names, last first, without empty ones.
fn folders(path: &str) -> impl Iterator<Item = &str> {
    path.rsplit(['\\', '/']).filter(|f| !f.is_empty())
}

/// The value of `--name=` on a command line, quoted or not: `--name="C:\a b"`,
/// `"--name=C:\a b"` or `--name=C:\a`.
fn switch<'a>(command_line: &'a str, name: &str) -> Option<&'a str> {
    let at = command_line.find(name)?;
    let value = &command_line[at + name.len()..];
    let quoted = command_line[..at].ends_with('"');
    let (value, end) = match value.strip_prefix('"') {
        Some(v) => (v, v.find('"')),
        None if quoted => (value, value.find('"')),
        None => (value, value.find(char::is_whitespace)),
    };
    let value = &value[..end.unwrap_or(value.len())];
    (!value.is_empty()).then_some(value)
}

/// A folder `node_modules` sits in that is a package manager's, not an app's:
/// `npm`, `pnpm\global\5`, `.bun\install\global`.
fn global_folder(f: &str) -> bool {
    f.is_empty()
        || f.starts_with('.')
        || f.bytes().all(|b| b.is_ascii_digit())
        || ["npm", "pnpm", "yarn", "global", "install", "lib", "nodejs"]
            .iter()
            .any(|g| f.eq_ignore_ascii_case(g))
}

/// A folder a build writes into, which says nothing about the app.
fn build_folder(f: &str) -> bool {
    [
        "dist",
        "dist-electron",
        "out",
        "build",
        "app",
        "app.asar",
        "resources",
        "src",
        "lib",
        "main",
        "electron",
    ]
    .iter()
    .any(|b| f.eq_ignore_ascii_case(b))
}

#[cfg(test)]
mod tests {
    use super::{electron_app, role};

    #[test]
    fn an_electron_app_is_named_by_the_project_it_runs_from() {
        let exe = r"C:\Users\me\git\t3code\node_modules\.pnpm\electron@44.4.2\node_modules\electron\dist\electron.exe";
        // Every process of it: the main one, a child, a script run as Node.
        for cl in [
            r"electron.exe --user-data-dir=C:\Users\me\AppData\Roaming\t3code dist-electron/main.cjs",
            r#""electron.exe" --type=renderer --user-data-dir="C:\Users\me\AppData\Roaming\t3code-v2""#,
            r"electron.exe C:\Users\me\git\t3code\apps\server\dist\bin.mjs",
        ] {
            assert_eq!(
                electron_app("electron.exe", Some(exe), Some(cl)),
                Some("t3code")
            );
        }
        // Not the generic runtime: the name already says which app.
        assert_eq!(electron_app("slack.exe", Some(exe), None), None);
    }

    #[test]
    fn a_global_electron_falls_back_to_the_command_line() {
        let exe = r"C:\Users\me\AppData\Roaming\npm\node_modules\electron\dist\electron.exe";
        let udd = r#"electron.exe --type=gpu-process "--user-data-dir=C:\Users\Jo Smith\AppData\Roaming\My App""#;
        assert_eq!(
            electron_app("electron.exe", Some(exe), Some(udd)),
            Some("My App")
        );
        let app = r#"electron.exe --type=renderer --app-path="C:\src\notes\dist-electron""#;
        assert_eq!(
            electron_app("Electron.exe", Some(exe), Some(app)),
            Some("notes")
        );
        assert_eq!(
            electron_app("electron.exe", Some(exe), Some("electron.exe .")),
            None
        );
        assert_eq!(electron_app("electron.exe", None, None), None);
    }

    const MOJO: &str = "--field-trial-handle=1916,i,1 --mojo-platform-channel-handle=2012";

    #[test]
    fn each_kind_of_child_is_named() {
        let exe = r#""C:\Program Files\Google\Chrome\Application\chrome.exe""#;
        for (args, want) in [
            (format!("--type=renderer {MOJO} --renderer-client-id=7"), "Renderer"),
            (format!("--type=renderer --extension-process {MOJO}"), "Extension"),
            (format!("--type=gpu-process {MOJO}"), "GPU"),
            (
                format!("--type=utility --utility-sub-type=network.mojom.NetworkService {MOJO}"),
                "Network",
            ),
            (
                format!("--type=utility --utility-sub-type=video_capture.mojom.VideoCaptureService {MOJO}"),
                "Video capture",
            ),
            (
                format!("--type=utility --utility-sub-type=on_device_model.mojom.FutureThingService {MOJO}"),
                "FutureThing",
            ),
            (format!("--type=utility {MOJO}"), "Utility"),
            // No Mojo handle, but a role only Chromium starts.
            ("--type=crashpad-handler --monitor-self".to_owned(), "Crash handler"),
            // A role this list does not know, on a real Chromium child.
            (format!("--type=new-kind {MOJO}"), "new-kind"),
        ] {
            assert_eq!(role(&format!("{exe} {args}")), Some(want), "{args}");
        }
    }

    #[test]
    fn the_main_process_and_strangers_have_no_role() {
        let main = r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --profile-directory=Default"#;
        assert_eq!(role(main), None);
        assert_eq!(role(r"C:\tools\pack.exe --type=zip out.zip"), None);
        assert_eq!(role(r"C:\tools\pack.exe --type="), None);
        assert_eq!(role(""), None);
    }

    #[test]
    fn quoted_switches_are_read() {
        assert_eq!(
            role(r#"app.exe "--type=gpu-process" "--mojo-platform-channel-handle=1""#),
            Some("GPU")
        );
    }
}
