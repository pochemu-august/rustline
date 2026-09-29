//! # rustline-media
//!
//! Audio I/O and RTP media pipeline for the rustline softphone.
//!
//! ## Current Status: Stubs
//!
//! This crate will eventually contain:
//! - Audio capture/playback via `cpal`
//! - RTP packet encoding/decoding
//! - SRTP encryption
//! - Codec negotiation (G.711 PCMA/PCMU first, then Opus)
//! - Jitter buffer
//!
//! ## Planned codec support (from MicroSIP analysis)
//!
//! | Priority | Codec | Sample Rate |
//! |----------|-------|-------------|
//! | Default  | PCMA (G.711 A-law) | 8 kHz |
//! | Default  | PCMU (G.711 μ-law) | 8 kHz |
//! | Optional | Opus | 48 kHz |
//! | Optional | G.722 | 16 kHz |

pub mod audio;
pub mod rtp;

/// Placeholder: Check if the media subsystem is available.
pub fn is_available() -> bool {
    // Will return true once cpal is initialized
    false
}
