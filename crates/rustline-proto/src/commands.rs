//! Client → Daemon command types.

use serde::{Deserialize, Serialize};

/// Parameters for the `register` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterParams {
    /// SIP server hostname or IP (e.g. "sip.example.com").
    pub server: String,

    /// SIP username / extension.
    pub username: String,

    /// SIP password.
    pub password: String,

    /// SIP domain. Falls back to `server` if omitted.
    #[serde(default)]
    pub domain: Option<String>,

    /// Display name shown in the From header.
    #[serde(default)]
    pub display_name: Option<String>,

    /// SIP transport: "udp" (default), "tcp", or "tls".
    #[serde(default = "default_transport")]
    pub transport: String,

    /// SIP server port. Defaults to 5060 (or 5061 for TLS).
    #[serde(default)]
    pub port: Option<u16>,

    /// REGISTER refresh interval in seconds (default: 300).
    #[serde(default = "default_register_refresh")]
    pub register_refresh: u32,

    /// UDP keep-alive interval in seconds (default: 15).
    #[serde(default = "default_keep_alive")]
    pub keep_alive: u32,
}

fn default_transport() -> String {
    "udp".to_string()
}

fn default_register_refresh() -> u32 {
    300
}

fn default_keep_alive() -> u32 {
    15
}

/// Parameters for the `unregister` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnregisterParams {
    // Currently no parameters; unregisters the active account.
}

/// Parameters for the `dial` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialParams {
    /// The SIP URI or phone number to dial.
    pub target: String,
}

/// Parameters for the `answer` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerParams {
    /// The call ID to answer.
    pub call_id: String,
}

/// Parameters for the `hangup` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HangupParams {
    /// The call ID to hang up.
    pub call_id: String,
}

/// Parameters for the `hold` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldParams {
    /// The call ID to hold/unhold.
    pub call_id: String,
}

/// Parameters for the `mute_mic` command.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MuteMicParams {
    /// Optional call ID (if omitted, applies to active call).
    pub call_id: Option<String>,
    /// Explicit mute state (true = mute, false = unmute). If None, toggles.
    pub muted: Option<bool>,
}

/// Parameters for the `mute_speaker` command.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MuteSpeakerParams {
    /// Optional call ID (if omitted, applies to active call).
    pub call_id: Option<String>,
    /// Explicit mute state (true = mute, false = unmute). If None, toggles.
    pub muted: Option<bool>,
}

/// Parameters for the `dtmf` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DtmfParams {
    /// The call ID.
    pub call_id: String,

    /// DTMF digits to send (e.g. "123#").
    pub digits: String,
}

/// Parameters for the `get_status` query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStatusParams {
    // No parameters — returns current daemon state.
}

/// Enum of all possible commands a client can send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Command {
    Register(RegisterParams),
    Unregister(UnregisterParams),
    Dial(DialParams),
    Answer(AnswerParams),
    Hangup(HangupParams),
    Hold(HoldParams),
    MuteMic(MuteMicParams),
    MuteSpeaker(MuteSpeakerParams),
    Dtmf(DtmfParams),
    GetStatus(GetStatusParams),
}
