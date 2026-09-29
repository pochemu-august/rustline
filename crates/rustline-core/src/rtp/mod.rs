//! RTP (Real-time Transport Protocol, RFC 3550) implementation.
//!
//! Provides:
//! - RTP packet serialization and parsing (12-byte fixed header).
//! - Active media stream session (sending 20ms G.711 frames and receiving inbound audio).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use thiserror::Error;
use tokio::net::UdpSocket;
use tracing::{debug, error, info, trace};

#[allow(unused_imports)]
use crate::audio::g711;

#[derive(Debug, Error)]
pub enum RtpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("packet too short (must be at least 12 bytes)")]
    PacketTooShort,
    #[error("invalid RTP version (must be 2)")]
    InvalidVersion,
}

// ── RTP Packet ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RtpPacket {
    pub payload_type: u8,
    pub marker: bool,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: Vec<u8>,
}

impl RtpPacket {
    /// Serializes the RTP packet to network bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12 + self.payload.len());

        // Byte 0: V=2 (0b10), P=0, X=0, CC=0 -> 0x80
        buf.push(0x80);

        // Byte 1: Marker (bit 7) | Payload Type (bits 0-6)
        let m_pt = if self.marker { 0x80 } else { 0x00 } | (self.payload_type & 0x7F);
        buf.push(m_pt);

        // Bytes 2-3: Sequence Number
        buf.extend_from_slice(&self.sequence_number.to_be_bytes());

        // Bytes 4-7: Timestamp
        buf.extend_from_slice(&self.timestamp.to_be_bytes());

        // Bytes 8-11: SSRC
        buf.extend_from_slice(&self.ssrc.to_be_bytes());

        // Payload
        buf.extend_from_slice(&self.payload);

        buf
    }

    /// Parses an RTP packet from raw network bytes.
    pub fn parse(data: &[u8]) -> Result<Self, RtpError> {
        if data.len() < 12 {
            return Err(RtpError::PacketTooShort);
        }

        let v_p_x_cc = data[0];
        let version = (v_p_x_cc >> 6) & 0x03;
        if version != 2 {
            return Err(RtpError::InvalidVersion);
        }

        let m_pt = data[1];
        let marker = (m_pt & 0x80) != 0;
        let payload_type = m_pt & 0x7F;

        let sequence_number = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        let payload = data[12..].to_vec();

        Ok(RtpPacket {
            payload_type,
            marker,
            sequence_number,
            timestamp,
            ssrc,
            payload,
        })
    }
}

// ── RTP Stream Handle ───────────────────────────────────────────────────────

use std::sync::RwLock;

/// A handle to an active RTP media streaming session.
pub struct RtpStream {
    pub local_port: u16,
    stop_signal: Arc<AtomicBool>,
    remote_addr: Arc<RwLock<SocketAddr>>,
}

impl RtpStream {
    /// Update the remote destination for RTP packets (e.g. after in-dialog re-INVITE).
    pub fn update_remote_target(&self, new_addr: SocketAddr) {
        if let Ok(mut lock) = self.remote_addr.write() {
            info!(old = %*lock, new = %new_addr, "updating RTP destination");
            *lock = new_addr;
        }
    }

    /// Binds a new UDP socket and starts the RTP session.
    pub async fn start(
        remote_rtp_addr: SocketAddr,
        payload_type: u8,
    ) -> Result<Self, RtpError> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        Self::start_with_socket(socket, remote_rtp_addr, payload_type).await
    }

    /// Starts streaming tasks using an already bound UdpSocket.
    /// This guarantees that the local RTP port in SDP matches the actual socket.
    pub async fn start_with_socket(
        socket: UdpSocket,
        remote_rtp_addr: SocketAddr,
        payload_type: u8,
    ) -> Result<Self, RtpError> {
        let local_port = socket.local_addr()?.port();
        debug!(local_port, remote = %remote_rtp_addr, "RTP socket bound");

        let stop_signal = Arc::new(AtomicBool::new(false));
        let socket_arc = Arc::new(socket);
        let remote_addr_arc = Arc::new(RwLock::new(remote_rtp_addr));

        // Queue for echo loopback (audio received from caller is sent back)
        let (echo_tx, mut echo_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);

        // Transmitter task: sends 20ms frames (160 samples) of audio
        let tx_socket = Arc::clone(&socket_arc);
        let tx_stop = Arc::clone(&stop_signal);
        let tx_remote = Arc::clone(&remote_addr_arc);

        tokio::spawn(async move {
            let mut seq: u16 = rand::thread_rng().gen();
            let mut ts: u32 = rand::thread_rng().gen();
            let ssrc: u32 = rand::thread_rng().gen();

            let mut ticker = tokio::time::interval(Duration::from_millis(20));
            let silence_frame = vec![0xFFu8; 160]; // 20ms @ 8000 Hz digital silence

            let mut frame_idx: u64 = 0;
            let mut first = true;
            while !tx_stop.load(Ordering::Relaxed) {
                ticker.tick().await;
                frame_idx = frame_idx.wrapping_add(1);

                // Priority 1: If caller spoke and we received audio, echo it back!
                // Priority 2: When quiet, play a gentle periodic chime (200ms 440Hz tone every 3 seconds)
                //             so caller can immediately verify audio is working.
                let payload = if let Ok(p) = echo_rx.try_recv() {
                    p
                } else if frame_idx % 150 < 10 { // 10 frames = 200ms every 150 frames (3s)
                    let mut beep = Vec::with_capacity(160);
                    let phase_offset = (frame_idx % 150) * 160;
                    for i in 0..160 {
                        let t = (phase_offset + i as u64) as f32 / 8000.0;
                        let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                        let pcm = (sample * 6000.0) as i16;
                        beep.push(crate::audio::g711::linear_to_ulaw(pcm));
                    }
                    beep
                } else {
                    silence_frame.clone()
                };

                let packet = RtpPacket {
                    payload_type,
                    marker: first,
                    sequence_number: seq,
                    timestamp: ts,
                    ssrc,
                    payload,
                };
                first = false;

                let target = match tx_remote.read() {
                    Ok(guard) => *guard,
                    Err(_) => remote_rtp_addr,
                };

                let bytes = packet.to_bytes();
                if let Err(e) = tx_socket.send_to(&bytes, target).await {
                    error!(error = %e, "error sending RTP packet");
                    break;
                }

                seq = seq.wrapping_add(1);
                ts = ts.wrapping_add(160);
            }
            debug!("RTP transmitter task stopped");
        });

        // Receiver task: reads incoming RTP packets and pushes to echo queue
        let rx_socket = Arc::clone(&socket_arc);
        let rx_stop = Arc::clone(&stop_signal);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 1500];
            while !rx_stop.load(Ordering::Relaxed) {
                match rx_socket.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        if let Ok(pkt) = RtpPacket::parse(&buf[..n]) {
                            trace!(
                                from = %from,
                                seq = pkt.sequence_number,
                                ts = pkt.timestamp,
                                len = pkt.payload.len(),
                                "received RTP packet"
                            );
                            let _ = echo_tx.try_send(pkt.payload);
                        }
                    }
                    Err(e) => {
                        // On Windows, WSAECONNRESET (error 10054) can be triggered on UDP sockets
                        // when a previous outgoing packet received an ICMP Port Unreachable.
                        // We must ignore this and continue receiving.
                        if let Some(10054) = e.raw_os_error() {
                            continue;
                        }
                        if !rx_stop.load(Ordering::Relaxed) {
                            error!(error = %e, "error receiving RTP packet");
                        }
                        break;
                    }
                }
            }
            debug!("RTP receiver task stopped");
        });

        info!(local_port, remote = %remote_rtp_addr, "RTP audio stream active");
        Ok(RtpStream {
            local_port,
            stop_signal,
            remote_addr: remote_addr_arc,
        })
    }

    /// Stops the media stream.
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::Relaxed);
    }
}

impl Drop for RtpStream {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtp_packet_roundtrip() {
        let packet = RtpPacket {
            payload_type: 0,
            marker: true,
            sequence_number: 1234,
            timestamp: 567890,
            ssrc: 0xAABBCCDD,
            payload: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };

        let bytes = packet.to_bytes();
        assert_eq!(bytes.len(), 12 + 8);

        let parsed = RtpPacket::parse(&bytes).unwrap();
        assert_eq!(parsed.payload_type, 0);
        assert_eq!(parsed.marker, true);
        assert_eq!(parsed.sequence_number, 1234);
        assert_eq!(parsed.timestamp, 567890);
        assert_eq!(parsed.ssrc, 0xAABBCCDD);
        assert_eq!(parsed.payload, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
