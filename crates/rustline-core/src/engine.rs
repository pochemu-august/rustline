//! Core engine — the central async loop that owns all SIP state.
//!
//! The engine communicates with the outside world exclusively through
//! `tokio::sync` channels:
//!
//! - **Commands** arrive via `mpsc::Receiver<CoreCommand>` (from the control layer).
//! - **Events** are broadcast via `broadcast::Sender<CoreEvent>` (to all subscribers).
//!
//! This means the engine has ZERO knowledge of WebSocket, JSON, or any
//! presentation format. Replacing the control layer (e.g. switching from
//! WebSocket/JSON to gRPC) requires no changes to this module.

use std::net::SocketAddr;

use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::sip::register::{self, RegisterConfig};
use crate::sip::transport::{self, SipTransport};
use crate::types::*;

// ── Engine ──────────────────────────────────────────────────────────────────

/// The core SIP engine.
///
/// Created via [`Engine::new`], which returns both the engine (to be spawned)
/// and an [`EngineHandle`] (to be handed to the control layer).
pub struct Engine {
    cmd_rx: mpsc::Receiver<CoreCommand>,
    event_tx: broadcast::Sender<CoreEvent>,

    // ── Internal state ──
    registration: RegistrationState,
    /// Cached registration config for re-registration.
    reg_config: Option<RegisterConfig>,
    /// Cached transport + remote address for the current registration.
    reg_transport: Option<(SipTransport, SocketAddr)>,
}

/// A cloneable handle for sending commands to the engine and subscribing
/// to its events. This is what the control layer holds.
#[derive(Clone)]
pub struct EngineHandle {
    cmd_tx: mpsc::Sender<CoreCommand>,
    event_tx: broadcast::Sender<CoreEvent>,
}

impl EngineHandle {
    /// Send a command to the engine.
    pub async fn send_command(&self, cmd: CoreCommand) -> Result<(), mpsc::error::SendError<CoreCommand>> {
        self.cmd_tx.send(cmd).await
    }

    /// Subscribe to engine events. Each subscriber gets its own receiver.
    pub fn subscribe_events(&self) -> broadcast::Receiver<CoreEvent> {
        self.event_tx.subscribe()
    }
}

impl Engine {
    /// Create a new engine and its control handle.
    ///
    /// The engine must be spawned (via [`Engine::run`]) on a tokio runtime.
    /// The handle can be cloned and given to one or more control-layer
    /// instances.
    pub fn new() -> (Self, EngineHandle) {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (event_tx, _) = broadcast::channel(256);

        let handle = EngineHandle {
            cmd_tx,
            event_tx: event_tx.clone(),
        };

        let engine = Engine {
            cmd_rx,
            event_tx,
            registration: RegistrationState::Unregistered,
            reg_config: None,
            reg_transport: None,
        };

        (engine, handle)
    }

    /// Run the engine loop. This future completes when `Shutdown` is received
    /// or all command senders are dropped.
    pub async fn run(mut self) {
        info!("engine started");

        while let Some(cmd) = self.cmd_rx.recv().await {
            match cmd {
                CoreCommand::Register {
                    server,
                    port,
                    username,
                    password,
                    transport,
                    response_tx,
                } => {
                    let result = self
                        .handle_register(server, port, username, password, transport)
                        .await;
                    let _ = response_tx.send(result);
                }

                CoreCommand::Unregister { response_tx } => {
                    let result = self.handle_unregister().await;
                    let _ = response_tx.send(result);
                }

                CoreCommand::GetStatus { response_tx } => {
                    let status = StatusResponse {
                        registration_state: self.registration.clone(),
                        active_calls: Vec::new(), // no calls yet
                    };
                    let _ = response_tx.send(status);
                }

                CoreCommand::Shutdown => {
                    info!("shutdown requested");
                    // Best-effort unregister before exiting
                    if matches!(self.registration, RegistrationState::Registered { .. }) {
                        let _ = self.handle_unregister().await;
                    }
                    break;
                }
            }
        }

        info!("engine stopped");
    }

    // ── Command handlers ────────────────────────────────────────────────

    async fn handle_register(
        &mut self,
        server: String,
        port: u16,
        username: String,
        password: String,
        transport_type: TransportType,
    ) -> Result<(), String> {
        // Update state → Registering
        self.set_registration_state(RegistrationState::Registering);

        // Resolve server address
        let remote_addr = transport::resolve(&server, port)
            .await
            .map_err(|e| format!("DNS resolution failed: {e}"))?;

        // Create transport
        let sip_transport = transport::create_transport(transport_type)
            .await
            .map_err(|e| format!("transport error: {e}"))?;

        let config = RegisterConfig {
            server: server.clone(),
            port,
            username,
            password,
        };

        // Execute REGISTER transaction
        match register::do_register(&config, &sip_transport, remote_addr).await {
            Ok(result) => {
                self.set_registration_state(RegistrationState::Registered {
                    expires: result.expires,
                });
                self.reg_config = Some(config);
                self.reg_transport = Some((sip_transport, remote_addr));
                Ok(())
            }
            Err(e) => {
                let msg = format!("{e}");
                self.set_registration_state(RegistrationState::Failed(msg.clone()));
                Err(msg)
            }
        }
    }

    async fn handle_unregister(&mut self) -> Result<(), String> {
        let config = match &self.reg_config {
            Some(c) => c.clone(),
            None => {
                return Err("not registered".to_owned());
            }
        };

        // Try to unregister using existing transport
        if let Some((ref transport, remote_addr)) = self.reg_transport {
            if let Err(e) = register::do_unregister(&config, transport, remote_addr).await {
                warn!("unregister error (non-fatal): {e}");
            }
        }

        self.set_registration_state(RegistrationState::Unregistered);
        self.reg_config = None;
        self.reg_transport = None;
        Ok(())
    }

    // ── State management ────────────────────────────────────────────────

    fn set_registration_state(&mut self, new_state: RegistrationState) {
        if self.registration != new_state {
            info!(
                from = ?self.registration,
                to = ?new_state,
                "registration state changed"
            );
            self.registration = new_state.clone();
            // Broadcast to all subscribers (ignore "no receivers" error)
            let _ = self.event_tx.send(CoreEvent::RegistrationStateChanged {
                state: new_state,
            });
        }
    }
}
