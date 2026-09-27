//! JSON protocol types — the bridge between WebSocket messages and [`CoreCommand`]/[`CoreEvent`].
//!
//! This module defines the wire format for the control API and provides
//! conversion functions to/from the core's internal types. If the protocol
//! changes (e.g. switching to gRPC), only this module needs rewriting —
//! the engine stays untouched.

use rustline_core::types::*;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

// ═══════════════════════════════════════════════════════════════════════════
//  Client → Daemon  (requests)
// ═══════════════════════════════════════════════════════════════════════════

/// A message sent by a WebSocket client.
#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message after connecting — must contain the auth token.
    Auth {
        token: String,
    },

    /// Register on a SIP server.
    Register {
        /// Client-chosen request ID for correlation.
        #[serde(default)]
        id: Option<String>,
        server: String,
        port: u16,
        username: String,
        password: String,
        /// "udp" or "tls" (default: "udp").
        #[serde(default = "default_transport")]
        transport: String,
    },

    /// Unregister from the SIP server.
    Unregister {
        #[serde(default)]
        id: Option<String>,
    },

    /// Query current status (registration + active calls).
    GetStatus {
        #[serde(default)]
        id: Option<String>,
    },

    /// Place an outgoing call (placeholder — will be implemented in later steps).
    Call {
        #[serde(default)]
        id: Option<String>,
        destination: String,
    },

    /// Answer an incoming call.
    Answer {
        #[serde(default)]
        id: Option<String>,
        call_id: String,
    },

    /// Hang up a call.
    Hangup {
        #[serde(default)]
        id: Option<String>,
        call_id: String,
    },
}

fn default_transport() -> String {
    "udp".into()
}

// ═══════════════════════════════════════════════════════════════════════════
//  Daemon → Client  (responses to individual requests)
// ═══════════════════════════════════════════════════════════════════════════

/// A response sent back to the requesting client.
#[derive(Debug, Serialize)]
pub struct ResponseMessage {
    /// Echoed request ID (if the client supplied one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `true` if the command succeeded.
    pub ok: bool,
    /// Error description (only present when `ok == false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Additional response data (e.g. status snapshot).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ResponseMessage {
    pub fn ok(id: Option<String>) -> Self {
        Self {
            id,
            ok: true,
            error: None,
            data: None,
        }
    }

    pub fn ok_with_data(id: Option<String>, data: serde_json::Value) -> Self {
        Self {
            id,
            ok: true,
            error: None,
            data: Some(data),
        }
    }

    pub fn err(id: Option<String>, error: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            error: Some(error.into()),
            data: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Daemon → All clients  (broadcast events)
// ═══════════════════════════════════════════════════════════════════════════

/// An event pushed to every authenticated client.
#[derive(Debug, Serialize)]
pub struct EventMessage {
    /// Event name (e.g. "registration_state_changed").
    pub event: String,
    /// Event-specific payload fields (flattened into the top-level JSON object).
    #[serde(flatten)]
    pub data: serde_json::Value,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Conversions: protocol ↔ core types
// ═══════════════════════════════════════════════════════════════════════════

/// Parse a transport string into the core enum.
pub fn parse_transport(s: &str) -> Result<TransportType, String> {
    match s.to_ascii_lowercase().as_str() {
        "udp" => Ok(TransportType::Udp),
        "tls" => Ok(TransportType::Tls),
        other => Err(format!("unknown transport: {other} (expected 'udp' or 'tls')")),
    }
}

/// Convert a [`ClientMessage`] into a [`CoreCommand`] (where applicable).
///
/// Returns `None` for `Auth` (handled separately by the session layer).
pub fn client_message_to_command(
    msg: ClientMessage,
) -> Result<Option<(Option<String>, CoreCommand)>, ResponseMessage> {
    match msg {
        ClientMessage::Auth { .. } => Ok(None),

        ClientMessage::Register {
            id,
            server,
            port,
            username,
            password,
            transport,
        } => {
            let transport_type = parse_transport(&transport)
                .map_err(|e| ResponseMessage::err(id.clone(), e))?;
            let (tx, _rx) = oneshot::channel();
            // We'll swap in the real oneshot in the caller.
            Ok(Some((
                id,
                CoreCommand::Register {
                    server,
                    port,
                    username,
                    password,
                    transport: transport_type,
                    response_tx: tx,
                },
            )))
        }

        ClientMessage::Unregister { id } => {
            let (tx, _rx) = oneshot::channel();
            Ok(Some((id, CoreCommand::Unregister { response_tx: tx })))
        }

        ClientMessage::GetStatus { id } => {
            let (tx, _rx) = oneshot::channel();
            Ok(Some((id, CoreCommand::GetStatus { response_tx: tx })))
        }

        ClientMessage::Call { id, .. } => {
            Err(ResponseMessage::err(id, "call command not yet implemented"))
        }
        ClientMessage::Answer { id, .. } => {
            Err(ResponseMessage::err(id, "answer command not yet implemented"))
        }
        ClientMessage::Hangup { id, .. } => {
            Err(ResponseMessage::err(id, "hangup command not yet implemented"))
        }
    }
}

/// Convert a [`CoreEvent`] into a JSON [`EventMessage`].
pub fn core_event_to_message(event: &CoreEvent) -> EventMessage {
    match event {
        CoreEvent::RegistrationStateChanged { state } => {
            let state_str = match state {
                RegistrationState::Unregistered => "unregistered",
                RegistrationState::Registering => "registering",
                RegistrationState::Registered { .. } => "registered",
                RegistrationState::Failed(_) => "failed",
            };

            let mut data = serde_json::json!({ "state": state_str });

            // Add extra fields depending on state variant
            if let RegistrationState::Registered { expires } = state {
                data["expires"] = serde_json::json!(expires);
            }
            if let RegistrationState::Failed(reason) = state {
                data["reason"] = serde_json::json!(reason);
            }

            EventMessage {
                event: "registration_state_changed".into(),
                data,
            }
        }

        CoreEvent::Error { call_id, message } => EventMessage {
            event: "error".into(),
            data: serde_json::json!({
                "call_id": call_id,
                "message": message,
            }),
        },
    }
}

/// Serialize a [`StatusResponse`] to JSON value.
pub fn status_to_json(status: &StatusResponse) -> serde_json::Value {
    let reg_state = match &status.registration_state {
        RegistrationState::Unregistered => "unregistered",
        RegistrationState::Registering => "registering",
        RegistrationState::Registered { .. } => "registered",
        RegistrationState::Failed(_) => "failed",
    };

    let calls: Vec<serde_json::Value> = status
        .active_calls
        .iter()
        .map(|c| {
            serde_json::json!({
                "call_id": c.call_id,
                "remote_party": c.remote_party,
                "state": format!("{:?}", c.state),
            })
        })
        .collect();

    serde_json::json!({
        "registration_state": reg_state,
        "active_calls": calls,
    })
}
