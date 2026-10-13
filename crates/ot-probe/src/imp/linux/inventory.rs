//! Lists read on demand: open connections, facts about the machine, and the
//! programs that start at sign-in.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use ot_model::connection::{Connection, Protocol, TcpState};
use ot_model::startup::{StartupEntry, StartupLocation};
use ot_model::system::SystemFacts;

use super::fs;

/// The kernel's TCP state numbers (`include/net/tcp_states.h`).
fn tcp_state(code: u8) -> TcpState {
    match code {
        0x01 => TcpState::Established,
        0x02 => TcpState::SynSent,
        0x03 => TcpState::SynReceived,
        0x04 => TcpState::FinWait1,
        0x05 => TcpState::FinWait2,
        0x06 => TcpState::TimeWait,
        0x07 => TcpState::Closed,
        0x08 => TcpState::CloseWait,
        0x09 => TcpState::LastAck,
        0x0A => TcpState::Listen,
        0x0B => TcpState::Closing,
        _ => TcpState::Unknown,
    }
}

/// `0100007F:0035`: an address as the kernel prints it (each 32-bit word in the
/// machine's byte order, in hex) and a port (in hex).
fn parse_endpoint(s: &str) -> Option<SocketAddr> {
    let (addr, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let words: Vec<u32> = (0..addr.len() / 8)
        .map(|i| u32::from_str_radix(&addr[i * 8..i * 8 + 8], 16))
        .collect::<Result<_, _>>()
        .ok()?;
    let ip = match words.len() {
        1 => IpAddr::V4(Ipv4Addr::from(words[0].to_ne_bytes())),
        4 => {
            let mut bytes = [0u8; 16];
            for (chunk, word) in bytes.chunks_exact_mut(4).zip(&words) {
                chunk.copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// The rows of one `/proc/net/{tcp,tcp6,udp,udp6}` file, with each socket's
/// inode for finding its owner.
fn parse_table(text: &str, protocol: Protocol) -> Vec<(Connection, u64)> {
    let tcp = matches!(protocol, Protocol::Tcp | Protocol::Tcp6);
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_ascii_whitespace();
            let local = parse_endpoint(f.nth(1)?)?;
            let remote = parse_endpoint(f.next()?)?;
            let state = u8::from_str_radix(f.next()?, 16).ok()?;
            let inode = f.nth(5)?.parse().ok()?;
            let state = if tcp {
                tcp_state(state)
            } else {
                TcpState::Unknown
            };
            let remote = (tcp && state != TcpState::Listen).then_some(remote);
            Some((
                Connection {
                    protocol,
                    local,
                    remote,
                    state,
                    pid: 0,
                },
                inode,
            ))
        })
        .collect()
}

/// Which process holds each socket inode, from every readable `/proc/<pid>/fd`.
/// Another user's process cannot be read except as root; its sockets stay at
/// PID 0.
fn socket_owners() -> HashMap<u64, u32> {
    let mut owners = HashMap::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return owners;
    };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse().ok())
            {
                owners.entry(inode).or_insert(pid);
            }
        }
    }
    owners
}

pub(super) fn connections() -> Vec<Connection> {
    let mut rows = Vec::new();
    for (file, protocol) in [
        ("tcp", Protocol::Tcp),
        ("tcp6", Protocol::Tcp6),
        ("udp", Protocol::Udp),
        ("udp6", Protocol::Udp6),
    ] {
        if let Ok(text) = std::fs::read_to_string(format!("/proc/net/{file}")) {
            rows.extend(parse_table(&text, protocol));
        }
    }
    let owners = socket_owners();
    rows.into_iter()
        .map(|(mut c, inode)| {
            c.pid = owners.get(&inode).copied().unwrap_or(0);
            c
        })
        .collect()
}

/// `KEY=value` lines, with optional quotes, as `/etc/os-release` has them.
fn parse_os_release(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(v);
            Some((k.trim().to_owned(), v.to_owned()))
        })
        .collect()
}

/// What the firmware says, from `/sys/class/dmi/id`, without its placeholders.
fn dmi(field: &str) -> Option<String> {
    fs::read_trimmed(format!("/sys/class/dmi/id/{field}")).filter(|v| {
        !matches!(
            v.as_str(),
            "To Be Filled By O.E.M."
                | "To be filled by O.E.M."
                | "Not Specified"
                | "Default string"
                | "System Product Name"
                | "System manufacturer"
        )
    })
}

/// Secure Boot's state from its EFI variable: four bytes of attributes, then
/// the value.
fn secure_boot() -> Option<bool> {
    let v =
        std::fs::read("/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c")
            .ok()?;
    v.get(4).map(|&b| b == 1)
}

pub(super) fn system_facts() -> SystemFacts {
    let os = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .map(|t| parse_os_release(&t))
        .unwrap_or_default();
    let uefi = std::path::Path::new("/sys/firmware/efi").exists();
    let page_files = std::fs::read_to_string("/proc/swaps")
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_ascii_whitespace();
            let path = f.next()?;
            let kib: u64 = f.nth(1)?.parse().ok()?;
            Some(format!("{path} ({} MB)", kib / 1024))
        })
        .collect();
    SystemFacts {
        computer_name: fs::read_trimmed("/proc/sys/kernel/hostname"),
        os_name: os.get("NAME").cloned(),
        os_version: os.get("VERSION").or_else(|| os.get("VERSION_ID")).cloned(),
        // The kernel is the build that matters on Linux.
        os_build: fs::read_trimmed("/proc/sys/kernel/osrelease"),
        system_manufacturer: dmi("sys_vendor"),
        system_model: dmi("product_name"),
        board_manufacturer: dmi("board_vendor"),
        board_product: dmi("board_name"),
        bios_vendor: dmi("bios_vendor"),
        bios_version: dmi("bios_version"),
        bios_date: dmi("bios_date"),
        uefi: Some(uefi),
        secure_boot: if uefi { secure_boot() } else { Some(false) },
        page_files,
        ..SystemFacts::default()
    }
}

/// One XDG autostart `.desktop` file's entry, when it is an application.
fn parse_desktop(text: &str) -> Option<(String, String, bool)> {
    let mut in_entry = false;
    let (mut name, mut exec) = (None, None);
    let mut enabled = true;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "Name" => name = Some(v.trim().to_owned()),
            "Exec" => exec = Some(v.trim().to_owned()),
            "Type" if v.trim() != "Application" => return None,
            "Hidden" if v.trim() == "true" => enabled = false,
            "X-GNOME-Autostart-enabled" if v.trim() == "false" => enabled = false,
            _ => {}
        }
    }
    Some((name?, exec?, enabled))
}

/// The user's autostart folder, `$XDG_CONFIG_HOME/autostart`.
fn user_autostart() -> Option<std::path::PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;
    Some(config.join("autostart"))
}

/// Programs XDG autostart runs at a graphical sign-in. A file in the user's
/// folder overrides the system's file of the same name, as the desktop does.
pub(super) fn startup_entries() -> Vec<StartupEntry> {
    let mut by_file: HashMap<String, StartupEntry> = HashMap::new();
    let dirs = [
        (
            Some(std::path::PathBuf::from("/etc/xdg/autostart")),
            StartupLocation::CommonFolder,
        ),
        (user_autostart(), StartupLocation::UserFolder),
    ];
    for (dir, location) in dirs {
        let Some(Ok(entries)) = dir.map(std::fs::read_dir) else {
            continue;
        };
        for e in entries.flatten() {
            let file = e.file_name().to_string_lossy().into_owned();
            if !file.ends_with(".desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let Some((name, command, enabled)) = parse_desktop(&text) else {
                continue;
            };
            let image_path = command
                .split_ascii_whitespace()
                .next()
                .filter(|p| p.starts_with('/'))
                .map(str::to_owned);
            by_file.insert(
                file,
                StartupEntry {
                    name,
                    location,
                    command,
                    enabled,
                    publisher: None,
                    image_path,
                },
            );
        }
    }
    let mut out: Vec<_> = by_file.into_values().collect();
    out.sort_by_cached_key(|e| e.name.to_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_decode() {
        assert_eq!(
            parse_endpoint("0100007F:0035"),
            Some("127.0.0.1:53".parse().unwrap())
        );
        assert_eq!(
            parse_endpoint("00000000000000000000000001000000:1F90"),
            Some("[::1]:8080".parse().unwrap())
        );
    }

    #[test]
    fn tcp_rows_parse() {
        let text = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:0035 00000000:0000 0A 00000000:00000000 00:00000000 00000000   101        0 20462 1 0000000000000000 100 0 0 10 5\n\
   1: 0F02000A:D2C4 22D8B85D:01BB 01 00000000:00000000 02:000A3B2B 00000000  1000        0 98765 2 0000000000000000 20 4 30 10 -1\n";
        let rows = parse_table(text, Protocol::Tcp);
        assert_eq!(rows.len(), 2);
        let (listen, inode) = &rows[0];
        assert_eq!(listen.state, TcpState::Listen);
        assert_eq!(listen.remote, None);
        assert_eq!(*inode, 20462);
        let (est, _) = &rows[1];
        assert_eq!(est.state, TcpState::Established);
        assert_eq!(est.local, "10.0.2.15:53956".parse().unwrap());
        assert_eq!(est.remote, Some("93.184.216.34:443".parse().unwrap()));
    }

    #[test]
    fn os_release_unquotes() {
        let os = parse_os_release("NAME=\"Ubuntu\"\nVERSION_ID='24.04'\nID=ubuntu\n");
        assert_eq!(os["NAME"], "Ubuntu");
        assert_eq!(os["VERSION_ID"], "24.04");
        assert_eq!(os["ID"], "ubuntu");
    }

    #[test]
    fn desktop_files_parse() {
        let (name, exec, enabled) = parse_desktop(
            "[Desktop Entry]\nType=Application\nName=Syncthing\nExec=/usr/bin/syncthing serve\n\
             X-GNOME-Autostart-enabled=false\n[Desktop Action New]\nName=Other\n",
        )
        .unwrap();
        assert_eq!(name, "Syncthing");
        assert_eq!(exec, "/usr/bin/syncthing serve");
        assert!(!enabled);
        assert_eq!(
            parse_desktop("[Desktop Entry]\nType=Link\nName=x\nExec=y\n"),
            None
        );
    }

    #[test]
    fn this_process_owns_its_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let c = connections();
        let mine = c
            .iter()
            .find(|c| c.state == TcpState::Listen && c.local.port() == port)
            .expect("the listener");
        assert_eq!(mine.pid, std::process::id());
    }

    #[test]
    fn facts_name_the_machine() {
        let f = system_facts();
        assert!(f.computer_name.is_some());
        assert!(f.os_build.is_some());
    }
}
