//! G.711 Audio Codec implementation (PCMU / $\mu$-law and PCMA / A-law).
//!
//! Standard narrowband audio (8000 Hz sample rate, 8-bit samples).
//! 20 ms frame = 160 samples = 160 bytes over the wire.
//! Implemented purely in Rust using standard ITU-T G.711 companding formulas.

// ── $\mu$-law (PCMU, Payload Type 0) ────────────────────────────────────────

const BIAS: i16 = 0x84; // 132
const CLIP: i16 = 32635;

/// Encodes a 16-bit signed linear PCM sample into an 8-bit $\mu$-law sample.
pub fn linear_to_ulaw(mut pcm: i16) -> u8 {
    let sign = (pcm >> 8) & 0x80;
    if sign != 0 {
        pcm = -pcm;
    }
    if pcm > CLIP {
        pcm = CLIP;
    }
    pcm += BIAS;

    let mut exponent = 7;
    let mut mask = 0x4000;
    while (pcm & mask) == 0 && exponent > 0 {
        exponent -= 1;
        mask >>= 1;
    }

    let mantissa = (pcm >> (exponent + 3)) & 0x0F;
    let ulaw_byte = (sign | (exponent << 4) | mantissa) as u8;
    !ulaw_byte
}

/// Decodes an 8-bit $\mu$-law sample back into a 16-bit signed linear PCM sample.
pub fn ulaw_to_linear(ulaw: u8) -> i16 {
    let ulaw = !ulaw;
    let sign = ulaw & 0x80;
    let exponent = ((ulaw >> 4) & 0x07) as usize;
    let mantissa = (ulaw & 0x0F) as i16;

    let mut sample = ((mantissa << 3) + BIAS) << exponent;
    sample -= BIAS;

    if sign != 0 {
        -sample
    } else {
        sample
    }
}

// ── A-law (PCMA, Payload Type 8) ───────────────────────────────────────────

/// Encodes a 16-bit signed linear PCM sample into an 8-bit A-law sample.
pub fn linear_to_alaw(mut pcm: i16) -> u8 {
    let sign = if pcm >= 0 { 0xD5 } else { 0x55 };
    if pcm < 0 {
        pcm = -pcm;
    }
    if pcm > CLIP {
        pcm = CLIP;
    }

    let exponent: u8;
    let mantissa: u8;

    if pcm >= 256 {
        let mut exp = 7;
        let mut mask = 0x4000;
        while (pcm & mask) == 0 && exp > 1 {
            exp -= 1;
            mask >>= 1;
        }
        exponent = exp;
        mantissa = ((pcm >> (exp + 3)) & 0x0F) as u8;
    } else {
        exponent = 0;
        mantissa = ((pcm >> 4) & 0x0F) as u8;
    }

    (sign ^ ((exponent << 4) | mantissa)) as u8
}

/// Decodes an 8-bit A-law sample back into a 16-bit signed linear PCM sample.
pub fn alaw_to_linear(mut alaw: u8) -> i16 {
    alaw ^= 0x55;
    let sign = alaw & 0x80;
    let exponent = ((alaw >> 4) & 0x07) as usize;
    let mantissa = (alaw & 0x0F) as i16;

    let sample = if exponent == 0 {
        (mantissa << 4) + 8
    } else {
        ((mantissa << 4) + 0x108) << (exponent - 1)
    };

    if sign != 0 {
        sample
    } else {
        -sample
    }
}

// ── Audio Utilities ─────────────────────────────────────────────────────────

/// Generates 20ms of a sine tone at specified frequency (sample rate 8000 Hz).
/// 20ms * 8000 samples/sec = 160 samples.
pub fn generate_test_tone_ulaw(frequency: f32, sample_offset: usize) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(160);
    for i in 0..160 {
        let t = (sample_offset + i) as f32 / 8000.0;
        let sample_f = (2.0 * std::f32::consts::PI * frequency * t).sin();
        let sample_i16 = (sample_f * 16000.0) as i16; // moderate volume
        buffer.push(linear_to_ulaw(sample_i16));
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ulaw_roundtrip() {
        for original in [-30000, -10000, -500, 0, 500, 10000, 30000] {
            let encoded = linear_to_ulaw(original);
            let decoded = ulaw_to_linear(encoded);
            // G.711 is lossy 8-bit, check error is within quantization tolerance
            let diff = (original as i32 - decoded as i32).abs();
            assert!(diff < 600, "original: {original}, decoded: {decoded}, diff: {diff}");
        }
    }

    #[test]
    fn test_tone_generation() {
        let frame = generate_test_tone_ulaw(440.0, 0);
        assert_eq!(frame.len(), 160);
    }
}
