//! Call state machine.
//!
//! Models the PJSIP INVITE state machine as used in MicroSIP's `on_call_state`:
//!
//! ```text
//! Outgoing: Idle → Calling → Early → Connecting → Confirmed → Disconnected
//! Incoming: Idle → Incoming → Connecting → Confirmed → Disconnected
//! ```

use rustline_proto::events::{CallDirection, CallState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a single SIP call with its state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    /// Unique identifier for this call (maps to SIP Call-ID).
    pub id: String,

    /// Current state of the call.
    pub state: CallState,

    /// Whether this is an inbound or outbound call.
    pub direction: CallDirection,

    /// Remote party display name (from SIP From/To header).
    pub remote_name: Option<String>,

    /// Remote party SIP URI.
    pub remote_uri: String,

    /// Call duration in seconds (only while Confirmed).
    pub duration_secs: u64,

    /// SIP response code at disconnect.
    pub last_code: Option<u16>,

    /// Disconnect reason text.
    pub last_reason: Option<String>,

    /// Whether local microphone is muted for this call.
    #[serde(default)]
    pub is_muted: bool,

    /// Whether local speaker is muted for this call.
    #[serde(default)]
    pub is_speaker_muted: bool,
}

impl Call {
    /// Create a new outgoing call.
    pub fn new_outgoing(target: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            state: CallState::Calling,
            direction: CallDirection::Outbound,
            remote_name: None,
            remote_uri: target.to_string(),
            duration_secs: 0,
            last_code: None,
            last_reason: None,
            is_muted: false,
            is_speaker_muted: false,
        }
    }

    /// Create a new incoming call.
    pub fn new_incoming(call_id: &str, caller_uri: &str, caller_name: Option<String>) -> Self {
        Self {
            id: call_id.to_string(),
            state: CallState::Incoming,
            direction: CallDirection::Inbound,
            remote_name: caller_name,
            remote_uri: caller_uri.to_string(),
            duration_secs: 0,
            last_code: None,
            last_reason: None,
            is_muted: false,
            is_speaker_muted: false,
        }
    }

    /// Attempt a state transition. Returns `true` if the transition is valid.
    pub fn transition(&mut self, new_state: CallState) -> bool {
        let valid = match (&self.state, &new_state) {
            // Outgoing flow
            (CallState::Calling, CallState::Early) => true,
            (CallState::Calling, CallState::Connecting) => true,
            (CallState::Calling, CallState::Disconnected) => true,
            (CallState::Early, CallState::Connecting) => true,
            (CallState::Early, CallState::Disconnected) => true,

            // Incoming flow
            (CallState::Incoming, CallState::Connecting) => true,
            (CallState::Incoming, CallState::Disconnected) => true,

            // Common flow
            (CallState::Connecting, CallState::Confirmed) => true,
            (CallState::Connecting, CallState::Disconnected) => true,
            (CallState::Confirmed, CallState::Held) => true,
            (CallState::Confirmed, CallState::Disconnected) => true,
            (CallState::Held, CallState::Confirmed) => true,
            (CallState::Held, CallState::Disconnected) => true,

            _ => false,
        };

        if valid {
            self.state = new_state;
        }
        valid
    }

    /// Check if the call is in a terminal state.
    pub fn is_terminated(&self) -> bool {
        self.state == CallState::Disconnected
    }

    /// Check if the call is active (connected or on hold).
    pub fn is_active(&self) -> bool {
        matches!(self.state, CallState::Confirmed | CallState::Held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outgoing_call_flow() {
        let mut call = Call::new_outgoing("sip:100@example.com");
        assert_eq!(call.state, CallState::Calling);

        assert!(call.transition(CallState::Early));
        assert_eq!(call.state, CallState::Early);

        assert!(call.transition(CallState::Connecting));
        assert_eq!(call.state, CallState::Connecting);

        assert!(call.transition(CallState::Confirmed));
        assert_eq!(call.state, CallState::Confirmed);

        assert!(call.transition(CallState::Disconnected));
        assert!(call.is_terminated());
    }

    #[test]
    fn test_incoming_call_flow() {
        let mut call = Call::new_incoming("abc-123", "sip:200@example.com", Some("Alice".into()));
        assert_eq!(call.state, CallState::Incoming);

        assert!(call.transition(CallState::Connecting));
        assert!(call.transition(CallState::Confirmed));
        assert!(call.is_active());

        assert!(call.transition(CallState::Disconnected));
        assert!(call.is_terminated());
    }

    #[test]
    fn test_invalid_transition() {
        let mut call = Call::new_outgoing("sip:100@example.com");
        // Cannot go from Calling directly to Confirmed
        assert!(!call.transition(CallState::Confirmed));
        // State should remain Calling
        assert_eq!(call.state, CallState::Calling);
    }

    #[test]
    fn test_hold_unhold() {
        let mut call = Call::new_outgoing("sip:100@example.com");
        call.transition(CallState::Connecting);
        call.transition(CallState::Confirmed);

        assert!(call.transition(CallState::Held));
        assert!(call.is_active());

        assert!(call.transition(CallState::Confirmed));
        assert!(call.is_active());
    }
}
