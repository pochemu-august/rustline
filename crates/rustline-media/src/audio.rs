//! Audio I/O stubs.
//!
//! Future: Will use `cpal` for cross-platform audio capture and playback.

/// Placeholder for audio device enumeration.
pub fn list_devices() -> Vec<String> {
    tracing::warn!("Audio device enumeration not yet implemented");
    vec![]
}

/// Placeholder for audio device selection.
pub fn select_device(_name: &str) -> Result<(), String> {
    Err("Audio subsystem not yet implemented".to_string())
}
