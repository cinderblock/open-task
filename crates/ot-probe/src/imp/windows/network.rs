//! Network adapters: which are connected, and their traffic.
//!
//! Two speeds. **Discovery**, every few seconds: `GetIfTable2Ex` without statistics
//! lists every interface with the connection name users know (`Wi-Fi`), the
//! adapter's description and its type, and `GetAdaptersAddresses` gives each its
//! unicast addresses, DNS suffix and MAC address; together they decide which
//! interfaces are listed, and the addresses are what the Performance page shows.
//! **Sampling**, every pass: `GetIfEntry2` for each listed interface only, for its
//! 64-bit octet counters, link speed and state. Rates are differences of the
//! counters over the wall time between passes.
//!
//! The split is for cost. On a Hyper-V host `GetIfTable2` with statistics walks
//! sixty-odd interfaces (every adapter's filter-driver shadows, the switch's
//! internals) and took 3.5 ms a pass here; reading the dozen listed ones took
//! 0.7 ms, and discovery without statistics 0.6 ms every 5 s.
//!
//! Listed: interfaces that are up and not a filter driver's shadow (the
//! `-WFP Native MAC Layer LightWeight Filter-0000` duplicates every adapter carries),
//! of an Ethernet, Wi-Fi, cellular, PPP or software-link type, and then either
//! physical, or carrying an IP address. The address test is what separates a
//! connection from plumbing: Hyper-V's `vEthernet` ports and a VPN such as
//! Tailscale have addresses; WAN miniports and the virtual switch's own internal
//! adapters look the same in every other field and have none. A physical adapter
//! is kept even without one, because an external virtual switch takes over its IP
//! binding while all the traffic still crosses it.
//!
//! Discovery also reads the interface stack (`GetIfStackTable`) to link each
//! listed interface to the nearest listed one below it: a `vEthernet` port of an
//! external virtual switch sits, through filter shadows and the switch's own
//! adapter, on the network card the switch is bound to, and its traffic is part of
//! that card's ([`AdapterInfo::parent`]). A port of an internal switch has nothing
//! listed below it.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ot_model::device::{AdapterInfo, AdapterSample, LinkKind};
use ot_model::Bytes;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetAdaptersAddresses, GetIfEntry2, GetIfStackTable, GetIfTable2Ex,
    MibIfTableNormalWithoutStatistics, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IF_TYPE_PPP,
    IF_TYPE_PROP_VIRTUAL, IF_TYPE_WWANPP, IF_TYPE_WWANPP2, IP_ADAPTER_ADDRESSES_LH,
    MIB_IFSTACK_ROW, MIB_IFSTACK_TABLE, MIB_IF_ROW2, MIB_IF_TABLE2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{
    IpDadStatePreferred, AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS,
};

use super::AlignedBuf;

/// `MIB_IF_ROW2.InterfaceAndOperStatusFlags` bits, from `netioapi.h`.
const FLAG_HARDWARE: u8 = 1 << 0;
const FLAG_FILTER: u8 = 1 << 1;
/// How often the set of listed interfaces is worked out again. A connection that
/// comes up (a VPN, a cable) appears within this long.
const DISCOVER_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
struct Octets {
    rx: u64,
    tx: u64,
}

/// What `GetAdaptersAddresses` says about one interface.
#[derive(Debug, Default, PartialEq, Eq)]
struct Facts {
    /// IPv4 first, then routable IPv6, then link-local.
    addresses: Vec<IpAddr>,
    dns_suffix: Option<String>,
    /// `AA-BB-CC-DD-EE-FF`, as Windows writes it.
    mac: Option<String>,
}

/// An interface discovery decided to list, before its facts are attached.
#[derive(Debug)]
struct Found {
    id: u64,
    /// The interface index, which the interface stack is written in.
    index: u32,
    name: String,
    adapter: String,
    kind: LinkKind,
    hardware: bool,
}

#[derive(Debug, Default)]
pub(super) struct NetProbe {
    /// Listed interfaces, in display order: physical first, then by name.
    listed: Vec<Arc<AdapterInfo>>,
    discovered_at: Option<Instant>,
    prev: HashMap<u64, Octets>,
    last: Option<Instant>,
    /// Where `GetAdaptersAddresses` writes its list, kept between discoveries.
    addr_buf: AlignedBuf,
}

impl NetProbe {
    /// Refill `out` with every listed adapter. Rates are zero for an adapter's
    /// first pass, when there is nothing to difference against.
    pub fn sample(&mut self, out: &mut Vec<AdapterSample>) {
        out.clear();
        let now = Instant::now();
        if self
            .discovered_at
            .is_none_or(|t| now.duration_since(t) >= DISCOVER_EVERY)
        {
            self.discover(now);
        }
        let secs = self.last.map(|t| now.duration_since(t).as_secs_f64());
        self.last = Some(now);

        let mut vanished = false;
        for info in &self.listed {
            let mut row = MIB_IF_ROW2::default();
            row.InterfaceLuid.Value = info.id;
            // SAFETY: `row` is a valid in-out struct with its LUID set.
            if unsafe { GetIfEntry2(&raw mut row) }.is_err() || row.OperStatus != IfOperStatusUp {
                // Removed or went down: stop listing it now, find out at the next
                // discovery whether it is gone for good.
                vanished = true;
                continue;
            }
            let now_octets = Octets {
                rx: row.InOctets,
                tx: row.OutOctets,
            };
            let rate = |now: u64, then: u64| -> Bytes {
                match secs {
                    Some(s) if s > 0.0 => Bytes((now.saturating_sub(then) as f64 / s) as u64),
                    _ => Bytes::ZERO,
                }
            };
            let (rx, tx) = match self.prev.insert(info.id, now_octets) {
                Some(p) => (rate(now_octets.rx, p.rx), rate(now_octets.tx, p.tx)),
                None => (Bytes::ZERO, Bytes::ZERO),
            };
            let link = row.TransmitLinkSpeed.max(row.ReceiveLinkSpeed);
            out.push(AdapterSample {
                info: Arc::clone(info),
                rx_per_sec: rx,
                tx_per_sec: tx,
                // Unknown speeds are reported as all ones.
                link_bps: (link > 0 && link != u64::MAX).then_some(link),
            });
        }
        if vanished {
            self.discovered_at = None;
        }
    }

    /// Work out which interfaces to list.
    fn discover(&mut self, now: Instant) {
        self.discovered_at = Some(now);
        let mut facts = adapter_facts(&mut self.addr_buf);

        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: `table` is a valid out-pointer; freed with FreeMibTable below.
        let ok =
            unsafe { GetIfTable2Ex(MibIfTableNormalWithoutStatistics, &raw mut table) }.is_ok();
        if !ok || table.is_null() {
            return;
        }
        // SAFETY: the call succeeded: `NumEntries` rows follow the header.
        let rows = unsafe {
            std::slice::from_raw_parts(
                (&raw const (*table).Table).cast::<MIB_IF_ROW2>(),
                (*table).NumEntries as usize,
            )
        };
        // Which interfaces to list, before their facts are shared out: the parent
        // links need the whole set.
        let mut found = Vec::new();
        for row in rows {
            let Some(kind) = kind_of(row) else {
                continue;
            };
            // SAFETY: NET_LUID_LH is a union over one u64.
            let id = unsafe { row.InterfaceLuid.Value };
            let hardware = row.InterfaceAndOperStatusFlags._bitfield & FLAG_HARDWARE != 0;
            let addressed = facts.get(&id).is_some_and(|f| !f.addresses.is_empty());
            if !hardware && !addressed {
                continue;
            }
            found.push(Found {
                id,
                index: row.InterfaceIndex,
                name: wide(&row.Alias),
                adapter: wide(&row.Description),
                kind,
                hardware,
            });
        }
        // SAFETY: allocated by GetIfTable2Ex, freed exactly once, not used after.
        unsafe { FreeMibTable(table.cast()) };

        let listed_at: HashMap<u32, u64> = found.iter().map(|f| (f.index, f.id)).collect();
        let below = stack_below();
        let previous = std::mem::take(&mut self.listed);
        for f in found {
            let parent = parent_of(f.index, &below, &listed_at);
            let Found {
                id,
                name,
                adapter,
                kind,
                hardware,
                ..
            } = f;
            let f = facts.remove(&id).unwrap_or_default();
            // Keep the same Arc when nothing changed, so samples keep pointing at one
            // allocation per adapter.
            let info = previous
                .iter()
                .find(|i| {
                    i.id == id
                        && i.name == name
                        && i.adapter == adapter
                        && i.addresses == f.addresses
                        && i.dns_suffix == f.dns_suffix
                        && i.mac == f.mac
                        && i.parent == parent
                })
                .cloned()
                .unwrap_or_else(|| {
                    Arc::new(AdapterInfo {
                        id,
                        name,
                        adapter,
                        kind,
                        hardware,
                        addresses: f.addresses,
                        dns_suffix: f.dns_suffix,
                        mac: f.mac,
                        parent,
                    })
                });
            self.listed.push(info);
        }

        self.listed.sort_by(|a, b| {
            b.hardware
                .cmp(&a.hardware)
                .then_with(|| natural_cmp(&a.name, &b.name))
        });
        let listed = &self.listed;
        self.prev.retain(|id, _| listed.iter().any(|i| i.id == *id));
    }
}

/// Every adapter's addresses, DNS suffix and MAC, by interface LUID. The list is
/// written into `buf`, grown once if the first size was too small (the API reports
/// what it needs). An adapter with no preferred unicast address has an empty list.
fn adapter_facts(buf: &mut AlignedBuf) -> HashMap<u64, Facts> {
    let mut facts = HashMap::new();
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    if buf.len_bytes() < 16 * 1024 {
        buf.resize_bytes(16 * 1024);
    }
    let family = u32::from(AF_UNSPEC.0);
    let mut size = u32::try_from(buf.len_bytes()).unwrap_or(u32::MAX);
    // SAFETY: the buffer holds `size` bytes and is 8-byte aligned; `size` is a
    // valid out-pointer.
    let mut status = unsafe {
        GetAdaptersAddresses(
            family,
            flags,
            None,
            Some(buf.as_mut_ptr().cast()),
            &raw mut size,
        )
    };
    if status == ERROR_BUFFER_OVERFLOW.0 {
        // Room for a few adapters that appeared since the size was asked for.
        buf.resize_bytes(size as usize + 4096);
        size = u32::try_from(buf.len_bytes()).unwrap_or(u32::MAX);
        // SAFETY: as above, with the larger buffer.
        status = unsafe {
            GetAdaptersAddresses(
                family,
                flags,
                None,
                Some(buf.as_mut_ptr().cast()),
                &raw mut size,
            )
        };
    }
    if status != 0 {
        tracing::debug!(code = status, "GetAdaptersAddresses failed");
        return facts;
    }
    let mut node: *const IP_ADAPTER_ADDRESSES_LH = buf.as_ptr().cast();
    while !node.is_null() {
        // SAFETY: the call filled `buf` with a linked list whose nodes all lie in
        // it; `node` came from that list.
        let adapter = unsafe { &*node };
        // SAFETY: NET_LUID_LH is a union over one u64.
        let id = unsafe { adapter.Luid.Value };
        let mut f = Facts::default();
        let mut unicast = adapter.FirstUnicastAddress.cast_const();
        while !unicast.is_null() {
            // SAFETY: as for `node`.
            let entry = unsafe { &*unicast };
            if entry.DadState == IpDadStatePreferred {
                if let Some(ip) = ip_of(&entry.Address) {
                    f.addresses.push(ip);
                }
            }
            unicast = entry.Next;
        }
        f.addresses.sort_by_key(|ip| match ip {
            IpAddr::V4(_) => 0,
            IpAddr::V6(v6) if v6.is_unicast_link_local() => 2,
            IpAddr::V6(_) => 1,
        });
        if !adapter.DnsSuffix.is_null() {
            // SAFETY: a NUL-terminated string in `buf`.
            let suffix = unsafe { adapter.DnsSuffix.to_string() }.unwrap_or_default();
            if !suffix.is_empty() {
                f.dns_suffix = Some(suffix);
            }
        }
        let mac_len = (adapter.PhysicalAddressLength as usize).min(adapter.PhysicalAddress.len());
        if mac_len > 0 {
            let mut mac = String::with_capacity(mac_len * 3);
            for (i, byte) in adapter.PhysicalAddress[..mac_len].iter().enumerate() {
                if i > 0 {
                    mac.push('-');
                }
                let _ = write!(mac, "{byte:02X}");
            }
            f.mac = Some(mac);
        }
        facts.insert(id, f);
        node = adapter.Next;
    }
    facts
}

/// The IP address in a socket address, if it is one of the two IP families.
fn ip_of(sa: &SOCKET_ADDRESS) -> Option<IpAddr> {
    if sa.lpSockaddr.is_null() {
        return None;
    }
    let len = usize::try_from(sa.iSockaddrLength).unwrap_or(0);
    // SAFETY: `lpSockaddr` points at `iSockaddrLength` bytes of a sockaddr, whose
    // family comes first; the reads below are guarded by that length, and are
    // unaligned because the API promises only a sockaddr's 2-byte alignment.
    let family = unsafe { (*sa.lpSockaddr).sa_family };
    if family == AF_INET && len >= size_of::<SOCKADDR_IN>() {
        // SAFETY: the length check above.
        let v4 = unsafe { sa.lpSockaddr.cast::<SOCKADDR_IN>().read_unaligned() };
        // `S_addr` is in network byte order: its bytes are the octets in order.
        // SAFETY: IN_ADDR is a union whose every view covers the same four bytes.
        let bytes = unsafe { v4.sin_addr.S_un.S_addr }.to_ne_bytes();
        return Some(IpAddr::V4(Ipv4Addr::from(bytes)));
    }
    if family == AF_INET6 && len >= size_of::<SOCKADDR_IN6>() {
        // SAFETY: the length check above.
        let v6 = unsafe { sa.lpSockaddr.cast::<SOCKADDR_IN6>().read_unaligned() };
        // SAFETY: IN6_ADDR is a union whose every view covers the same 16 bytes.
        let bytes = unsafe { v6.sin6_addr.u.Byte };
        return Some(IpAddr::V6(Ipv6Addr::from(bytes)));
    }
    None
}

/// The interface stack: for each interface index, the interfaces directly below it.
/// Empty if the system will not say.
fn stack_below() -> HashMap<u32, Vec<u32>> {
    let mut below: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut table: *mut MIB_IFSTACK_TABLE = std::ptr::null_mut();
    // SAFETY: `table` is a valid out-pointer; freed with FreeMibTable below.
    if unsafe { GetIfStackTable(&raw mut table) } != NO_ERROR || table.is_null() {
        return below;
    }
    // SAFETY: the call succeeded: `NumEntries` rows follow the header.
    let rows = unsafe {
        std::slice::from_raw_parts(
            (&raw const (*table).Table).cast::<MIB_IFSTACK_ROW>(),
            (*table).NumEntries as usize,
        )
    };
    for row in rows {
        // Index 0 stands for "nothing": the top or the bottom of a stack.
        if row.HigherLayerInterfaceIndex != 0 && row.LowerLayerInterfaceIndex != 0 {
            below
                .entry(row.HigherLayerInterfaceIndex)
                .or_default()
                .push(row.LowerLayerInterfaceIndex);
        }
    }
    // SAFETY: allocated by GetIfStackTable, freed exactly once, not used after.
    unsafe { FreeMibTable(table.cast()) };
    below
}

/// The nearest listed interface below `index` in the stack, by id: what a Hyper-V
/// port's traffic crosses on its way out. The walk passes through what is not
/// listed (filter drivers' shadows, the virtual switch's own adapter), and
/// `listed` maps listed interfaces' indexes to their ids.
fn parent_of(
    index: u32,
    below: &HashMap<u32, Vec<u32>>,
    listed: &HashMap<u32, u64>,
) -> Option<u64> {
    // A stack is a handful of layers; the cap only guards against a cycle.
    let mut todo: Vec<(u32, u8)> = below.get(&index)?.iter().map(|&i| (i, 1)).collect();
    while let Some((at, depth)) = todo.pop() {
        if at == index {
            continue;
        }
        if let Some(&id) = listed.get(&at) {
            return Some(id);
        }
        if depth < 16 {
            if let Some(next) = below.get(&at) {
                todo.extend(next.iter().map(|&i| (i, depth + 1)));
            }
        }
    }
    None
}

/// Names in the order people read them: runs of digits by their value, so
/// `VLAN-3` comes before `VLAN-11`.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        let (Some(ca), Some(cb)) = (a.chars().next(), b.chars().next()) else {
            return a.len().cmp(&b.len());
        };
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let ea = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
            let eb = b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len());
            let (na, nb) = (
                a[..ea].trim_start_matches('0'),
                b[..eb].trim_start_matches('0'),
            );
            let order = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if order != Ordering::Equal {
                return order;
            }
            (a, b) = (&a[ea..], &b[eb..]);
        } else {
            let order = ca.cmp(&cb);
            if order != Ordering::Equal {
                return order;
            }
            (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
        }
    }
}

/// The link kind of an interface that might be listed, or `None` to skip it.
fn kind_of(row: &MIB_IF_ROW2) -> Option<LinkKind> {
    if row.OperStatus != IfOperStatusUp {
        return None;
    }
    if row.InterfaceAndOperStatusFlags._bitfield & FLAG_FILTER != 0 {
        return None;
    }
    match row.Type {
        IF_TYPE_ETHERNET_CSMACD => Some(LinkKind::Ethernet),
        IF_TYPE_IEEE80211 => Some(LinkKind::WiFi),
        IF_TYPE_WWANPP | IF_TYPE_WWANPP2 => Some(LinkKind::Cellular),
        IF_TYPE_PPP | IF_TYPE_PROP_VIRTUAL => Some(LinkKind::Virtual),
        _ => None,
    }
}

fn wide(units: &[u16]) -> String {
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_belongs_to_the_nearest_listed_interface_below_it() {
        // As on a Hyper-V host (higher -> lower): two ports of an external switch
        // over filter shadows (40, 51) and the switch's own adapter (36) to the
        // card (13); a port of an internal switch (56 -> 38 -> 29) over nothing
        // listed; and a cycle, which must not hang.
        let below: HashMap<u32, Vec<u32>> = [
            (3, vec![40]),
            (12, vec![40]),
            (40, vec![36]),
            (36, vec![51]),
            (51, vec![13]),
            (56, vec![38]),
            (38, vec![29]),
            (70, vec![71]),
            (71, vec![70]),
        ]
        .into_iter()
        .collect();
        let listed: HashMap<u32, u64> = [(3, 103), (12, 112), (13, 113), (56, 156), (70, 170)]
            .into_iter()
            .collect();
        assert_eq!(parent_of(3, &below, &listed), Some(113));
        assert_eq!(parent_of(12, &below, &listed), Some(113));
        assert_eq!(
            parent_of(13, &below, &listed),
            None,
            "the card is the bottom"
        );
        assert_eq!(parent_of(56, &below, &listed), None, "an internal switch");
        assert_eq!(
            parent_of(70, &below, &listed),
            None,
            "a cycle back to itself"
        );
    }

    #[test]
    fn names_sort_by_the_numbers_in_them() {
        let mut names = [
            "vEthernet (VLAN-11)",
            "vEthernet (VLAN-3)",
            "vEthernet (VLAN-FMS)",
            "vEthernet (VLAN-099)",
            "vEthernet (VLAN-5)",
            "Ethernet 2",
            "Ethernet",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "Ethernet",
                "Ethernet 2",
                "vEthernet (VLAN-3)",
                "vEthernet (VLAN-5)",
                "vEthernet (VLAN-11)",
                "vEthernet (VLAN-099)",
                "vEthernet (VLAN-FMS)",
            ]
        );
    }

    #[test]
    fn this_machine_lists_an_adapter_and_measures_it() {
        let mut probe = NetProbe::default();
        let mut out = Vec::new();
        probe.sample(&mut out);
        assert!(!out.is_empty(), "a connected machine has an adapter up");
        assert!(
            out.iter().all(|a| a.rx_per_sec == Bytes::ZERO),
            "no rate on first sight"
        );
        assert!(out.iter().all(|a| !a.info.name.is_empty()), "{out:?}");
        let first = Arc::clone(&out[0].info);
        std::thread::sleep(Duration::from_millis(300));
        probe.sample(&mut out);
        assert!(!out.is_empty(), "the adapter is still listed");
        assert!(
            Arc::ptr_eq(&first, &out[0].info),
            "facts are shared, not rebuilt"
        );
        // Physical adapters sort first.
        let mut seen_virtual = false;
        for a in &out {
            assert!(!(seen_virtual && a.info.hardware), "{out:?}");
            seen_virtual |= !a.info.hardware;
        }
        // A connected machine has an address somewhere, and a MAC on something (a
        // physical adapter bound to an external virtual switch shows neither; the
        // switch's adapter carries them).
        assert!(out.iter().any(|a| !a.info.addresses.is_empty()), "{out:?}");
        assert!(out.iter().any(|a| a.info.mac.is_some()), "{out:?}");
        // A port's parent is listed, and is not a port itself.
        for a in &out {
            if let Some(p) = a.parent_in(&out) {
                assert!(p.info.parent.is_none(), "{out:?}");
            } else {
                assert!(a.info.parent.is_none(), "an unlisted parent: {out:?}");
            }
        }
        for a in &out {
            let v4s = a
                .info
                .addresses
                .iter()
                .take_while(|ip| ip.is_ipv4())
                .count();
            assert!(
                a.info.addresses[v4s..].iter().all(IpAddr::is_ipv6),
                "IPv4 first: {:?}",
                a.info.addresses
            );
        }
    }
}
