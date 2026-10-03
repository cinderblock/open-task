//! Open network endpoints and which process owns each.
//!
//! Four IP Helper tables, read whole: `GetExtendedTcpTable` with
//! `TCP_TABLE_OWNER_PID_ALL` for IPv4 and again for IPv6, and `GetExtendedUdpTable`
//! with `UDP_TABLE_OWNER_PID` for both. Each call copies the kernel's current table
//! into a caller-supplied buffer along with the owning PID of every row, which is
//! what `netstat -ano` and Resource Monitor read; no handle to any process is needed
//! and an unelevated caller sees every process's endpoints.
//!
//! Every table is sized by asking: a call with a too-small buffer fails with
//! `ERROR_INSUFFICIENT_BUFFER` and reports the size it wanted. The table can grow
//! between that report and the next call, so the buffer is grown with headroom and
//! the call repeated until it fits, as [`super::query_growing`] does for the process
//! list. One buffer serves all four tables in turn and is kept between calls, so a
//! steady-state `list` allocates nothing beyond growing its output.
//!
//! Addresses and ports in these tables are in network byte order: the IPv4 address
//! is four octets in memory order, and the port is the low 16 bits of a `u32` with
//! the bytes swapped. IPv6 rows carry a scope id, kept on the address so a
//! link-local listener reads as `[fe80::1%12]:445`.
//!
//! The listing is ordered TCP before UDP, IPv4 before IPv6, then by local port.
//!
//! Cost on this machine (Windows 11, a Hyper-V host, about 400 endpoints, dev
//! profile with dependencies optimized): 1.9 ms per `list` on average, 3 to 4 ms at
//! worst over 50 runs, measured by the ignored `list_cost` test. The time is the
//! kernel copying the tables out; the parse is a small fraction of it.
//!
//! ```text
//! cargo test -p ot-probe list_cost -- --ignored --nocapture
//! ```

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use ot_model::connection::{Connection, Protocol, TcpState};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
    MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_TCP_STATE_CLOSED, MIB_TCP_STATE_CLOSE_WAIT,
    MIB_TCP_STATE_CLOSING, MIB_TCP_STATE_DELETE_TCB, MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_FIN_WAIT1,
    MIB_TCP_STATE_FIN_WAIT2, MIB_TCP_STATE_LAST_ACK, MIB_TCP_STATE_LISTEN, MIB_TCP_STATE_SYN_RCVD,
    MIB_TCP_STATE_SYN_SENT, MIB_TCP_STATE_TIME_WAIT, MIB_UDP6ROW_OWNER_PID,
    MIB_UDP6TABLE_OWNER_PID, MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

use super::AlignedBuf;

/// First buffer size. A few hundred endpoints fit; a machine with more grows it
/// once and keeps the larger buffer.
const INITIAL_BYTES: usize = 64 * 1024;
/// Added on every growth, so a table that grows between the size report and the
/// fill still fits without another round trip.
const HEADROOM_BYTES: usize = 16 * 1024;
/// Rounds of growth before giving up on one table. Each round at least doubles the
/// buffer, so this is far beyond any real table; it only stops a pathological
/// provider from spinning.
const MAX_GROWTH: u32 = 8;

/// Reads the endpoint tables into one reusable buffer.
#[derive(Debug, Default)]
pub(super) struct ConnectionProbe {
    buf: AlignedBuf,
}

/// Which table to read. The two families share one API each; this picks the call.
#[derive(Debug, Clone, Copy)]
enum Table {
    Tcp4,
    Tcp6,
    Udp4,
    Udp6,
}

impl ConnectionProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Refill `out` with every TCP and UDP endpoint, TCP before UDP, IPv4 before
    /// IPv6, each group by local port. A table that cannot be read contributes
    /// nothing; the others are still listed.
    pub fn list(&mut self, out: &mut Vec<Connection>) {
        out.clear();
        for table in [Table::Tcp4, Table::Tcp6, Table::Udp4, Table::Udp6] {
            let start = out.len();
            if self.fill(table) {
                self.push_rows(table, out);
            }
            out[start..].sort_by_key(|c| c.local.port());
        }
    }

    /// Read one table into the buffer, growing until it fits. Returns whether the
    /// buffer now holds the table.
    fn fill(&mut self, table: Table) -> bool {
        if self.buf.len_bytes() == 0 {
            self.buf.resize_bytes(INITIAL_BYTES);
        }
        let mut rounds = 0;
        loop {
            let mut size = self.buf.len_bytes() as u32;
            // SAFETY: the buffer is `size` bytes and 8-byte aligned, more than the
            // 4 these tables need; `size` is a valid in-out pointer. The API writes
            // at most `size` bytes and reports the size it wants otherwise.
            let rc = unsafe {
                let ptr = Some(self.buf.as_mut_ptr().cast());
                match table {
                    Table::Tcp4 => GetExtendedTcpTable(
                        ptr,
                        &raw mut size,
                        false,
                        u32::from(AF_INET.0),
                        TCP_TABLE_OWNER_PID_ALL,
                        0,
                    ),
                    Table::Tcp6 => GetExtendedTcpTable(
                        ptr,
                        &raw mut size,
                        false,
                        u32::from(AF_INET6.0),
                        TCP_TABLE_OWNER_PID_ALL,
                        0,
                    ),
                    Table::Udp4 => GetExtendedUdpTable(
                        ptr,
                        &raw mut size,
                        false,
                        u32::from(AF_INET.0),
                        UDP_TABLE_OWNER_PID,
                        0,
                    ),
                    Table::Udp6 => GetExtendedUdpTable(
                        ptr,
                        &raw mut size,
                        false,
                        u32::from(AF_INET6.0),
                        UDP_TABLE_OWNER_PID,
                        0,
                    ),
                }
            };
            if rc == ERROR_SUCCESS.0 {
                return true;
            }
            if rc == ERROR_INSUFFICIENT_BUFFER.0 && rounds < MAX_GROWTH {
                rounds += 1;
                // The table can grow between the size report and the fill, so add
                // headroom instead of sizing exactly and looping again.
                let target = (size as usize).max(self.buf.len_bytes() * 2) + HEADROOM_BYTES;
                self.buf.resize_bytes(target);
                continue;
            }
            tracing::debug!(?table, rc, "endpoint table not read");
            return false;
        }
    }

    /// Append the rows of the table the buffer holds.
    fn push_rows(&self, table: Table, out: &mut Vec<Connection>) {
        let base = self.buf.as_ptr();
        match table {
            Table::Tcp4 => {
                // SAFETY: `fill` succeeded for this table, so the buffer starts with a
                // MIB_TCPTABLE_OWNER_PID whose `dwNumEntries` rows follow its header;
                // the 1-element `table` array is where the rows begin.
                let rows = unsafe {
                    let t = base.cast::<MIB_TCPTABLE_OWNER_PID>();
                    std::slice::from_raw_parts(
                        (&raw const (*t).table).cast::<MIB_TCPROW_OWNER_PID>(),
                        (*t).dwNumEntries as usize,
                    )
                };
                out.extend(rows.iter().map(tcp4));
            }
            Table::Tcp6 => {
                // SAFETY: as above, for MIB_TCP6TABLE_OWNER_PID.
                let rows = unsafe {
                    let t = base.cast::<MIB_TCP6TABLE_OWNER_PID>();
                    std::slice::from_raw_parts(
                        (&raw const (*t).table).cast::<MIB_TCP6ROW_OWNER_PID>(),
                        (*t).dwNumEntries as usize,
                    )
                };
                out.extend(rows.iter().map(tcp6));
            }
            Table::Udp4 => {
                // SAFETY: as above, for MIB_UDPTABLE_OWNER_PID.
                let rows = unsafe {
                    let t = base.cast::<MIB_UDPTABLE_OWNER_PID>();
                    std::slice::from_raw_parts(
                        (&raw const (*t).table).cast::<MIB_UDPROW_OWNER_PID>(),
                        (*t).dwNumEntries as usize,
                    )
                };
                out.extend(rows.iter().map(udp4));
            }
            Table::Udp6 => {
                // SAFETY: as above, for MIB_UDP6TABLE_OWNER_PID.
                let rows = unsafe {
                    let t = base.cast::<MIB_UDP6TABLE_OWNER_PID>();
                    std::slice::from_raw_parts(
                        (&raw const (*t).table).cast::<MIB_UDP6ROW_OWNER_PID>(),
                        (*t).dwNumEntries as usize,
                    )
                };
                out.extend(rows.iter().map(udp6));
            }
        }
    }
}

/// A port as the tables carry it: network byte order in the low 16 bits.
fn port(raw: u32) -> u16 {
    u16::from_be(raw as u16)
}

/// An IPv4 address as the tables carry it: four octets in memory order.
fn v4(raw: u32, port_raw: u32) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::from(raw.to_ne_bytes()),
        port(port_raw),
    ))
}

/// An IPv6 address with its scope id, which is what makes a link-local address
/// routable to one interface.
fn v6(raw: [u8; 16], scope: u32, port_raw: u32) -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(
        Ipv6Addr::from(raw),
        port(port_raw),
        0,
        scope,
    ))
}

fn tcp4(r: &MIB_TCPROW_OWNER_PID) -> Connection {
    let state = tcp_state(r.dwState);
    Connection {
        protocol: Protocol::Tcp,
        local: v4(r.dwLocalAddr, r.dwLocalPort),
        remote: (state != TcpState::Listen).then(|| v4(r.dwRemoteAddr, r.dwRemotePort)),
        state,
        pid: r.dwOwningPid,
    }
}

fn tcp6(r: &MIB_TCP6ROW_OWNER_PID) -> Connection {
    let state = tcp_state(r.dwState);
    Connection {
        protocol: Protocol::Tcp6,
        local: v6(r.ucLocalAddr, r.dwLocalScopeId, r.dwLocalPort),
        remote: (state != TcpState::Listen)
            .then(|| v6(r.ucRemoteAddr, r.dwRemoteScopeId, r.dwRemotePort)),
        state,
        pid: r.dwOwningPid,
    }
}

fn udp4(r: &MIB_UDPROW_OWNER_PID) -> Connection {
    Connection {
        protocol: Protocol::Udp,
        local: v4(r.dwLocalAddr, r.dwLocalPort),
        remote: None,
        state: TcpState::Unknown,
        pid: r.dwOwningPid,
    }
}

fn udp6(r: &MIB_UDP6ROW_OWNER_PID) -> Connection {
    Connection {
        protocol: Protocol::Udp6,
        local: v6(r.ucLocalAddr, r.dwLocalScopeId, r.dwLocalPort),
        remote: None,
        state: TcpState::Unknown,
        pid: r.dwOwningPid,
    }
}

/// `dwState` of a TCP row to the model's state. The SDK declares the constants as
/// `i32` and the row carries a `u32`; they are the same small positive numbers.
fn tcp_state(raw: u32) -> TcpState {
    const CLOSED: u32 = MIB_TCP_STATE_CLOSED.0 as u32;
    const LISTEN: u32 = MIB_TCP_STATE_LISTEN.0 as u32;
    const SYN_SENT: u32 = MIB_TCP_STATE_SYN_SENT.0 as u32;
    const SYN_RCVD: u32 = MIB_TCP_STATE_SYN_RCVD.0 as u32;
    const ESTAB: u32 = MIB_TCP_STATE_ESTAB.0 as u32;
    const FIN_WAIT1: u32 = MIB_TCP_STATE_FIN_WAIT1.0 as u32;
    const FIN_WAIT2: u32 = MIB_TCP_STATE_FIN_WAIT2.0 as u32;
    const CLOSE_WAIT: u32 = MIB_TCP_STATE_CLOSE_WAIT.0 as u32;
    const CLOSING: u32 = MIB_TCP_STATE_CLOSING.0 as u32;
    const LAST_ACK: u32 = MIB_TCP_STATE_LAST_ACK.0 as u32;
    const TIME_WAIT: u32 = MIB_TCP_STATE_TIME_WAIT.0 as u32;
    const DELETE_TCB: u32 = MIB_TCP_STATE_DELETE_TCB.0 as u32;
    match raw {
        CLOSED => TcpState::Closed,
        LISTEN => TcpState::Listen,
        SYN_SENT => TcpState::SynSent,
        SYN_RCVD => TcpState::SynReceived,
        ESTAB => TcpState::Established,
        FIN_WAIT1 => TcpState::FinWait1,
        FIN_WAIT2 => TcpState::FinWait2,
        CLOSE_WAIT => TcpState::CloseWait,
        CLOSING => TcpState::Closing,
        LAST_ACK => TcpState::LastAck,
        TIME_WAIT => TcpState::TimeWait,
        DELETE_TCB => TcpState::DeleteTcb,
        _ => TcpState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::net::{TcpListener, UdpSocket};
    use std::time::Instant;

    use super::*;

    fn listing() -> Vec<Connection> {
        let mut probe = ConnectionProbe::new();
        let mut out = Vec::new();
        probe.list(&mut out);
        out
    }

    #[test]
    fn this_machine_has_endpoints_and_they_are_consistent() {
        let out = listing();
        assert!(!out.is_empty(), "a networked machine has endpoints");
        let tcp = |c: &Connection| matches!(c.protocol, Protocol::Tcp | Protocol::Tcp6);
        assert!(
            out.iter()
                .any(|c| tcp(c) && c.state == TcpState::Listen && c.remote.is_none()),
            "something listens on TCP"
        );
        for c in &out {
            if tcp(c) {
                assert_eq!(
                    c.state != TcpState::Listen,
                    c.remote.is_some(),
                    "a connection has a far end, a listener has none: {c:?}"
                );
            } else {
                assert_eq!(c.remote, None, "{c:?}");
                assert_eq!(c.state, TcpState::Unknown, "{c:?}");
            }
            let v6 = matches!(c.protocol, Protocol::Tcp6 | Protocol::Udp6);
            assert_eq!(c.local.is_ipv6(), v6, "{c:?}");
        }
    }

    #[test]
    fn listing_is_ordered() {
        let out = listing();
        let rank = |p: Protocol| match p {
            Protocol::Tcp => 0,
            Protocol::Tcp6 => 1,
            Protocol::Udp => 2,
            Protocol::Udp6 => 3,
        };
        for pair in out.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                (rank(a.protocol), a.local.port()) <= (rank(b.protocol), b.local.port()),
                "{a:?} before {b:?}"
            );
        }
    }

    #[test]
    fn our_own_sockets_are_listed_with_our_pid() {
        let tcp = TcpListener::bind("127.0.0.1:0").expect("bind tcp");
        let udp = UdpSocket::bind("127.0.0.1:0").expect("bind udp");
        let tcp_addr = tcp.local_addr().expect("tcp addr");
        let udp_addr = udp.local_addr().expect("udp addr");
        let out = listing();
        let pid = std::process::id();

        let found = out
            .iter()
            .find(|c| c.protocol == Protocol::Tcp && c.local == tcp_addr);
        let found = found.unwrap_or_else(|| panic!("{tcp_addr} is listed: {out:?}"));
        assert_eq!(found.state, TcpState::Listen);
        assert_eq!(found.remote, None);
        assert_eq!(found.pid, pid);

        let found = out
            .iter()
            .find(|c| c.protocol == Protocol::Udp && c.local == udp_addr);
        let found = found.unwrap_or_else(|| panic!("{udp_addr} is listed: {out:?}"));
        assert_eq!(found.state, TcpState::Unknown);
        assert_eq!(found.remote, None);
        assert_eq!(found.pid, pid);
    }

    #[test]
    fn steady_state_keeps_its_buffer() {
        let mut probe = ConnectionProbe::new();
        let mut out = Vec::new();
        probe.list(&mut out);
        let bytes = probe.buf.len_bytes();
        probe.list(&mut out);
        assert_eq!(
            probe.buf.len_bytes(),
            bytes,
            "the buffer is kept between calls"
        );
    }

    #[test]
    fn tcp_states_map() {
        assert_eq!(tcp_state(2), TcpState::Listen);
        assert_eq!(tcp_state(5), TcpState::Established);
        assert_eq!(tcp_state(11), TcpState::TimeWait);
        assert_eq!(tcp_state(0), TcpState::Unknown);
        assert_eq!(tcp_state(100), TcpState::Unknown);
    }

    #[test]
    fn network_byte_order_is_undone() {
        // 127.0.0.1 is the bytes 7f 00 00 01 in memory; port 8080 is 0x1f90 big-endian.
        let raw = u32::from_ne_bytes([127, 0, 0, 1]);
        let port_raw = u32::from(0x1f90u16.to_be());
        assert_eq!(v4(raw, port_raw), "127.0.0.1:8080".parse().unwrap());
        let mut six = [0u8; 16];
        six[0] = 0xfe;
        six[1] = 0x80;
        six[15] = 1;
        assert_eq!(v6(six, 12, port_raw), "[fe80::1%12]:8080".parse().unwrap());
    }

    /// What one listing costs here. Ignored because it measures the machine it runs
    /// on; the number is in the module doc.
    #[test]
    #[ignore = "measures this machine; run by hand"]
    fn list_cost() {
        let mut probe = ConnectionProbe::new();
        let mut out = Vec::new();
        probe.list(&mut out);
        let n = 50;
        let mut worst = 0f64;
        let start = Instant::now();
        for _ in 0..n {
            let t = Instant::now();
            probe.list(&mut out);
            worst = worst.max(t.elapsed().as_secs_f64() * 1e3);
        }
        let mean = start.elapsed().as_secs_f64() * 1e3 / f64::from(n);
        println!(
            "list: mean {mean:.3} ms, worst {worst:.3} ms over {n} runs ({} endpoints, {} byte buffer)",
            out.len(),
            probe.buf.len_bytes()
        );
    }
}
