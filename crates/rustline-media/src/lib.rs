//! # rustline-media
//!
//! Pure Rust Audio I/O and RTP media pipeline for the rustline softphone.
//!
//! - Audio capture/playback via `cpal` (WASAPI / ALSA / CoreAudio)
//! - Pure Rust ITU-T G.711 PCMA (A-law) and PCMU (μ-law) codecs
//! - Pure Rust RFC 3550 RTP packet encoder/decoder
//! - Background RTP UDP streaming session with jitter buffering

pub mod audio;
pub mod codec;
pub mod rtp;

pub use audio::{AudioEngine, is_audio_available};
pub use codec::g711;
pub use rtp::{AudioBuffer, RtpPacket, RtpSession};

/// Check if the media subsystem (audio devices) is available on the current host.
pub fn is_available() -> bool {
    audio::is_audio_available()
}
