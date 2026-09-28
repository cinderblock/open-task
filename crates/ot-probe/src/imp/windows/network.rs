//! Network adapters: which are connected, and their traffic.
//!
//! Two speeds. **Discovery**, every few seconds: `GetIfTable2Ex` without statistics
//! lists every interface with the connection name users know (`Wi-Fi`), the
//! adapter's description and its type, and `GetUnicastIpAddressTable` says which
//! carry an address; together they decide which interfaces are listed.
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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ot_model::device::{AdapterInfo, AdapterSample, LinkKind};
use ot_model::Bytes;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfEntry2, GetIfTable2Ex, GetUnicastIpAddressTable,
    MibIfTableNormalWithoutStatistics, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, IF_TYPE_PPP,
    IF_TYPE_PROP_VIRTUAL, IF_TYPE_WWANPP, IF_TYPE_WWANPP2, MIB_IF_ROW2, MIB_IF_TABLE2,
    MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::AF_UNSPEC;

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

#[derive(Debug, Default)]
pub(super) struct NetProbe {
    /// Listed interfaces, in display order: physical first, then by name.
    listed: Vec<Arc<AdapterInfo>>,
    discovered_at: Option<Instant>,
    prev: HashMap<u64, Octets>,
    last: Option<Instant>,
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
        let addressed = addressed_interfaces();

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
        let previous = std::mem::take(&mut self.listed);
        for row in rows {
            let Some(kind) = kind_of(row) else {
                continue;
            };
            // SAFETY: NET_LUID_LH is a union over one u64.
            let id = unsafe { row.InterfaceLuid.Value };
            let hardware = row.InterfaceAndOperStatusFlags._bitfield & FLAG_HARDWARE != 0;
            if !hardware && !addressed.contains(&id) {
                continue;
            }
            let name = wide(&row.Alias);
            let adapter = wide(&row.Description);
            // Keep the same Arc when nothing changed, so samples keep pointing at one
            // allocation per adapter.
            let info = previous
                .iter()
                .find(|i| i.id == id && i.name == name && i.adapter == adapter)
                .cloned()
                .unwrap_or_else(|| {
                    Arc::new(AdapterInfo {
                        id,
                        name,
                        adapter,
                        kind,
                        hardware,
                    })
                });
            self.listed.push(info);
        }
        // SAFETY: allocated by GetIfTable2Ex, freed exactly once, not used after.
        unsafe { FreeMibTable(table.cast()) };

        self.listed.sort_by(|a, b| {
            b.hardware
                .cmp(&a.hardware)
                .then_with(|| a.name.cmp(&b.name))
        });
        let listed = &self.listed;
        self.prev.retain(|id, _| listed.iter().any(|i| i.id == *id));
    }
}

/// Every interface with at least one unicast IP address.
fn addressed_interfaces() -> Vec<u64> {
    let mut out = Vec::new();
    let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
    // SAFETY: `table` is a valid out-pointer; freed with FreeMibTable below.
    if unsafe { GetUnicastIpAddressTable(AF_UNSPEC, &raw mut table) }.is_err() || table.is_null() {
        return out;
    }
    // SAFETY: the call succeeded: `NumEntries` rows follow the header.
    let rows = unsafe {
        std::slice::from_raw_parts(
            (&raw const (*table).Table).cast::<MIB_UNICASTIPADDRESS_ROW>(),
            (*table).NumEntries as usize,
        )
    };
    for r in rows {
        // SAFETY: NET_LUID_LH is a union over one u64.
        let id = unsafe { r.InterfaceLuid.Value };
        if !out.contains(&id) {
            out.push(id);
        }
    }
    // SAFETY: allocated by the call above, freed exactly once.
    unsafe { FreeMibTable(table.cast()) };
    out
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
        assert!(!out.is_empty());
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
    }
}
