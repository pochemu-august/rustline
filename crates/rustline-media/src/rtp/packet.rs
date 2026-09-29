//! Pure Rust RTP (Real-Time Transport Protocol) Packet implementation (RFC 3550).

use anyhow::{Result, anyhow};

/// Represents an RFC 3550 RTP Packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    /// RTP version (must be 2).
    pub version: u8,
    /// Padding flag.
    pub padding: bool,
    /// Extension flag.
    pub extension: bool,
    /// CSRC count (0..15).
    pub csrc_count: u8,
    /// Marker bit (e.g. start of talkspurt).
    pub marker: bool,
    /// Payload type (e.g. 0 for PCMU, 8 for PCMA, 101 for DTMF telephone-event).
    pub payload_type: u8,
    /// Sequence number (increments by 1 for each packet).
    pub sequence_number: u16,
    /// Timestamp (reflects sampling instant, increments by 160 for 20ms of 8kHz audio).
    pub timestamp: u32,
    /// Synchronization Source (SSRC) identifier.
    pub ssrc: u32,
    /// Contributing Source (CSRC) identifiers.
    pub csrc: Vec<u32>,
    /// Audio or media payload data.
    pub payload: Vec<u8>,
}

impl RtpPacket {
    /// Create a new standard audio RTP packet.
    pub fn new(
        payload_type: u8,
        sequence_number: u16,
        timestamp: u32,
        ssrc: u32,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            csrc: Vec::new(),
            payload,
        }
    }

    /// Parse an RTP packet from raw bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 12 {
            return Err(anyhow!(
                "RTP packet too small: {} bytes (minimum 12)",
                data.len()
            ));
        }

        let b0 = data[0];
        let version = (b0 >> 6) & 0x03;
        if version != 2 {
            return Err(anyhow!("Unsupported RTP version: {}", version));
        }

        let padding = (b0 & 0x20) != 0;
        let extension = (b0 & 0x10) != 0;
        let csrc_count = b0 & 0x0F;

        let b1 = data[1];
        let marker = (b1 & 0x80) != 0;
        let payload_type = b1 & 0x7F;

        let sequence_number = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        let header_len = 12 + (csrc_count as usize * 4);
        if data.len() < header_len {
            return Err(anyhow!("RTP packet truncated: missing CSRC list"));
        }

        let mut csrc = Vec::with_capacity(csrc_count as usize);
        let mut offset = 12;
        for _ in 0..csrc_count {
            let id = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            csrc.push(id);
            offset += 4;
        }

        // If extension header is present, skip it (RFC 3550 5.3.1)
        if extension {
            if data.len() < offset + 4 {
                return Err(anyhow!("RTP packet truncated: missing extension header"));
            }
            let ext_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize * 4;
            offset += 4 + ext_len;
            if data.len() < offset {
                return Err(anyhow!("RTP packet truncated: extension data cut off"));
            }
        }

        let payload_end = if padding && !data.is_empty() {
            let pad_len = *data.last().unwrap() as usize;
            if pad_len > (data.len() - offset) {
                data.len()
            } else {
                data.len() - pad_len
            }
        } else {
            data.len()
        };

        let payload = if offset <= payload_end {
            data[offset..payload_end].to_vec()
        } else {
            Vec::new()
        };

        Ok(Self {
            version,
            padding,
            extension,
            csrc_count,
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            csrc,
            payload,
        })
    }

    /// Serialize the RTP packet into raw bytes for UDP transmission.
    pub fn to_bytes(&self) -> Vec<u8> {
        let header_len = 12 + (self.csrc.len() * 4);
        let mut buf = Vec::with_capacity(header_len + self.payload.len());

        let b0 = (self.version << 6)
            | ((self.padding as u8) << 5)
            | ((self.extension as u8) << 4)
            | ((self.csrc.len() as u8) & 0x0F);
        buf.push(b0);

        let b1 = ((self.marker as u8) << 7) | (self.payload_type & 0x7F);
        buf.push(b1);

        buf.extend_from_slice(&self.sequence_number.to_be_bytes());
        buf.extend_from_slice(&self.timestamp.to_be_bytes());
        buf.extend_from_slice(&self.ssrc.to_be_bytes());

        for csrc in &self.csrc {
            buf.extend_from_slice(&csrc.to_be_bytes());
        }

        buf.extend_from_slice(&self.payload);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtp_serialize_and_parse() {
        let packet = RtpPacket::new(8, 1234, 160000, 0x12345678, vec![0x55; 160]);
        let bytes = packet.to_bytes();
        assert_eq!(bytes.len(), 12 + 160);

        let parsed = RtpPacket::from_bytes(&bytes).expect("Failed to parse RTP packet");
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.payload_type, 8);
        assert_eq!(parsed.sequence_number, 1234);
        assert_eq!(parsed.timestamp, 160000);
        assert_eq!(parsed.ssrc, 0x12345678);
        assert_eq!(parsed.payload.len(), 160);
        assert_eq!(parsed.payload[0], 0x55);
    }
}
