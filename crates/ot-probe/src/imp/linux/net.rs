//! Network adapters, from `/proc/net/dev`, `/sys/class/net` and `getifaddrs`.
//!
//! An adapter is listed while it is up, as Windows lists connected adapters. The
//! loopback is not. Facts are re-read when the adapter's addresses or link change,
//! and shared by pointer otherwise.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use ot_model::device::{AdapterInfo, AdapterSample, LinkKind};
use ot_model::units::Bytes;

use super::fs;

/// `ARPHRD_LOOPBACK`, a loopback device's `type`.
const ARPHRD_LOOPBACK: u64 = 772;

/// Received and sent bytes for each interface in `/proc/net/dev`.
fn parse_dev(text: &str) -> Vec<(&str, u64, u64)> {
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let mut f = rest.split_ascii_whitespace();
            let rx = f.next()?.parse().ok()?;
            let tx = f.nth(7)?.parse().ok()?;
            Some((name.trim(), rx, tx))
        })
        .collect()
}

/// Every interface's addresses, IPv4 first, in the order the system lists them.
fn addresses() -> HashMap<String, Vec<IpAddr>> {
    let mut out: HashMap<String, Vec<IpAddr>> = HashMap::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list freed below.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return out;
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned, not yet freed.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
            continue;
        }
        // SAFETY: a NUL-terminated name, and an address whose family says which
        // sockaddr it is.
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
        let ip = match family {
            libc::AF_INET => {
                let sa = unsafe { &*ifa.ifa_addr.cast::<libc::sockaddr_in>() };
                IpAddr::from(u32::from_be(sa.sin_addr.s_addr).to_be_bytes())
            }
            libc::AF_INET6 => {
                let sa = unsafe { &*ifa.ifa_addr.cast::<libc::sockaddr_in6>() };
                IpAddr::from(sa.sin6_addr.s6_addr)
            }
            _ => continue,
        };
        out.entry(name).or_default().push(ip);
    }
    // SAFETY: the list getifaddrs returned, freed once.
    unsafe { libc::freeifaddrs(head) };
    for v in out.values_mut() {
        // Stable: IPv4 first, each family in the system's order.
        v.sort_by_key(IpAddr::is_ipv6);
    }
    out
}

/// What kind of link an interface is.
fn kind_of(dir: &str, hardware: bool) -> LinkKind {
    let devtype = fs::read_trimmed(format!("{dir}/uevent"))
        .and_then(|u| {
            u.lines()
                .find_map(|l| l.strip_prefix("DEVTYPE=").map(str::to_owned))
        })
        .unwrap_or_default();
    if devtype == "wlan" || std::path::Path::new(&format!("{dir}/wireless")).exists() {
        LinkKind::WiFi
    } else if devtype == "wwan" {
        LinkKind::Cellular
    } else if !hardware {
        LinkKind::Virtual
    } else if fs::read_u64(format!("{dir}/type")) == Some(1) {
        LinkKind::Ethernet
    } else {
        LinkKind::Other
    }
}

/// What the adapter is: its driver for hardware, its kind for a software link.
fn describe(dir: &str, hardware: bool) -> String {
    if hardware {
        if let Ok(driver) = std::fs::read_link(format!("{dir}/device/driver")) {
            if let Some(name) = driver.file_name() {
                return name.to_string_lossy().into_owned();
            }
        }
    }
    fs::read_trimmed(format!("{dir}/uevent"))
        .and_then(|u| {
            u.lines()
                .find_map(|l| l.strip_prefix("DEVTYPE=").map(str::to_owned))
        })
        .unwrap_or_else(|| "virtual".to_owned())
}

/// `00:11:22:33:44:55` as the model writes it, `00-11-22-33-44-55`. The all-zero
/// address of a tunnel is no address.
fn mac(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.bytes().any(|b| b != b'0' && b != b':'))
        .then(|| s.replace(':', "-").to_uppercase())
}

#[derive(Debug)]
struct Adapter {
    info: Arc<AdapterInfo>,
    rx: u64,
    tx: u64,
}

#[derive(Debug, Default)]
pub(super) struct NetProbe {
    adapters: HashMap<String, Adapter>,
    last_pass: Option<Instant>,
}

impl NetProbe {
    pub(super) fn sample(&mut self, out: &mut Vec<AdapterSample>) {
        let now = Instant::now();
        let secs = self
            .last_pass
            .map_or(0.0, |p| now.duration_since(p).as_secs_f64());
        self.last_pass = Some(now);
        let Ok(text) = std::fs::read_to_string("/proc/net/dev") else {
            return;
        };
        let addrs = addresses();
        let mut present = Vec::new();
        let mut ids: HashMap<String, u64> = HashMap::new();
        for (name, rx, tx) in parse_dev(&text) {
            let dir = format!("/sys/class/net/{name}");
            if fs::read_u64(format!("{dir}/type")) == Some(ARPHRD_LOOPBACK) {
                continue;
            }
            // Tunnels report "unknown" while carrying traffic; trust the carrier.
            let up = match fs::read_trimmed(format!("{dir}/operstate")).as_deref() {
                Some("up") => true,
                Some("unknown") => fs::read_u64(format!("{dir}/carrier")) == Some(1),
                _ => false,
            };
            if !up {
                continue;
            }
            let id = fs::read_u64(format!("{dir}/ifindex")).unwrap_or(0);
            ids.insert(name.to_owned(), id);
            present.push(name.to_owned());
            let hardware = std::path::Path::new(&format!("{dir}/device")).exists();
            let info = AdapterInfo {
                id,
                name: name.to_owned(),
                adapter: describe(&dir, hardware),
                kind: kind_of(&dir, hardware),
                hardware,
                addresses: addrs.get(name).cloned().unwrap_or_default(),
                dns_suffix: None,
                mac: fs::read_trimmed(format!("{dir}/address")).and_then(|m| mac(&m)),
                parent: None,
            };
            let link_bps = fs::read_i64(format!("{dir}/speed"))
                .filter(|&mbps| mbps > 0)
                .map(|mbps| mbps.cast_unsigned() * 1_000_000);
            let (rx_per_sec, tx_per_sec, info) = self.update(info, rx, tx, secs);
            out.push(AdapterSample {
                info,
                rx_per_sec,
                tx_per_sec,
                link_bps,
            });
        }
        self.adapters.retain(|name, _| present.contains(name));
        self.settle_parents(out, &ids);
        // Physical adapters first, then by index.
        out.sort_by_key(|a| (!a.info.hardware, a.info.id));
    }

    /// Record this pass's counters for an adapter and return its rates, and its
    /// facts, shared with the previous pass when unchanged.
    fn update(
        &mut self,
        info: AdapterInfo,
        rx: u64,
        tx: u64,
        secs: f64,
    ) -> (Bytes, Bytes, Arc<AdapterInfo>) {
        let Some(a) = self.adapters.get_mut(&info.name) else {
            let info = Arc::new(info);
            self.adapters.insert(
                info.name.clone(),
                Adapter {
                    info: Arc::clone(&info),
                    rx,
                    tx,
                },
            );
            return (Bytes::ZERO, Bytes::ZERO, info);
        };
        let rate = |now: u64, prev: u64| {
            if secs > 0.0 {
                Bytes((now.saturating_sub(prev) as f64 / secs) as u64)
            } else {
                Bytes::ZERO
            }
        };
        let rates = (rate(rx, a.rx), rate(tx, a.tx));
        (a.rx, a.tx) = (rx, tx);
        // The parent is settled after the pass, once every adapter is known.
        let info = AdapterInfo {
            parent: a.info.parent,
            ..info
        };
        if *a.info != info {
            a.info = Arc::new(info);
        }
        (rates.0, rates.1, Arc::clone(&a.info))
    }

    /// A VLAN, or a bridge with a single port, carries its traffic over the one
    /// link under it: that link is its parent.
    fn settle_parents(&mut self, out: &mut [AdapterSample], ids: &HashMap<String, u64>) {
        for a in out.iter_mut() {
            let dir = format!("/sys/class/net/{}", a.info.name);
            let lowers: Vec<u64> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name();
                    let lower = n.to_str()?.strip_prefix("lower_")?;
                    ids.get(lower).copied()
                })
                .collect();
            let parent = (lowers.len() == 1).then(|| lowers[0]);
            if a.info.parent != parent {
                let info = Arc::new(AdapterInfo {
                    parent,
                    ..(*a.info).clone()
                });
                if let Some(t) = self.adapters.get_mut(&info.name) {
                    t.info = Arc::clone(&info);
                }
                a.info = info;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_counters_parse() {
        let text = "Inter-|   Receive                            |  Transmit\n \
             face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets\n\
                lo:  1000      10    0    0    0     0          0         0     1000      10    0 0 0 0 0 0\n\
              eth0: 52000     400    0    0    0     0          0         0    31000     300    0 0 0 0 0 0\n";
        let d = parse_dev(text);
        assert_eq!(d, [("lo", 1000, 1000), ("eth0", 52000, 31000)]);
    }

    #[test]
    fn macs_use_dashes() {
        assert_eq!(
            mac("00:15:5d:aa:bb:cc").as_deref(),
            Some("00-15-5D-AA-BB-CC")
        );
        assert_eq!(mac("00:00:00:00:00:00"), None);
        assert_eq!(mac(""), None);
    }

    #[test]
    fn a_pass_skips_the_loopback() {
        let mut p = NetProbe::default();
        let mut out = Vec::new();
        p.sample(&mut out);
        assert!(out.iter().all(|a| a.info.name != "lo"));
    }
}
