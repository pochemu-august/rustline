//! Shared types for communication between the core engine and any control layer.
//!
//! The control layer (WebSocket/JSON, gRPC, etc.) translates external commands
//! into [`CoreCommand`] variants and delivers [`CoreEvent`] notifications back
//! to connected clients. The core engine never touches transport-specific
//! serialization — all communication goes through `tokio::sync` channels.

use tokio::sync::oneshot;

// ── Registration ────────────────────────────────────────────────────────────

/// SIP transport selection for the signalling channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportType {
    Udp,
    Tls,
}

/// Lifecycle states of SIP registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationState {
    /// No registration attempted or previously unregistered.
    Unregistered,
    /// REGISTER transaction is in progress.
    Registering,
    /// Successfully registered (200 OK received, `expires` seconds left).
    Registered { expires: u32 },
    /// Registration failed with the given reason.
    Failed(String),
}

// ── Calls (placeholder for later steps) ─────────────────────────────────────

/// Lifecycle states of a single call leg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallState {
    /// INVITE sent, waiting for provisional response.
    Calling,
    /// 180 Ringing received (outgoing) or INVITE received (incoming).
    Ringing,
    /// 200 OK + ACK exchanged, media flowing.
    Active,
    /// Call terminated (BYE or CANCEL).
    Ended,
}

/// Minimal info about an active call.
#[derive(Debug, Clone)]
pub struct CallInfo {
    pub call_id: String,
    pub remote_party: String,
    pub state: CallState,
}

// ── Status snapshot ─────────────────────────────────────────────────────────

/// Response payload for `GetStatus` command.
#[derive(Debug, Clone)]
pub struct StatusResponse {
    pub registration_state: RegistrationState,
    pub active_calls: Vec<CallInfo>,
}

// ── Commands (control layer → core) ─────────────────────────────────────────

/// A command sent by the control layer to the core engine.
///
/// Each command that expects a direct reply carries a `oneshot::Sender` so the
/// caller can `await` the result without polling.
pub enum CoreCommand {
    /// Register on a SIP server.
    Register {
        server: String,
        port: u16,
        username: String,
        password: String,
        transport: TransportType,
        response_tx: oneshot::Sender<Result<(), String>>,
    },

    /// Unregister (send REGISTER with Expires: 0).
    Unregister {
        response_tx: oneshot::Sender<Result<(), String>>,
    },

    /// Query current status (registration + active calls).
    GetStatus {
        response_tx: oneshot::Sender<StatusResponse>,
    },

    /// Gracefully shut down the engine loop.
    Shutdown,

    // ── Call-related commands (will be added in later steps) ──
    // Call { destination, response_tx }
    // Answer { call_id, response_tx }
    // Hangup { call_id, response_tx }
}

// ── Events (core → control layer) ──────────────────────────────────────────

/// An event emitted by the core engine to all subscribed control-layer clients.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    /// Registration state transitioned.
    RegistrationStateChanged {
        state: RegistrationState,
    },

    /// An error occurred (optionally tied to a specific call).
    Error {
        call_id: Option<String>,
        message: String,
    },

    // ── Call-related events (later steps) ──
    // IncomingCall { call_id, from }
    // CallStateChanged { call_id, state }
}
