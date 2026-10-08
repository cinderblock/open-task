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

#[cfg(test)]
mod tests {
    use super::role;

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
