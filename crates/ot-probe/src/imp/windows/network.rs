//! Network adapters: which are connected, and their traffic.
//!
//! `GetIfTable2` lists every interface with 64-bit octet counters, the connection
//! name users know (`Wi-Fi`), the adapter's description, and its link speed. One
//! call per pass; rates are differences of the counters over the pass's wall time.
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
use std::time::Instant;

use ot_model::device::{AdapterInfo, AdapterSample, LinkKind};
use ot_model::Bytes;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, GetUnicastIpAddressTable, IF_TYPE_ETHERNET_CSMACD,
    IF_TYPE_IEEE80211, IF_TYPE_PPP, IF_TYPE_PROP_VIRTUAL, IF_TYPE_WWANPP, IF_TYPE_WWANPP2,
    MIB_IF_ROW2, MIB_IF_TABLE2, MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::AF_UNSPEC;

/// `MIB_IF_ROW2.InterfaceAndOperStatusFlags` bits, from `netioapi.h`.
const FLAG_HARDWARE: u8 = 1 << 0;
const FLAG_FILTER: u8 = 1 << 1;
/// Addresses change rarely; re-read them this often, or when an interface appears.
const ADDRESS_REFRESH: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
struct Octets {
    rx: u64,
    tx: u64,
}

#[derive(Debug, Default)]
pub(super) struct NetProbe {
    prev: HashMap<u64, Octets>,
    infos: HashMap<u64, Arc<AdapterInfo>>,
    last: Option<Instant>,
    /// Interfaces that carry at least one IP address, as of `addressed_at`.
    addressed: Vec<u64>,
    addressed_at: Option<Instant>,
}

impl NetProbe {
    /// Refill `out` with every listed adapter. Rates are zero for an adapter's
    /// first pass, when there is nothing to difference against.
    pub fn sample(&mut self, out: &mut Vec<AdapterSample>) {
        out.clear();
        let now = Instant::now();
        let secs = self.last.map(|t| now.duration_since(t).as_secs_f64());
        self.last = Some(now);

        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: `table` is a valid out-pointer; freed with FreeMibTable below.
        if unsafe { GetIfTable2(&raw mut table) }.is_err() || table.is_null() {
            return;
        }
        // SAFETY: GetIfTable2 succeeded: `NumEntries` rows follow the header.
        let rows = unsafe {
            std::slice::from_raw_parts(
                (&raw const (*table).Table).cast::<MIB_IF_ROW2>(),
                (*table).NumEntries as usize,
            )
        };

        // A candidate we have not seen may have just come up: re-read addresses.
        let stale = self
            .addressed_at
            .is_none_or(|t| now.duration_since(t) >= ADDRESS_REFRESH);
        // SAFETY: NET_LUID_LH is a union over one u64.
        let luid = |row: &MIB_IF_ROW2| unsafe { row.InterfaceLuid.Value };
        if stale
            || rows
                .iter()
                .any(|r| kind_of(r).is_some() && !self.prev.contains_key(&luid(r)))
        {
            self.refresh_addresses(now);
        }

        let mut seen = Vec::with_capacity(8);
        for row in rows {
            let Some(kind) = kind_of(row) else {
                continue;
            };
            let id = luid(row);
            let hardware = row.InterfaceAndOperStatusFlags._bitfield & FLAG_HARDWARE != 0;
            if !hardware && !self.addressed.contains(&id) {
                continue;
            }
            seen.push(id);
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
            let (rx, tx) = match self.prev.insert(id, now_octets) {
                Some(p) => (rate(now_octets.rx, p.rx), rate(now_octets.tx, p.tx)),
                None => (Bytes::ZERO, Bytes::ZERO),
            };
            let info = self
                .infos
                .entry(id)
                .or_insert_with(|| {
                    Arc::new(AdapterInfo {
                        id,
                        name: wide(&row.Alias),
                        adapter: wide(&row.Description),
                        kind,
                        hardware,
                    })
                })
                .clone();
            let link = row.TransmitLinkSpeed.max(row.ReceiveLinkSpeed);
            out.push(AdapterSample {
                info,
                rx_per_sec: rx,
                tx_per_sec: tx,
                // Unknown speeds are reported as all ones.
                link_bps: (link > 0 && link != u64::MAX).then_some(link),
            });
        }
        // SAFETY: allocated by GetIfTable2, freed exactly once, not used after.
        unsafe { FreeMibTable(table.cast()) };

        self.prev.retain(|id, _| seen.contains(id));
        self.infos.retain(|id, _| seen.contains(id));
        // Physical adapters first, then by name.
        out.sort_by(|a, b| {
            b.info
                .hardware
                .cmp(&a.info.hardware)
                .then_with(|| a.info.name.cmp(&b.info.name))
        });
    }
}

impl NetProbe {
    /// Which interfaces have a unicast IP address right now.
    fn refresh_addresses(&mut self, now: Instant) {
        self.addressed_at = Some(now);
        let mut table: *mut MIB_UNICASTIPADDRESS_TABLE = std::ptr::null_mut();
        // SAFETY: `table` is a valid out-pointer; freed with FreeMibTable below.
        if unsafe { GetUnicastIpAddressTable(AF_UNSPEC, &raw mut table) }.is_err()
            || table.is_null()
        {
            return;
        }
        // SAFETY: the call succeeded: `NumEntries` rows follow the header.
        let rows = unsafe {
            std::slice::from_raw_parts(
                (&raw const (*table).Table).cast::<MIB_UNICASTIPADDRESS_ROW>(),
                (*table).NumEntries as usize,
            )
        };
        self.addressed.clear();
        for r in rows {
            // SAFETY: NET_LUID_LH is a union over one u64.
            let id = unsafe { r.InterfaceLuid.Value };
            if !self.addressed.contains(&id) {
                self.addressed.push(id);
            }
        }
        // SAFETY: allocated by the call above, freed exactly once.
        unsafe { FreeMibTable(table.cast()) };
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
        std::thread::sleep(std::time::Duration::from_millis(300));
        probe.sample(&mut out);
        assert!(!out.is_empty());
    }
}
