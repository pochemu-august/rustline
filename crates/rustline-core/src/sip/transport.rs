//! SIP transport abstraction (UDP for now, TLS placeholder).
//!
//! The transport layer owns the socket and provides `send_to` / `recv_from`
//! plus local-address discovery. Each transport variant is behind an `enum` so
//! the caller doesn't need dynamic dispatch.

use std::net::{IpAddr, SocketAddr};
use thiserror::Error;
use tokio::net::UdpSocket;
use tracing::{debug, trace};

use crate::types::TransportType;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("DNS resolution failed for {host}: {source}")]
    DnsResolution {
        host: String,
        source: std::io::Error,
    },
    #[error("TLS transport not yet implemented")]
    TlsNotImplemented,
}

// ── UDP Transport ───────────────────────────────────────────────────────────

pub struct UdpTransport {
    socket: UdpSocket,
}

impl UdpTransport {
    /// Bind a UDP socket on an ephemeral port on all interfaces.
    pub async fn bind() -> Result<Self, TransportError> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        debug!(local = %socket.local_addr()?, "UDP transport bound");
        Ok(Self { socket })
    }

    /// Bind a UDP socket on a specific address.
    pub async fn bind_addr(addr: &str) -> Result<Self, TransportError> {
        let socket = UdpSocket::bind(addr).await?;
        debug!(local = %socket.local_addr()?, "UDP transport bound");
        Ok(Self { socket })
    }

    /// Send raw bytes to the given address.
    pub async fn send_to(
        &self,
        data: &[u8],
        addr: SocketAddr,
    ) -> Result<usize, TransportError> {
        trace!(to = %addr, bytes = data.len(), "UDP send");
        Ok(self.socket.send_to(data, addr).await?)
    }

    /// Receive raw bytes from the socket. Returns (bytes_read, source_addr).
    pub async fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, SocketAddr), TransportError> {
        let (n, addr) = self.socket.recv_from(buf).await?;
        trace!(from = %addr, bytes = n, "UDP recv");
        Ok((n, addr))
    }

    /// Local address the socket is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        Ok(self.socket.local_addr()?)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Discover the local IP that would be used to reach `remote`.
///
/// Works by "connecting" a UDP socket to the remote address (no data sent)
/// and reading back the chosen local address. This is the standard trick
/// for finding the outbound interface on multihomed hosts.
pub async fn discover_local_ip(remote: SocketAddr) -> Result<IpAddr, TransportError> {
    let probe = UdpSocket::bind("0.0.0.0:0").await?;
    probe.connect(remote).await?;
    let local = probe.local_addr()?;
    debug!(local_ip = %local.ip(), remote = %remote, "discovered local IP");
    Ok(local.ip())
}

/// Resolve a hostname + port into a [`SocketAddr`].
///
/// If `host` is already an IP address, parsing is instant.
/// Otherwise we perform async DNS resolution and return the first result.
pub async fn resolve(host: &str, port: u16) -> Result<SocketAddr, TransportError> {
    // Fast path: try parsing as IP literal
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }

    // Async DNS lookup
    let lookup = format!("{host}:{port}");
    let addr = tokio::net::lookup_host(&lookup)
        .await
        .map_err(|e| TransportError::DnsResolution {
            host: host.to_owned(),
            source: e,
        })?
        .next()
        .ok_or_else(|| TransportError::DnsResolution {
            host: host.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no addresses returned",
            ),
        })?;

    debug!(%host, resolved = %addr, "DNS resolved");
    Ok(addr)
}

/// Create the appropriate transport for the given type.
///
/// Returns a `SipTransport` enum that the caller can use polymorphically.
pub async fn create_transport(
    transport_type: TransportType,
) -> Result<SipTransport, TransportError> {
    match transport_type {
        TransportType::Udp => {
            let udp = UdpTransport::bind().await?;
            Ok(SipTransport::Udp(udp))
        }
        TransportType::Tls => Err(TransportError::TlsNotImplemented),
    }
}

// ── Polymorphic wrapper ─────────────────────────────────────────────────────

/// Enum-dispatch wrapper so callers don't need `dyn` or generics.
pub enum SipTransport {
    Udp(UdpTransport),
    // Tls(TlsTransport),  — will be added with rustls
}

impl SipTransport {
    pub async fn send_to(
        &self,
        data: &[u8],
        addr: SocketAddr,
    ) -> Result<usize, TransportError> {
        match self {
            Self::Udp(t) => t.send_to(data, addr).await,
        }
    }

    pub async fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, SocketAddr), TransportError> {
        match self {
            Self::Udp(t) => t.recv_from(buf).await,
        }
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        match self {
            Self::Udp(t) => t.local_addr(),
        }
    }

    pub fn transport_param(&self) -> &'static str {
        match self {
            Self::Udp(_) => "UDP",
        }
    }
}
