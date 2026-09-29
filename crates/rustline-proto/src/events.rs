//! Daemon → Client event types.

use serde::{Deserialize, Serialize};

/// The registration state of the SIP account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationState {
    /// No account configured.
    Unregistered,
    /// REGISTER request sent, waiting for response.
    Registering,
    /// Successfully registered (200 OK).
    Registered,
    /// Registration failed.
    Failed,
}

/// The state of an individual call, matching the SIP INVITE state machine.
///
/// Modeled after PJSIP's `pjsip_inv_state` as observed in MicroSIP:
/// `NULL → CALLING → EARLY → CONNECTING → CONFIRMED → DISCONNECTED`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    /// Outgoing INVITE sent, waiting for provisional response.
    Calling,
    /// Incoming call received, ringing on our side.
    Incoming,
    /// Provisional response received (180 Ringing / 183 Session Progress).
    Early,
    /// 200 OK received, ACK pending.
    Connecting,
    /// Call is fully established, media is flowing.
    Confirmed,
    /// Call is on hold (re-INVITE with inactive/sendonly SDP).
    Held,
    /// Call has ended.
    Disconnected,
}

/// Direction of the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    Inbound,
    Outbound,
}

/// Event payload for `registration_state_changed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationStateChanged {
    pub state: RegistrationState,
    /// SIP response code (e.g. 200, 401, 408).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<u16>,
    /// Human-readable reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Event payload for `call_state_changed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallStateChanged {
    pub call_id: String,
    pub state: CallState,
    pub direction: CallDirection,
    /// Remote party display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<String>,
    /// Remote party URI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_uri: Option<String>,
    /// Call duration in seconds (only for Confirmed→Disconnected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    /// SIP response code at disconnect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<u16>,
    /// Disconnect reason text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Event payload for `incoming_call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingCall {
    pub call_id: String,
    /// Caller display name from the From header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_name: Option<String>,
    /// Caller SIP URI.
    pub caller_uri: String,
}

/// Enum of all server-pushed events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    RegistrationStateChanged(RegistrationStateChanged),
    CallStateChanged(CallStateChanged),
    IncomingCall(IncomingCall),
}
