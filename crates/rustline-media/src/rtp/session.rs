//! RTP session handling (RFC 3550 audio stream transmission and reception).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, trace};

use super::packet::RtpPacket;
use crate::codec::g711::{SAMPLES_PER_FRAME, alaw_to_linear, linear_to_alaw};

/// Shared buffer for received PCM audio samples waiting to be played.
pub type AudioBuffer = Arc<std::sync::Mutex<VecDeque<i16>>>;

/// An active bidirectional RTP audio session.
pub struct RtpSession {
    local_port: u16,
    remote_addr: Arc<Mutex<Option<SocketAddr>>>,
    is_sdp_configured: Arc<AtomicBool>,
    is_running: Arc<AtomicBool>,
    _seq_num: Arc<AtomicU16>,
    _timestamp: Arc<AtomicU32>,
    _ssrc: u32,
    /// Channel to send outgoing microphone PCM samples into the RTP sender task.
    _mic_tx: mpsc::Sender<Vec<i16>>,
    /// Shared buffer from which the audio playback output device pulls received PCM samples.
    _playback_buf: AudioBuffer,
    /// Handle to abort background tasks when session stops.
    stop_tx: Option<tokio::sync::broadcast::Sender<()>>,
}

impl RtpSession {
    /// Create and start an RTP session on `local_port`.
    /// `initial_remote`: target remote IP:port (if known from SDP, e.g. Asterisk or peer).
    pub async fn start(
        local_ip: &str,
        local_port: u16,
        initial_remote: Option<SocketAddr>,
        playback_buf: AudioBuffer,
    ) -> anyhow::Result<(Self, mpsc::Sender<Vec<i16>>)> {
        let bind_addr = format!("{}:{}", local_ip, local_port);
        let socket = UdpSocket::bind(&bind_addr).await?;
        let actual_port = socket.local_addr()?.port();

        info!(
            local_port = actual_port,
            ?initial_remote,
            "Started RTP audio session"
        );

        let has_initial = initial_remote.is_some();
        let remote_addr = Arc::new(Mutex::new(initial_remote));
        let is_sdp_configured = Arc::new(AtomicBool::new(has_initial));
        let is_running = Arc::new(AtomicBool::new(true));
        let seq_num = Arc::new(AtomicU16::new(1000));
        let timestamp = Arc::new(AtomicU32::new(16000));
        let ssrc = uuid::Uuid::new_v4().as_u128() as u32;

        let (mic_tx, mut mic_rx) = mpsc::channel::<Vec<i16>>(100);
        let (stop_tx, _) = tokio::sync::broadcast::channel::<()>(1);

        let socket = Arc::new(socket);

        // --- Task 1: RTP Packet Receiver ---
        let recv_socket = Arc::clone(&socket);
        let recv_remote = Arc::clone(&remote_addr);
        let recv_sdp_auth = Arc::clone(&is_sdp_configured);
        let recv_running = Arc::clone(&is_running);
        let recv_playback = Arc::clone(&playback_buf);
        let mut stop_rx1 = stop_tx.subscribe();

        tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            while recv_running.load(Ordering::Relaxed) {
                tokio::select! {
                    _ = stop_rx1.recv() => {
                        break;
                    }
                    res = recv_socket.recv_from(&mut buf) => {
                        let (len, src_addr) = match res {
                            Ok(r) => r,
                            Err(e) => {
                                trace!("RTP recv_from error: {}", e);
                                break;
                            }
                        };

                        if len < 12 {
                            continue;
                        }

                        // Auto-learn remote RTP address (symmetric RTP / NAT traversal)
                        // ONLY if not explicitly configured by SDP signaling
                        if !recv_sdp_auth.load(Ordering::Relaxed) {
                            let mut rem = recv_remote.lock().await;
                            if rem.is_none() {
                                debug!(%src_addr, "Learned remote RTP source address via symmetric RTP");
                                *rem = Some(src_addr);
                            }
                        }

                        let packet = match RtpPacket::from_bytes(&buf[..len]) {
                            Ok(p) => p,
                            Err(e) => {
                                trace!("Invalid RTP packet: {}", e);
                                continue;
                            }
                        };

                        // Decode audio payload to 16-bit linear PCM
                        let mut pcm_samples = Vec::with_capacity(packet.payload.len());
                        if packet.payload_type == 8 {
                            // PCMA (A-law)
                            for &b in &packet.payload {
                                pcm_samples.push(alaw_to_linear(b));
                            }
                        } else if packet.payload_type == 0 {
                            // PCMU (μ-law)
                            for &b in &packet.payload {
                                pcm_samples.push(crate::codec::g711::ulaw_to_linear(b));
                            }
                        } else {
                            // Non-G711 (e.g. DTMF event) - ignore for now
                            continue;
                        }

                        // Push decoded PCM samples into playback buffer
                        if let Ok(mut lock) = recv_playback.lock() {
                            // Keep buffer small (at most ~200ms = 1600 samples) to prevent latency buildup
                            if lock.len() > 1600 {
                                lock.clear();
                            }
                            lock.extend(pcm_samples);
                        }
                    }
                }
            }
            debug!("RTP receiver task stopped");
        });

        // --- Task 2: RTP Packet Sender ---
        let send_socket = Arc::clone(&socket);
        let send_remote = Arc::clone(&remote_addr);
        let send_running = Arc::clone(&is_running);
        let send_seq = Arc::clone(&seq_num);
        let send_ts = Arc::clone(&timestamp);
        let mut stop_rx2 = stop_tx.subscribe();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(20));
            // Missed frame ticker behavior
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            while send_running.load(Ordering::Relaxed) {
                tokio::select! {
                    _ = stop_rx2.recv() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        let target = {
                            let rem = send_remote.lock().await;
                            *rem
                        };

                        let dest = match target {
                            Some(d) => d,
                            None => continue, // Waiting for remote address
                        };

                        // Get 160 PCM samples from mic, or send comfort silence if mic has no data
                        let pcm_chunk = match mic_rx.try_recv() {
                            Ok(chunk) if !chunk.is_empty() => chunk,
                            _ => vec![0i16; SAMPLES_PER_FRAME],
                        };

                        // Encode to G.711 A-law (PT 8)
                        let payload: Vec<u8> = pcm_chunk.iter().map(|&s| linear_to_alaw(s)).collect();

                        let seq = send_seq.fetch_add(1, Ordering::SeqCst);
                        let ts = send_ts.fetch_add(SAMPLES_PER_FRAME as u32, Ordering::SeqCst);

                        let packet = RtpPacket::new(8, seq, ts, ssrc, payload);
                        let bytes = packet.to_bytes();

                        if let Err(e) = send_socket.send_to(&bytes, dest).await {
                            trace!("Error sending RTP packet: {}", e);
                        }
                    }
                }
            }
            debug!("RTP sender task stopped");
        });

        let session = Self {
            local_port: actual_port,
            remote_addr,
            is_sdp_configured: Arc::clone(&is_sdp_configured),
            is_running,
            _seq_num: seq_num,
            _timestamp: timestamp,
            _ssrc: ssrc,
            _mic_tx: mic_tx.clone(),
            _playback_buf: playback_buf,
            stop_tx: Some(stop_tx),
        };

        Ok((session, mic_tx))
    }

    /// Update the remote target address (e.g. from SDP answer or re-INVITE).
    pub async fn set_remote_addr(&self, addr: SocketAddr) {
        let mut rem = self.remote_addr.lock().await;
        *rem = Some(addr);
        self.is_sdp_configured.store(true, Ordering::SeqCst);
        info!(remote_addr = %addr, "Set authoritative RTP destination address from SDP");
    }

    /// Stop the RTP session and background sender/receiver tasks.
    pub fn stop(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        info!(local_port = self.local_port, "RTP session stopped");
    }
}

impl Drop for RtpSession {
    fn drop(&mut self) {
        self.stop();
    }
}
