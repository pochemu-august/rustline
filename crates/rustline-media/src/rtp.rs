//! RTP media pipeline stubs.
//!
//! Future: Will handle RTP packet sending/receiving, SRTP, and jitter buffering.

/// Placeholder for RTP session creation.
pub fn create_session(_local_port: u16) -> Result<(), String> {
    Err("RTP subsystem not yet implemented".to_string())
}
