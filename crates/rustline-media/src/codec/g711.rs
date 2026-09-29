//! ITU-T G.711 PCMA (A-law) and PCMU (μ-law) audio codecs.
//!
//! Converts 16-bit linear PCM audio samples at 8000 Hz to/from 8-bit G.711 companded samples.
//! Implemented in pure safe Rust according to the ITU-T G.711 specification.

/// Payload types defined in RFC 3551
pub const PAYLOAD_TYPE_PCMU: u8 = 0;
pub const PAYLOAD_TYPE_PCMA: u8 = 8;
pub const SAMPLE_RATE: u32 = 8000;
pub const FRAME_DURATION_MS: u32 = 20;
pub const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE * FRAME_DURATION_MS / 1000) as usize; // 160 samples

// --- G.711 A-law ---

/// Convert a 16-bit linear PCM sample to an 8-bit A-law sample.
#[inline]
pub fn linear_to_alaw(pcm_val: i16) -> u8 {
    let mut pcm = pcm_val;
    let mask = if pcm >= 0 {
        0xD5
    } else {
        pcm = -pcm;
        0x55
    };

    if pcm == i16::MIN {
        pcm = 32767;
    }

    let seg = if pcm >= 4096 {
        if pcm >= 16384 {
            7
        } else if pcm >= 8192 {
            6
        } else {
            5
        }
    } else if pcm >= 512 {
        if pcm >= 2048 {
            4
        } else if pcm >= 1024 {
            3
        } else {
            2
        }
    } else if pcm >= 256 {
        1
    } else {
        0
    };

    let aval = if seg == 0 {
        (pcm >> 4) as u8
    } else {
        ((seg << 4) | ((pcm >> (seg + 3)) & 0x0F)) as u8
    };

    aval ^ mask
}

/// Convert an 8-bit A-law sample to a 16-bit linear PCM sample.
#[inline]
pub fn alaw_to_linear(a_val: u8) -> i16 {
    let a_val = a_val ^ 0x55;
    let mut t = (a_val & 0x0F) as i16;
    let seg = ((a_val >> 4) & 0x07) as i16;

    t = if seg == 0 {
        (t << 4) + 8
    } else {
        ((t << 4) + 0x108) << (seg - 1)
    };

    if (a_val & 0x80) != 0 { t } else { -t }
}

// --- G.711 μ-law ---

const BIAS: i16 = 0x84; // 132
const CLIP: i16 = 32635;

/// Convert a 16-bit linear PCM sample to an 8-bit μ-law sample.
#[inline]
pub fn linear_to_ulaw(pcm_val: i16) -> u8 {
    let (pcm, mask) = if pcm_val < 0 {
        let p = if pcm_val == i16::MIN { 32767 } else { -pcm_val };
        (p, 0x7F)
    } else {
        (pcm_val, 0xFF)
    };

    let pcm = pcm.min(CLIP) + BIAS;

    let seg = if pcm >= 16384 {
        7
    } else if pcm >= 8192 {
        6
    } else if pcm >= 4096 {
        5
    } else if pcm >= 2048 {
        4
    } else if pcm >= 1024 {
        3
    } else if pcm >= 512 {
        2
    } else if pcm >= 256 {
        1
    } else {
        0
    };

    let uval = ((seg << 4) | ((pcm >> (seg + 3)) & 0x0F)) as u8;
    uval ^ mask
}

/// Convert an 8-bit μ-law sample to a 16-bit linear PCM sample.
#[inline]
pub fn ulaw_to_linear(u_val: u8) -> i16 {
    let u_val = !u_val;
    let mut t = (((u_val & 0x0F) as i16) << 3) + BIAS;
    t <<= (u_val & 0x70) >> 4;

    if (u_val & 0x80) != 0 {
        BIAS - t
    } else {
        t - BIAS
    }
}

/// Encode a buffer of 16-bit linear PCM samples to G.711 A-law bytes.
pub fn encode_alaw(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().copied().map(linear_to_alaw).collect()
}

/// Decode a buffer of G.711 A-law bytes to 16-bit linear PCM samples.
pub fn decode_alaw(alaw: &[u8]) -> Vec<i16> {
    alaw.iter().copied().map(alaw_to_linear).collect()
}

/// Encode a buffer of 16-bit linear PCM samples to G.711 μ-law bytes.
pub fn encode_ulaw(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().copied().map(linear_to_ulaw).collect()
}

/// Decode a buffer of G.711 μ-law bytes to 16-bit linear PCM samples.
pub fn decode_ulaw(ulaw: &[u8]) -> Vec<i16> {
    ulaw.iter().copied().map(ulaw_to_linear).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alaw_roundtrip() {
        for &sample in &[-30000, -10000, -1000, -100, 0, 100, 1000, 10000, 30000] {
            let encoded = linear_to_alaw(sample);
            let decoded = alaw_to_linear(encoded);
            let diff = (sample as i32 - decoded as i32).abs();
            // G.711 quantization error should be within companding curve tolerance
            assert!(
                diff < (sample.abs() as i32 / 10).max(64),
                "A-law sample {} diff too large: {}",
                sample,
                diff
            );
        }
    }

    #[test]
    fn test_ulaw_roundtrip() {
        for &sample in &[-30000, -10000, -1000, -100, 0, 100, 1000, 10000, 30000] {
            let encoded = linear_to_ulaw(sample);
            let decoded = ulaw_to_linear(encoded);
            let diff = (sample as i32 - decoded as i32).abs();
            assert!(
                diff < (sample.abs() as i32 / 10).max(64),
                "μ-law sample {} diff too large: {}",
                sample,
                diff
            );
        }
    }
}
