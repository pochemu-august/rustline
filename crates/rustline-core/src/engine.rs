//! The core SIP engine.
//!
//! Owns the account state, active calls, and dispatches commands
//! from the daemon layer to the appropriate state machines.
//!
//! For now, the engine returns mock responses. In the future, it will
//! drive real SIP transactions via `rsip` and a UDP/TCP transport.

use std::collections::HashMap;

use rustline_proto::{
    commands::*,
    events::*,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::account::{AccountConfig, AccountState};
use crate::call::Call;

/// Messages sent from the engine to the daemon (for broadcasting to clients).
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A protocol event to broadcast to all connected WebSocket clients.
    Broadcast(Event),
}

/// The core SIP engine.
pub struct Engine {
    /// Current account state.
    account: AccountState,

    /// Active calls, keyed by call ID.
    calls: HashMap<String, Call>,

    /// Channel to send events back to the daemon.
    event_tx: mpsc::UnboundedSender<EngineEvent>,
}

impl Engine {
    /// Create a new engine with an event channel.
    pub fn new(event_tx: mpsc::UnboundedSender<EngineEvent>) -> Self {
        Self {
            account: AccountState::default(),
            calls: HashMap::new(),
            event_tx,
        }
    }

    /// Handle a `register` command.
    pub async fn handle_register(&mut self, params: RegisterParams) -> Result<serde_json::Value, String> {
        info!(
            server = %params.server,
            username = %params.username,
            "Registering SIP account"
        );

        let mut config = AccountConfig {
            server: params.server,
            username: params.username,
            password: params.password,
            domain: params.domain.unwrap_or_default(),
            display_name: params.display_name.unwrap_or_default(),
            transport: params.transport,
            port: params.port.unwrap_or(0),
            register_refresh: params.register_refresh,
            keep_alive: params.keep_alive,
        };
        config.sanitize();

        // Transition: Unregistered → Registering
        self.account.start_registering(config);

        // Emit "registering" event
        let _ = self.event_tx.send(EngineEvent::Broadcast(
            Event::RegistrationStateChanged(RegistrationStateChanged {
                state: RegistrationState::Registering,
                code: None,
                reason: None,
            }),
        ));

        // TODO: In the future, send a real SIP REGISTER via rsip transport.
        //       For now, simulate immediate success after a short delay.

        // Transition: Registering → Registered (mock)
        self.account.mark_registered();

        let _ = self.event_tx.send(EngineEvent::Broadcast(
            Event::RegistrationStateChanged(RegistrationStateChanged {
                state: RegistrationState::Registered,
                code: Some(200),
                reason: Some("OK".to_string()),
            }),
        ));

        Ok(serde_json::json!({
            "status": "registered",
            "message": "SIP account registered successfully (mock)"
        }))
    }

    /// Handle an `unregister` command.
    pub async fn handle_unregister(&mut self) -> Result<serde_json::Value, String> {
        info!("Unregistering SIP account");

        self.account.mark_unregistered();

        let _ = self.event_tx.send(EngineEvent::Broadcast(
            Event::RegistrationStateChanged(RegistrationStateChanged {
                state: RegistrationState::Unregistered,
                code: None,
                reason: None,
            }),
        ));

        Ok(serde_json::json!({
            "status": "unregistered"
        }))
    }

    /// Handle a `dial` command.
    pub async fn handle_dial(&mut self, params: DialParams) -> Result<serde_json::Value, String> {
        if self.account.registration != RegistrationState::Registered {
            return Err("Not registered. Call `register` first.".to_string());
        }

        let call = Call::new_outgoing(&params.target);
        let call_id = call.id.clone();
        info!(call_id = %call_id, target = %params.target, "Initiating outgoing call");

        // Emit call_state_changed → Calling
        let _ = self.event_tx.send(EngineEvent::Broadcast(
            Event::CallStateChanged(CallStateChanged {
                call_id: call_id.clone(),
                state: CallState::Calling,
                direction: CallDirection::Outbound,
                remote_name: None,
                remote_uri: Some(params.target),
                duration_secs: None,
                code: None,
                reason: None,
            }),
        ));

        self.calls.insert(call_id.clone(), call);

        Ok(serde_json::json!({
            "call_id": call_id,
            "status": "calling"
        }))
    }

    /// Handle an `answer` command.
    pub async fn handle_answer(&mut self, params: AnswerParams) -> Result<serde_json::Value, String> {
        let call = self.calls.get_mut(&params.call_id)
            .ok_or_else(|| format!("Call {} not found", params.call_id))?;

        if call.state != CallState::Incoming {
            return Err(format!("Call {} is not in Incoming state", params.call_id));
        }

        call.transition(CallState::Connecting);
        call.transition(CallState::Confirmed);

        let _ = self.event_tx.send(EngineEvent::Broadcast(
            Event::CallStateChanged(CallStateChanged {
                call_id: params.call_id.clone(),
                state: CallState::Confirmed,
                direction: call.direction,
                remote_name: call.remote_name.clone(),
                remote_uri: Some(call.remote_uri.clone()),
                duration_secs: None,
                code: Some(200),
                reason: Some("OK".to_string()),
            }),
        ));

        Ok(serde_json::json!({
            "call_id": params.call_id,
            "status": "answered"
        }))
    }

    /// Handle a `hangup` command.
    pub async fn handle_hangup(&mut self, params: HangupParams) -> Result<serde_json::Value, String> {
        let call = self.calls.get_mut(&params.call_id)
            .ok_or_else(|| format!("Call {} not found", params.call_id))?;

        call.transition(CallState::Disconnected);

        let _ = self.event_tx.send(EngineEvent::Broadcast(
            Event::CallStateChanged(CallStateChanged {
                call_id: params.call_id.clone(),
                state: CallState::Disconnected,
                direction: call.direction,
                remote_name: call.remote_name.clone(),
                remote_uri: Some(call.remote_uri.clone()),
                duration_secs: Some(call.duration_secs),
                code: Some(200),
                reason: Some("Normal call clearing".to_string()),
            }),
        ));

        // Remove terminated calls
        self.calls.retain(|_, c| !c.is_terminated());

        Ok(serde_json::json!({
            "call_id": params.call_id,
            "status": "disconnected"
        }))
    }

    /// Handle a `get_status` query.
    pub async fn handle_get_status(&self) -> Result<serde_json::Value, String> {
        let calls: Vec<_> = self.calls.values().collect();
        Ok(serde_json::json!({
            "registration": self.account.registration,
            "active_calls": calls.len(),
            "calls": calls,
        }))
    }

    /// Handle a `hold` command (stub).
    pub async fn handle_hold(&mut self, params: HoldParams) -> Result<serde_json::Value, String> {
        let call = self.calls.get_mut(&params.call_id)
            .ok_or_else(|| format!("Call {} not found", params.call_id))?;

        let new_state = if call.state == CallState::Held {
            CallState::Confirmed
        } else {
            CallState::Held
        };

        if !call.transition(new_state) {
            return Err(format!("Cannot hold/unhold call in state {:?}", call.state));
        }

        let _ = self.event_tx.send(EngineEvent::Broadcast(
            Event::CallStateChanged(CallStateChanged {
                call_id: params.call_id.clone(),
                state: call.state,
                direction: call.direction,
                remote_name: call.remote_name.clone(),
                remote_uri: Some(call.remote_uri.clone()),
                duration_secs: None,
                code: None,
                reason: None,
            }),
        ));

        Ok(serde_json::json!({
            "call_id": params.call_id,
            "status": format!("{:?}", call.state).to_lowercase()
        }))
    }

    /// Handle a `dtmf` command (stub).
    pub async fn handle_dtmf(&mut self, params: DtmfParams) -> Result<serde_json::Value, String> {
        let call = self.calls.get(&params.call_id)
            .ok_or_else(|| format!("Call {} not found", params.call_id))?;

        if !call.is_active() {
            return Err(format!("Call {} is not active", params.call_id));
        }

        warn!(call_id = %params.call_id, digits = %params.digits, "DTMF sending not yet implemented");

        Ok(serde_json::json!({
            "call_id": params.call_id,
            "digits": params.digits,
            "status": "sent (mock)"
        }))
    }
}
