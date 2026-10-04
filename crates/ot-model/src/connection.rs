//! Network endpoints and which process owns each.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
    Tcp6,
    Udp,
    Udp6,
}

impl Protocol {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Tcp6 => "TCPv6",
            Self::Udp => "UDP",
            Self::Udp6 => "UDPv6",
        }
    }
}

/// A TCP connection's state, as RFC 793 names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    DeleteTcb,
    #[default]
    Unknown,
}

impl TcpState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Closed => "Closed",
            Self::Listen => "Listening",
            Self::SynSent => "SYN sent",
            Self::SynReceived => "SYN received",
            Self::Established => "Established",
            Self::FinWait1 => "FIN wait 1",
            Self::FinWait2 => "FIN wait 2",
            Self::CloseWait => "Close wait",
            Self::Closing => "Closing",
            Self::LastAck => "Last ACK",
            Self::TimeWait => "Time wait",
            Self::DeleteTcb => "Delete TCB",
            Self::Unknown => "",
        }
    }
}

/// One endpoint: a TCP connection or listener, or a bound UDP socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub protocol: Protocol,
    pub local: SocketAddr,
    /// The far end, for a TCP connection; `None` for a listener or UDP.
    pub remote: Option<SocketAddr>,
    /// TCP only; `Unknown` for UDP.
    pub state: TcpState,
    /// The owning process's PID, 0 when the system owns it.
    pub pid: u32,
}
