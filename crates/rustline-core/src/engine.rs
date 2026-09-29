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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::sip::call::{self, ActiveCall, IncomingInvite};
use crate::sip::message::{SipMessage, SipMethod};
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
    /// Cached registration config for re-registration and calls.
    reg_config: Option<RegisterConfig>,
    /// Cached transport + remote address for the current registration.
    reg_transport: Option<(Arc<SipTransport>, SocketAddr)>,
    /// Currently active calls.
    active_calls: HashMap<String, ActiveCall>,
    /// Pending incoming calls waiting to be answered: call_id -> IncomingInvite.
    pending_incoming: HashMap<String, IncomingInvite>,
    sip_packet_tx: mpsc::Sender<(Vec<u8>, SocketAddr, Option<Arc<SipTransport>>)>,
    sip_packet_rx: mpsc::Receiver<(Vec<u8>, SocketAddr, Option<Arc<SipTransport>>)>,
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
        let (sip_packet_tx, sip_packet_rx) = mpsc::channel(128);

        let handle = EngineHandle {
            cmd_tx,
            event_tx: event_tx.clone(),
        };

        let engine = Engine {
            cmd_rx,
            event_tx,
            sip_packet_tx,
            sip_packet_rx,
            registration: RegistrationState::Unregistered,
            reg_config: None,
            reg_transport: None,
            active_calls: HashMap::new(),
            pending_incoming: HashMap::new(),
        };

        (engine, handle)
    }

    /// Run the engine loop. This future completes when `Shutdown` is received
    /// or all command senders are dropped.
    pub async fn run(mut self) {
        info!("engine started");

        let mut sip_buf = vec![0u8; 4096];

        loop {
            let reg_transport_opt = self.reg_transport.as_ref().map(|(t, addr)| (Arc::clone(t), *addr));

            tokio::select! {
                maybe_cmd = self.cmd_rx.recv() => {
                    match maybe_cmd {
                        Some(cmd) => {
                            let should_break = self.handle_command(cmd).await;
                            if should_break {
                                break;
                            }
                        }
                        None => {
                            info!("all command senders dropped");
                            break;
                        }
                    }
                }

                // Packets received on call_transports (e.g. BYE or re-INVITE from remote party)
                maybe_call_packet = self.sip_packet_rx.recv() => {
                    if let Some((bytes, peer_addr, transport)) = maybe_call_packet {
                        let trans = transport.as_ref().map(Arc::clone);
                        self.handle_incoming_sip_packet(&bytes, peer_addr, trans.as_deref()).await;
                    }
                }

                recv_res = async {
                    match reg_transport_opt {
                        Some((ref t, _)) => {
                            t.recv_from(&mut sip_buf).await
                        }
                        None => {
                            std::future::pending().await
                        }
                    }
                } => {
                    match recv_res {
                        Ok((n, peer_addr)) => {
                            let trans = self.reg_transport.as_ref().map(|(t, _)| Arc::clone(t));
                            self.handle_incoming_sip_packet(&sip_buf[..n], peer_addr, trans.as_deref()).await;
                        }
                        Err(e) => {
                            warn!("SIP recv error on registration transport: {e}");
                        }
                    }
                }
            }
        }

        info!("engine stopped");
    }

    // ── Command dispatch ────────────────────────────────────────────────

    async fn handle_command(&mut self, cmd: CoreCommand) -> bool {
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
                false
            }

            CoreCommand::Unregister { response_tx } => {
                let result = self.handle_unregister().await;
                let _ = response_tx.send(result);
                false
            }

            CoreCommand::Call {
                destination,
                response_tx,
            } => {
                let result = self.handle_call(destination).await;
                let _ = response_tx.send(result);
                false
            }

            CoreCommand::Answer {
                call_id,
                response_tx,
            } => {
                let result = self.handle_answer(call_id.as_deref()).await;
                let _ = response_tx.send(result);
                false
            }

            CoreCommand::Hangup {
                call_id,
                response_tx,
            } => {
                let result = self.handle_hangup(call_id.as_deref()).await;
                let _ = response_tx.send(result);
                false
            }

            CoreCommand::GetStatus { response_tx } => {
                let calls: Vec<CallInfo> = self
                    .active_calls
                    .values()
                    .map(|c| CallInfo {
                        call_id: c.call_id.clone(),
                        remote_party: c.destination.clone(),
                        state: c.state.clone(),
                    })
                    .collect();

                let status = StatusResponse {
                    registration_state: self.registration.clone(),
                    active_calls: calls,
                };
                let _ = response_tx.send(status);
                false
            }

            CoreCommand::Shutdown => {
                info!("shutdown requested");
                // Decline any pending incoming calls
                let pending_ids: Vec<String> = self.pending_incoming.keys().cloned().collect();
                for cid in pending_ids {
                    let _ = self.handle_hangup(Some(&cid)).await;
                }

                // Hangup all active calls
                let call_ids: Vec<String> = self.active_calls.keys().cloned().collect();
                for cid in call_ids {
                    let _ = self.handle_hangup(Some(&cid)).await;
                }

                // Best-effort unregister before exiting
                if matches!(self.registration, RegistrationState::Registered { .. }) {
                    let _ = self.handle_unregister().await;
                }
                true
            }
        }
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
        self.set_registration_state(RegistrationState::Registering);

        let remote_addr = transport::resolve(&server, port)
            .await
            .map_err(|e| format!("DNS resolution failed: {e}"))?;

        let sip_transport = Arc::new(
            transport::create_transport(transport_type)
                .await
                .map_err(|e| format!("transport error: {e}"))?,
        );

        let config = RegisterConfig {
            server: server.clone(),
            port,
            username,
            password,
        };

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

    async fn handle_call(&mut self, destination: String) -> Result<String, String> {
        let (config, (_reg_transport, remote_addr)) = match (&self.reg_config, &self.reg_transport) {
            (Some(c), Some((t, addr))) => (c.clone(), (t.clone(), *addr)),
            _ => return Err("cannot place call: not registered on a SIP server".to_string()),
        };

        info!(destination = %destination, "initiating call from engine");

        let call_transport = Arc::new(
            transport::create_transport(TransportType::Udp)
                .await
                .map_err(|e| format!("failed to create call transport: {e}"))?,
        );

        match call::start_outgoing_call(
            &destination,
            &config,
            &call_transport,
            remote_addr,
            &self.event_tx,
        )
        .await {
            Ok(mut active_call) => {
                let call_id = active_call.call_id.clone();
                let state = active_call.state.clone();

                // Spawn ongoing receiver task on call_transport so BYE and re-INVITE are received!
                let ct = Arc::clone(&call_transport);
                let tx = self.sip_packet_tx.clone();
                let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let stop_clone = Arc::clone(&stop);

                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                        match ct.recv_from(&mut buf).await {
                            Ok((n, peer)) => {
                                let _ = tx.send((buf[..n].to_vec(), peer, Some(Arc::clone(&ct)))).await;
                            }
                            Err(_) => break,
                        }
                    }
                    debug!("call transport receiver task terminated");
                });

                active_call.call_transport = Some(call_transport);
                active_call.call_stop = Some(stop);
                self.active_calls.insert(call_id.clone(), active_call);

                let _ = self.event_tx.send(CoreEvent::CallStateChanged {
                    call_id: call_id.clone(),
                    state,
                });

                Ok(call_id)
            }
            Err(e) => {
                let err_msg = format!("{e}");
                let _ = self.event_tx.send(CoreEvent::Error {
                    call_id: None,
                    message: err_msg.clone(),
                });
                Err(err_msg)
            }
        }
    }

    async fn handle_answer(&mut self, call_id: Option<&str>) -> Result<(), String> {
        let cid = match call_id {
            Some(id) if !id.trim().is_empty() => id.to_string(),
            _ => {
                if self.pending_incoming.len() == 1 {
                    self.pending_incoming.keys().next().unwrap().clone()
                } else if self.pending_incoming.is_empty() {
                    return Err("no incoming calls to answer".to_string());
                } else {
                    return Err("multiple incoming calls; call_id must be specified".to_string());
                }
            }
        };

        let invite = self
            .pending_incoming
            .remove(&cid)
            .ok_or_else(|| format!("incoming call '{cid}' not found or already answered"))?;

        let (config, transport) = match (&self.reg_config, &self.reg_transport) {
            (Some(c), Some((t, _addr))) => (c.clone(), t.clone()),
            _ => return Err("cannot answer call: not registered on a SIP server".to_string()),
        };

        info!(call_id = %cid, "answering incoming call from engine");

        match call::answer_incoming_call(&invite, &config, &transport).await {
            Ok(active_call) => {
                let call_id = active_call.call_id.clone();
                let state = active_call.state.clone();
                self.active_calls.insert(call_id.clone(), active_call);

                let _ = self.event_tx.send(CoreEvent::CallStateChanged {
                    call_id: call_id.clone(),
                    state,
                });

                info!(call_id = %call_id, "incoming call answered successfully");
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("{e}");
                let _ = self.event_tx.send(CoreEvent::Error {
                    call_id: Some(cid.clone()),
                    message: err_msg.clone(),
                });
                Err(err_msg)
            }
        }
    }

    async fn handle_hangup(&mut self, call_id: Option<&str>) -> Result<(), String> {
        let cid = match call_id {
            Some(id) if !id.trim().is_empty() => id.to_string(),
            _ => {
                if self.pending_incoming.len() == 1 {
                    self.pending_incoming.keys().next().unwrap().clone()
                } else if self.active_calls.len() == 1 {
                    self.active_calls.keys().next().unwrap().clone()
                } else if self.pending_incoming.is_empty() && self.active_calls.is_empty() {
                    return Err("no active or pending calls to hang up".to_string());
                } else {
                    return Err("multiple calls in progress; call_id must be specified".to_string());
                }
            }
        };

        // If it's a pending incoming call, reject with 486 Busy Here
        if let Some(invite) = self.pending_incoming.remove(&cid) {
            if let Some((ref transport, _)) = self.reg_transport {
                let mut busy = SipMessage::new_response(486, "Busy Here");
                for via in &invite.via_headers {
                    busy.add_header("Via", via.clone());
                }
                busy.add_header("To", invite.to_header.clone());
                busy.add_header("From", invite.from_header.clone());
                busy.add_header("Call-ID", invite.call_id.clone());
                busy.add_header("CSeq", format!("{} INVITE", invite.cseq));
                busy.add_header("User-Agent", "RustlineCore/0.1");
                let _ = transport.send_to(&busy.to_bytes(), invite.remote_addr).await;
            }
            let _ = self.event_tx.send(CoreEvent::CallStateChanged {
                call_id: cid.clone(),
                state: CallState::Ended,
            });
            info!(call_id = %cid, "incoming call rejected (486 Busy Here)");
            return Ok(());
        }

        let mut active_call = self
            .active_calls
            .remove(&cid)
            .ok_or_else(|| format!("call '{cid}' not found"))?;

        if let Some(config) = &self.reg_config {
            let reg_t = self.reg_transport.as_ref().map(|(t, _)| t.as_ref());
            if let Err(e) = active_call.hangup(config, reg_t).await {
                warn!(call_id = %cid, "error hanging up call: {e}");
            }
        }

        let _ = self.event_tx.send(CoreEvent::CallStateChanged {
            call_id: cid.clone(),
            state: CallState::Ended,
        });

        info!(call_id = %cid, "call hung up successfully");
        Ok(())
    }

    // ── Incoming SIP packet handling ────────────────────────────────────

    async fn handle_incoming_sip_packet(
        &mut self,
        data: &[u8],
        peer_addr: SocketAddr,
        incoming_transport: Option<&SipTransport>,
    ) {
        let msg = match SipMessage::parse(data) {
            Ok(m) => m,
            Err(e) => {
                debug!("failed to parse incoming SIP packet from {peer_addr}: {e}");
                return;
            }
        };

        debug!(peer = %peer_addr, "<<< SIP INCOMING <<<\n{msg}");

        match msg.method() {
            Some(&SipMethod::Invite) => {
                let call_id = match msg.header("Call-ID") {
                    Some(cid) => cid.trim().to_string(),
                    None => return,
                };

                // In-dialog re-INVITE for active call (e.g. direct media or session refresh)
                if let Some(active_call) = self.active_calls.get_mut(&call_id) {
                    info!(call_id = %call_id, "handling in-dialog re-INVITE");
                    let transport = incoming_transport
                        .or_else(|| active_call.call_transport.as_ref().map(|t| t.as_ref()))
                        .or_else(|| self.reg_transport.as_ref().map(|(t, _)| t.as_ref()));

                    if let Some(transport) = transport {
                        let local_addr = transport.local_addr().unwrap_or(peer_addr);
                        let transport_param = transport.transport_param();

                        // Parse SDP if present in re-INVITE to update remote RTP target
                        if !msg.body.is_empty() {
                            if let Ok(remote_sdp) = crate::sip::sdp::SdpSession::parse(&msg.body) {
                                let new_rtp_target = SocketAddr::new(remote_sdp.connection_ip, remote_sdp.media_port);
                                if let Some(ref rtp) = active_call.rtp_stream {
                                    rtp.update_remote_target(new_rtp_target);
                                }
                            }
                        }

                        let mut ok_resp = SipMessage::new_response(200, "OK");
                        for via in msg.headers_all("Via") {
                            ok_resp.add_header("Via", via.to_string());
                        }
                        if let Some(to) = msg.header("To") {
                            ok_resp.add_header("To", to.to_string());
                        }
                        if let Some(from) = msg.header("From") {
                            ok_resp.add_header("From", from.to_string());
                        }
                        ok_resp.add_header("Call-ID", call_id.clone());
                        if let Some(cseq) = msg.header("CSeq") {
                            ok_resp.add_header("CSeq", cseq.to_string());
                        }
                        if let Some(config) = &self.reg_config {
                            ok_resp.add_header(
                                "Contact",
                                format!(
                                    "<sip:{}@{};transport={}>",
                                    config.username,
                                    local_addr,
                                    transport_param.to_ascii_lowercase()
                                ),
                            );
                        }
                        ok_resp.add_header("Allow", "INVITE, ACK, CANCEL, BYE, OPTIONS");
                        ok_resp.add_header("Supported", "replaces, timer");
                        ok_resp.add_header("User-Agent", "RustlineCore/0.1");

                        if let Some(ref rtp) = active_call.rtp_stream {
                            use rand::Rng;
                            let session_id: u64 = rand::thread_rng().gen();
                            let sdp_answer = crate::sip::sdp::SdpSession::build_offer(local_addr.ip(), rtp.local_port, session_id);
                            ok_resp.add_header("Content-Type", "application/sdp");
                            ok_resp.body = sdp_answer;
                        }

                        let _ = transport.send_to(&ok_resp.to_bytes(), peer_addr).await;
                        debug!(call_id = %call_id, "sent 200 OK to in-dialog re-INVITE");
                    }
                    return;
                }

                // Retransmission for ringing call?
                if let Some(invite) = self.pending_incoming.get(&call_id) {
                    debug!(call_id = %call_id, "re-sending 180 Ringing for incoming call");
                    let transport = incoming_transport
                        .or_else(|| self.reg_transport.as_ref().map(|(t, _)| t.as_ref()));
                    if let Some(transport) = transport {
                        let local_addr = transport.local_addr().unwrap_or(peer_addr);
                        let ringing = call::build_ringing_response(invite, local_addr, transport.transport_param());
                        let _ = transport.send_to(&ringing.to_bytes(), peer_addr).await;
                    }
                    return;
                }

                // New incoming INVITE
                if let Some(invite) = IncomingInvite::from_sip_message(&msg, peer_addr) {
                    info!(call_id = %invite.call_id, from = %invite.from_user, "incoming INVITE received");

                    // Send 180 Ringing immediately
                    let transport = incoming_transport
                        .or_else(|| self.reg_transport.as_ref().map(|(t, _)| t.as_ref()));
                    if let Some(transport) = transport {
                        let local_addr = transport.local_addr().unwrap_or(peer_addr);
                        let ringing = call::build_ringing_response(&invite, local_addr, transport.transport_param());
                        let _ = transport.send_to(&ringing.to_bytes(), peer_addr).await;
                    }

                    let call_id = invite.call_id.clone();
                    let from_user = invite.from_user.clone();
                    self.pending_incoming.insert(call_id.clone(), invite);

                    let _ = self.event_tx.send(CoreEvent::IncomingCall {
                        call_id,
                        from: from_user,
                    });
                }
            }

            Some(&SipMethod::Cancel) => {
                let call_id = match msg.header("Call-ID") {
                    Some(cid) => cid.trim().to_string(),
                    None => return,
                };

                if let Some(invite) = self.pending_incoming.remove(&call_id) {
                    info!(call_id = %call_id, "received CANCEL for incoming call");

                    let transport = incoming_transport
                        .or_else(|| self.reg_transport.as_ref().map(|(t, _)| t.as_ref()));
                    if let Some(transport) = transport {
                        let mut cancel_ok = SipMessage::new_response(200, "OK");
                        for via in msg.headers_all("Via") {
                            cancel_ok.add_header("Via", via.to_string());
                        }
                        if let Some(to) = msg.header("To") {
                            cancel_ok.add_header("To", to.to_string());
                        }
                        if let Some(from) = msg.header("From") {
                            cancel_ok.add_header("From", from.to_string());
                        }
                        cancel_ok.add_header("Call-ID", call_id.clone());
                        if let Some(cseq) = msg.header("CSeq") {
                            cancel_ok.add_header("CSeq", cseq.to_string());
                        }
                        cancel_ok.add_header("User-Agent", "RustlineCore/0.1");
                        let _ = transport.send_to(&cancel_ok.to_bytes(), peer_addr).await;

                        let mut term_resp = SipMessage::new_response(487, "Request Terminated");
                        for via in &invite.via_headers {
                            term_resp.add_header("Via", via.clone());
                        }
                        term_resp.add_header("To", invite.to_header.clone());
                        term_resp.add_header("From", invite.from_header.clone());
                        term_resp.add_header("Call-ID", call_id.clone());
                        term_resp.add_header("CSeq", format!("{} INVITE", invite.cseq));
                        term_resp.add_header("User-Agent", "RustlineCore/0.1");
                        let _ = transport.send_to(&term_resp.to_bytes(), invite.remote_addr).await;
                    }

                    let _ = self.event_tx.send(CoreEvent::CallStateChanged {
                        call_id,
                        state: CallState::Ended,
                    });
                }
            }

            Some(&SipMethod::Bye) => {
                let call_id = match msg.header("Call-ID") {
                    Some(cid) => cid.trim().to_string(),
                    None => return,
                };

                if let Some(mut active_call) = self.active_calls.remove(&call_id) {
                    info!(call_id = %call_id, "received remote BYE, call ended");
                    if let Some(ref rtp) = active_call.rtp_stream {
                        rtp.stop();
                    }
                    if let Some(ref stop) = active_call.call_stop {
                        stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    active_call.state = CallState::Ended;

                    let transport = incoming_transport
                        .or_else(|| active_call.call_transport.as_ref().map(|t| t.as_ref()))
                        .or_else(|| self.reg_transport.as_ref().map(|(t, _)| t.as_ref()));

                    if let Some(transport) = transport {
                        let mut ok_resp = SipMessage::new_response(200, "OK");
                        for via in msg.headers_all("Via") {
                            ok_resp.add_header("Via", via.to_string());
                        }
                        if let Some(to) = msg.header("To") {
                            ok_resp.add_header("To", to.to_string());
                        }
                        if let Some(from) = msg.header("From") {
                            ok_resp.add_header("From", from.to_string());
                        }
                        ok_resp.add_header("Call-ID", call_id.clone());
                        if let Some(cseq) = msg.header("CSeq") {
                            ok_resp.add_header("CSeq", cseq.to_string());
                        }
                        ok_resp.add_header("User-Agent", "RustlineCore/0.1");
                        let _ = transport.send_to(&ok_resp.to_bytes(), peer_addr).await;
                    }

                    let _ = self.event_tx.send(CoreEvent::CallStateChanged {
                        call_id,
                        state: CallState::Ended,
                    });
                }
            }

            Some(&SipMethod::Options) => {
                let transport = incoming_transport
                    .or_else(|| self.reg_transport.as_ref().map(|(t, _)| t.as_ref()));
                if let Some(transport) = transport {
                    let mut ok_resp = SipMessage::new_response(200, "OK");
                    for via in msg.headers_all("Via") {
                        ok_resp.add_header("Via", via.to_string());
                    }
                    if let Some(to) = msg.header("To") {
                        ok_resp.add_header("To", to.to_string());
                    }
                    if let Some(from) = msg.header("From") {
                        ok_resp.add_header("From", from.to_string());
                    }
                    if let Some(cid) = msg.header("Call-ID") {
                        ok_resp.add_header("Call-ID", cid.to_string());
                    }
                    if let Some(cseq) = msg.header("CSeq") {
                        ok_resp.add_header("CSeq", cseq.to_string());
                    }
                    ok_resp.add_header("Allow", "INVITE, ACK, CANCEL, BYE, OPTIONS");
                    ok_resp.add_header("User-Agent", "RustlineCore/0.1");
                    let _ = transport.send_to(&ok_resp.to_bytes(), peer_addr).await;
                }
            }

            Some(&SipMethod::Ack) => {
                let call_id = msg.header("Call-ID").map(|c| c.trim()).unwrap_or("");
                debug!(call_id = %call_id, "received ACK for dialog");
            }

            _ => {
                debug!("unhandled incoming SIP method: {:?}", msg.method());
            }
        }
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
            let _ = self.event_tx.send(CoreEvent::RegistrationStateChanged {
                state: new_state,
            });
        }
    }
}
