//! SDP (Session Description Protocol, RFC 4566) parsing and generation.
//!
//! Provides minimal, robust support for audio media sessions:
//! - Local offer generation (PCMU 0, PCMA 8, telephone-event 101).
//! - Remote answer parsing to extract media IP, RTP port, and selected codec.

use std::net::IpAddr;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdpSession {
    /// Connection IP address (`c=IN IP4 <ip>`).
    pub connection_ip: IpAddr,
    /// Media port (`m=audio <port> RTP/AVP ...`).
    pub media_port: u16,
    /// Payload type (e.g. 0 for PCMU, 8 for PCMA).
    pub payload_type: u8,
}

#[derive(Debug, Error)]
pub enum SdpError {
    #[error("missing 'c=' connection line in SDP")]
    MissingConnectionLine,
    #[error("missing 'm=audio' media line in SDP")]
    MissingMediaLine,
    #[error("invalid IP address in 'c=' line: {0}")]
    InvalidIp(String),
    #[error("invalid media port in 'm=' line: {0}")]
    InvalidPort(String),
}

impl SdpSession {
    /// Generates an SDP offer for local RTP media endpoint.
    pub fn build_offer(local_ip: IpAddr, rtp_port: u16, session_id: u64) -> String {
        format!(
            "v=0\r\n\
            o=Rustline {session_id} {session_id} IN IP4 {local_ip}\r\n\
            s=Rustline VoIP Call\r\n\
            c=IN IP4 {local_ip}\r\n\
            t=0 0\r\n\
            m=audio {rtp_port} RTP/AVP 0 8 101\r\n\
            a=rtpmap:0 PCMU/8000\r\n\
            a=rtpmap:8 PCMA/8000\r\n\
            a=rtpmap:101 telephone-event/8000\r\n\
            a=fmtp:101 0-16\r\n\
            a=sendrecv\r\n\
            a=ptime:20\r\n"
        )
    }

    /// Parses an SDP body (from a 200 OK or 183 Session Progress response).
    pub fn parse(sdp_text: &str) -> Result<Self, SdpError> {
        let mut connection_ip = None;
        let mut media_port = None;
        let mut payload_type = 0; // Default to PCMU (0)

        for line in sdp_text.lines() {
            let line = line.trim();
            if line.starts_with("c=") {
                // Example: c=IN IP4 192.168.0.104
                let parts: Vec<&str> = line[2..].split_whitespace().collect();
                if parts.len() >= 3 {
                    let ip_str = parts[2];
                    if let Ok(ip) = ip_str.parse::<IpAddr>() {
                        connection_ip = Some(ip);
                    } else {
                        return Err(SdpError::InvalidIp(ip_str.to_string()));
                    }
                }
            } else if line.starts_with("m=audio ") {
                // Example: m=audio 10240 RTP/AVP 0 8 101
                let parts: Vec<&str> = line[8..].split_whitespace().collect();
                if !parts.is_empty() {
                    let port_str = parts[0];
                    let port: u16 = port_str
                        .parse()
                        .map_err(|_| SdpError::InvalidPort(port_str.to_string()))?;
                    media_port = Some(port);

                    // Choose first supported payload type (prefer 0 or 8)
                    for pt_str in &parts[2..] {
                        if let Ok(pt) = pt_str.parse::<u8>() {
                            if pt == 0 || pt == 8 {
                                payload_type = pt;
                                break;
                            }
                        }
                    }
                }
            }
        }

        let connection_ip = connection_ip.ok_or(SdpError::MissingConnectionLine)?;
        let media_port = media_port.ok_or(SdpError::MissingMediaLine)?;

        Ok(SdpSession {
            connection_ip,
            media_port,
            payload_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sdp_offer_build() {
        let ip: IpAddr = "192.168.0.102".parse().unwrap();
        let offer = SdpSession::build_offer(ip, 16384, 12345);
        assert!(offer.contains("c=IN IP4 192.168.0.102"));
        assert!(offer.contains("m=audio 16384 RTP/AVP 0 8 101"));
    }

    #[test]
    fn test_sdp_answer_parse() {
        let answer = "v=0\r\n\
            o=Asterisk 1790531530 1790531530 IN IP4 192.168.0.104\r\n\
            s=Asterisk\r\n\
            c=IN IP4 192.168.0.104\r\n\
            t=0 0\r\n\
            m=audio 10254 RTP/AVP 0 101\r\n\
            a=rtpmap:0 PCMU/8000\r\n\
            a=rtpmap:101 telephone-event/8000\r\n\
            a=sendrecv\r\n";

        let session = SdpSession::parse(answer).unwrap();
        assert_eq!(session.connection_ip, "192.168.0.104".parse::<IpAddr>().unwrap());
        assert_eq!(session.media_port, 10254);
        assert_eq!(session.payload_type, 0);
    }
}
