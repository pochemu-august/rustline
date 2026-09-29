//! SIP UDP Transport layer.
//!
//! Handles sending and receiving raw SIP messages over UDP.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::net::UdpSocket;
use tracing::{debug, trace};

/// SIP UDP transport wrapper.
#[derive(Clone)]
pub struct SipTransport {
    socket: Arc<UdpSocket>,
    local_ip: IpAddr,
    local_port: u16,
    remote_addr: SocketAddr,
}

impl SipTransport {
    /// Create and bind a new SIP UDP transport targeting a remote SIP server.
    pub async fn new(server_host: &str, server_port: u16) -> Result<Self> {
        let server_addr_str = format!("{}:{}", server_host, server_port);
        let remote_addr: SocketAddr = tokio::net::lookup_host(&server_addr_str)
            .await?
            .next()
            .ok_or_else(|| {
                anyhow!(
                    "Failed to resolve remote SIP server address: {}",
                    server_addr_str
                )
            })?;

        // Determine local outbound IP by creating a connected UDP socket to the server
        let dummy = std::net::UdpSocket::bind("0.0.0.0:0")?;
        dummy.connect(remote_addr)?;
        let local_ip = dummy.local_addr()?.ip();

        // Bind the actual asynchronous UDP socket on all interfaces with an OS-assigned port
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let local_port = socket.local_addr()?.port();

        debug!(
            %local_ip,
            local_port,
            %remote_addr,
            "Bound SIP UDP socket"
        );

        Ok(Self {
            socket: Arc::new(socket),
            local_ip,
            local_port,
            remote_addr,
        })
    }

    /// The local IP address used for Contact and Via headers.
    pub fn local_ip(&self) -> IpAddr {
        self.local_ip
    }

    /// The local UDP port used for Contact and Via headers.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// The remote SIP server address.
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    /// Get reference to the shared UDP socket.
    pub fn socket(&self) -> &Arc<UdpSocket> {
        &self.socket
    }

    /// Send raw SIP bytes to the remote SIP server.
    pub async fn send_raw(&self, data: &[u8]) -> Result<()> {
        self.send_to(data, self.remote_addr).await
    }

    /// Send raw SIP bytes to a specific destination socket address.
    pub async fn send_to(&self, data: &[u8], addr: SocketAddr) -> Result<()> {
        trace!(
            "SIP UDP send to {}:\n{}",
            addr,
            String::from_utf8_lossy(data)
        );
        self.socket.send_to(data, addr).await?;
        Ok(())
    }

    /// Send a CRLF keep-alive ping (as used by MicroSIP / RFC 5626).
    pub async fn send_keep_alive(&self) -> Result<()> {
        trace!("Sending SIP CRLF keep-alive ping to {}", self.remote_addr);
        self.socket.send_to(b"\r\n\r\n", self.remote_addr).await?;
        Ok(())
    }

    /// Receive the next SIP message with an optional timeout.
    pub async fn recv_message(&self, timeout: Duration) -> Result<(rsip::SipMessage, SocketAddr)> {
        let mut buf = vec![0u8; 65535];
        let recv_future = self.socket.recv_from(&mut buf);

        let (len, src_addr) = tokio::time::timeout(timeout, recv_future)
            .await
            .map_err(|_| anyhow!("SIP receive timed out after {:?}", timeout))??;

        let raw_bytes = &buf[..len];
        trace!(
            "SIP UDP recv from {}:\n{}",
            src_addr,
            String::from_utf8_lossy(raw_bytes)
        );

        let msg = rsip::SipMessage::try_from(raw_bytes)
            .map_err(|e| anyhow!("Failed to parse incoming SIP message: {}", e))?;

        Ok((msg, src_addr))
    }
}
