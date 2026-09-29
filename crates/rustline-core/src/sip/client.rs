//! High-level SIP Client handling Registration, Calls (INVITE/ACK/BYE), Digest Auth, and Keep-Alive.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use rsip::message::HasHeaders;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

use super::auth::{DigestChallenge, calculate_authorization};
use super::dialog::{SipDialog, build_sdp, parse_sdp_audio_endpoint, rand_u32};
use super::transport::SipTransport;
use crate::account::AccountConfig;
use crate::engine::EngineEvent;
use rustline_proto::events::{CallDirection, CallState, CallStateChanged, Event, IncomingCall};

/// Active media session tuple: (RTP session, optional OS audio hardware engine).
pub type CallMediaSession = (
    rustline_media::RtpSession,
    Option<rustline_media::AudioEngine>,
);
type ActiveMediaMap = Arc<Mutex<HashMap<String, CallMediaSession>>>;

/// High-level SIP client.
pub struct SipClient {
    config: AccountConfig,
    transport: SipTransport,
    registration_call_id: String,
    from_tag: String,
    cseq: AtomicU32,
    auth_nc: AtomicU32,
    dialogs: Arc<Mutex<HashMap<String, SipDialog>>>,
    response_waiters: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<rsip::Response>>>>,
    active_rtp: ActiveMediaMap,
    engine_tx: mpsc::UnboundedSender<EngineEvent>,
}

impl SipClient {
    /// Create and initialize a new SIP client with an active receiver loop.
    pub async fn new(
        config: AccountConfig,
        engine_tx: mpsc::UnboundedSender<EngineEvent>,
    ) -> Result<Arc<Self>> {
        let port = if config.port == 0 { 5060 } else { config.port };
        let transport = SipTransport::new(&config.server, port).await?;
        let registration_call_id = format!("{}@{}", Uuid::new_v4(), transport.local_ip());
        let from_tag = format!("{:x}", rand_u32());

        let client = Arc::new(Self {
            config,
            transport,
            registration_call_id,
            from_tag,
            cseq: AtomicU32::new(1),
            auth_nc: AtomicU32::new(1),
            dialogs: Arc::new(Mutex::new(HashMap::new())),
            response_waiters: Arc::new(Mutex::new(HashMap::new())),
            active_rtp: Arc::new(Mutex::new(HashMap::new())),
            engine_tx,
        });

        // Spawn background listener task on the UDP socket
        client.clone().spawn_listen_loop();

        Ok(client)
    }

    /// Access the underlying transport.
    pub fn transport(&self) -> &SipTransport {
        &self.transport
    }

    /// Start RTP media streaming and CPAL audio hardware pipeline for a call.
    async fn start_media_session(&self, call_id: &str, remote_rtp: Option<std::net::SocketAddr>) {
        let local_ip = self.transport.local_ip().to_string();
        let local_rtp_port = 10030;

        info!(%call_id, ?remote_rtp, "Starting media pipeline (RTP + CPAL Audio)");

        let playback_buf = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        match rustline_media::RtpSession::start(
            &local_ip,
            local_rtp_port,
            remote_rtp,
            Arc::clone(&playback_buf),
        )
        .await
        {
            Ok((rtp_session, mic_tx)) => {
                let audio_engine = if rustline_media::is_available() {
                    Some(rustline_media::AudioEngine::new(mic_tx, playback_buf))
                } else {
                    warn!("Audio hardware not available, running RTP in headless mode");
                    None
                };

                let mut rtp_map = self.active_rtp.lock().await;
                rtp_map.insert(call_id.to_string(), (rtp_session, audio_engine));
            }
            Err(e) => {
                error!(%call_id, "Failed to start RTP session: {}", e);
            }
        }
    }

    /// Stop and clean up RTP media streaming and CPAL audio hardware pipeline for a call.
    async fn stop_media_session(&self, call_id: &str) {
        let mut rtp_map = self.active_rtp.lock().await;
        if let Some((mut rtp_session, audio_engine)) = rtp_map.remove(call_id) {
            info!(%call_id, "Stopping media pipeline");
            rtp_session.stop();
            drop(audio_engine);
        }
    }

    /// Update destination remote RTP address (e.g. from re-INVITE SDP).
    async fn update_media_remote(&self, call_id: &str, new_remote: std::net::SocketAddr) {
        let rtp_map = self.active_rtp.lock().await;
        if let Some((rtp_session, _)) = rtp_map.get(call_id) {
            rtp_session.set_remote_addr(new_remote).await;
        }
    }

    /// Spawn the continuous background UDP packet listener.
    fn spawn_listen_loop(self: Arc<Self>) {
        let socket = self.transport.socket().clone();
        let client = self.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            debug!(
                "Started SIP background listener loop on {}",
                socket.local_addr().unwrap()
            );

            loop {
                let (len, src_addr) = match socket.recv_from(&mut buf).await {
                    Ok(res) => res,
                    Err(e) => {
                        error!("SIP UDP recv error: {}", e);
                        break;
                    }
                };

                // Ignore CRLF keep-alive responses
                if len <= 4 && buf[..len].iter().all(|&b| b == b'\r' || b == b'\n') {
                    continue;
                }

                let raw_bytes = &buf[..len];
                trace!(
                    "Incoming SIP packet from {}:\n{}",
                    src_addr,
                    String::from_utf8_lossy(raw_bytes)
                );

                let msg = match rsip::SipMessage::try_from(raw_bytes) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("Failed to parse SIP packet: {}", e);
                        continue;
                    }
                };

                client.handle_incoming_message(msg, src_addr).await;
            }
        });
    }

    /// Handle an incoming SIP message (Request or Response).
    async fn handle_incoming_message(&self, msg: rsip::SipMessage, src_addr: SocketAddr) {
        match msg {
            rsip::SipMessage::Response(resp) => {
                let call_id = extract_header_val(resp.headers(), "Call-ID");
                debug!(
                    status = resp.status_code.code(),
                    ?call_id,
                    "Received SIP Response"
                );

                if let Some(cid) = call_id {
                    let waiters = self.response_waiters.lock().await;
                    if let Some(tx) = waiters.get(&cid) {
                        let _ = tx.send(resp);
                    }
                }
            }
            rsip::SipMessage::Request(req) => {
                self.handle_incoming_request(req, src_addr).await;
            }
        }
    }

    /// Handle incoming SIP Requests from Asterisk (OPTIONS, INVITE, BYE, ACK, CANCEL).
    async fn handle_incoming_request(&self, req: rsip::Request, src_addr: SocketAddr) {
        let method = req.method();
        let headers = req.headers();
        let call_id = extract_header_val(headers, "Call-ID").unwrap_or_default();
        let cseq_str = extract_header_val(headers, "CSeq").unwrap_or_default();
        let from_str = extract_header_val(headers, "From").unwrap_or_default();
        let to_str = extract_header_val(headers, "To").unwrap_or_default();
        let via_str = extract_header_val(headers, "Via").unwrap_or_default();

        debug!(?method, %call_id, %cseq_str, "Received SIP Request from {}", src_addr);

        match method {
            // Asterisk Qualify / Keep-Alive Ping
            rsip::Method::Options => {
                debug!("Replying 200 OK to OPTIONS qualify ping");
                let reply = format!(
                    "SIP/2.0 200 OK\r\n\
                    Via: {via_str}\r\n\
                    From: {from_str}\r\n\
                    To: {to_str};tag={local_tag}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: {cseq_str}\r\n\
                    User-Agent: rustline/0.1.0\r\n\
                    Allow: PRACK, INVITE, ACK, BYE, CANCEL, UPDATE, INFO, SUBSCRIBE, NOTIFY, REFER, MESSAGE, OPTIONS\r\n\
                    Content-Length: 0\r\n\r\n",
                    local_tag = self.from_tag,
                );
                let _ = self.transport.send_to(reply.as_bytes(), src_addr).await;
            }

            // Incoming Call (e.g. from 101 to 100) or in-dialog re-INVITE
            rsip::Method::Invite => {
                let is_reinvite = {
                    let dialogs = self.dialogs.lock().await;
                    dialogs.contains_key(&call_id)
                };

                if is_reinvite {
                    debug!(%call_id, %cseq_str, "Received in-dialog re-INVITE from Asterisk");
                    let cseq_num = parse_cseq_num(&cseq_str);
                    {
                        let mut dialogs = self.dialogs.lock().await;
                        if let Some(d) = dialogs.get_mut(&call_id) {
                            d.remote_cseq = cseq_num;
                            d.incoming_via = Some(via_str.clone());
                            d.incoming_from = Some(from_str.clone());
                            d.incoming_to = Some(to_str.clone());
                        }
                    }

                    if let Some(new_rtp) = parse_sdp_audio_endpoint(req.body()) {
                        self.update_media_remote(&call_id, new_rtp).await;
                    }

                    // Directly respond with 200 OK + SDP (in-dialog To header already has our local tag)
                    let local_ip = self.transport.local_ip().to_string();
                    let local_port = self.transport.local_port();
                    let sdp = build_sdp(&local_ip, 10030);
                    let sdp_len = sdp.len();

                    let reply = format!(
                        "SIP/2.0 200 OK\r\n\
                        Via: {via_str}\r\n\
                        From: {from_str}\r\n\
                        To: {to_str}\r\n\
                        Call-ID: {call_id}\r\n\
                        CSeq: {cseq_str}\r\n\
                        Contact: <sip:{username}@{local_ip}:{local_port};transport=udp>\r\n\
                        User-Agent: rustline/0.1.0\r\n\
                        Content-Type: application/sdp\r\n\
                        Content-Length: {sdp_len}\r\n\r\n\
                        {sdp}",
                        username = self.config.username,
                    );
                    let _ = self.transport.send_to(reply.as_bytes(), src_addr).await;
                    return;
                }

                let caller_uri = extract_uri_from_from(&from_str);
                let caller_name = extract_display_name_from_from(&from_str);
                let remote_tag = extract_tag_from_header(&from_str);
                let contact_str = extract_header_val(headers, "Contact");
                let remote_contact = contact_str.as_deref().map(extract_contact_uri);
                let remote_rtp = parse_sdp_audio_endpoint(req.body());
                let cseq_num = parse_cseq_num(&cseq_str);

                info!(
                    caller = %caller_uri,
                    call_id = %call_id,
                    ?remote_rtp,
                    "📞 Incoming SIP call from Asterisk!"
                );

                let local_ip = self.transport.local_ip().to_string();
                let local_port = self.transport.local_port();

                let mut dialog = SipDialog::new_inbound(
                    call_id.clone(),
                    caller_uri.clone(),
                    caller_name.clone(),
                    remote_tag,
                    cseq_num,
                    via_str.clone(),
                    from_str.clone(),
                    to_str.clone(),
                );
                dialog.remote_contact = remote_contact;
                dialog.remote_rtp_addr = remote_rtp;
                let local_tag = dialog.local_tag.clone();

                {
                    let mut dialogs = self.dialogs.lock().await;
                    dialogs.insert(call_id.clone(), dialog);
                }

                // 1. Send 100 Trying
                let trying = format!(
                    "SIP/2.0 100 Trying\r\n\
                    Via: {via_str}\r\n\
                    From: {from_str}\r\n\
                    To: {to_str}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: {cseq_str}\r\n\
                    Content-Length: 0\r\n\r\n"
                );
                let _ = self.transport.send_to(trying.as_bytes(), src_addr).await;

                // 2. Send 180 Ringing
                let ringing = format!(
                    "SIP/2.0 180 Ringing\r\n\
                    Via: {via_str}\r\n\
                    From: {from_str}\r\n\
                    To: {to_str};tag={local_tag}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: {cseq_str}\r\n\
                    Contact: <sip:{username}@{local_ip}:{local_port};transport=udp>\r\n\
                    User-Agent: rustline/0.1.0\r\n\
                    Content-Length: 0\r\n\r\n",
                    username = self.config.username,
                );
                let _ = self.transport.send_to(ringing.as_bytes(), src_addr).await;

                // 3. Notify Engine & UI
                let _ = self
                    .engine_tx
                    .send(EngineEvent::Broadcast(Event::IncomingCall(IncomingCall {
                        call_id: call_id.clone(),
                        caller_uri: caller_uri.clone(),
                        caller_name,
                    })));

                let _ = self
                    .engine_tx
                    .send(EngineEvent::Broadcast(Event::CallStateChanged(
                        CallStateChanged {
                            call_id,
                            state: CallState::Incoming,
                            direction: CallDirection::Inbound,
                            remote_name: None,
                            remote_uri: Some(caller_uri),
                            duration_secs: None,
                            code: None,
                            reason: None,
                        },
                    )));
            }

            // Remote hangup
            rsip::Method::Bye => {
                info!(call_id = %call_id, "Remote party hung up (BYE received)");
                self.stop_media_session(&call_id).await;

                let reply = format!(
                    "SIP/2.0 200 OK\r\n\
                    Via: {via_str}\r\n\
                    From: {from_str}\r\n\
                    To: {to_str}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: {cseq_str}\r\n\
                    Content-Length: 0\r\n\r\n"
                );
                let _ = self.transport.send_to(reply.as_bytes(), src_addr).await;

                let mut dialogs = self.dialogs.lock().await;
                let removed = dialogs.remove(&call_id);

                let _ = self
                    .engine_tx
                    .send(EngineEvent::Broadcast(Event::CallStateChanged(
                        CallStateChanged {
                            call_id: call_id.clone(),
                            state: CallState::Disconnected,
                            direction: if removed.map(|d| d.is_inbound).unwrap_or(true) {
                                CallDirection::Inbound
                            } else {
                                CallDirection::Outbound
                            },
                            remote_name: None,
                            remote_uri: None,
                            duration_secs: None,
                            code: Some(200),
                            reason: Some("Normal clearing".to_string()),
                        },
                    )));
            }

            // Remote cancelled call before answer
            rsip::Method::Cancel => {
                info!(call_id = %call_id, "Call cancelled by remote (CANCEL received)");
                self.stop_media_session(&call_id).await;

                // 200 OK for CANCEL
                let cancel_ok = format!(
                    "SIP/2.0 200 OK\r\n\
                    Via: {via_str}\r\n\
                    From: {from_str}\r\n\
                    To: {to_str}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: {cseq_str}\r\n\
                    Content-Length: 0\r\n\r\n"
                );
                let _ = self.transport.send_to(cancel_ok.as_bytes(), src_addr).await;

                // 487 Request Terminated for INVITE
                let req_term = format!(
                    "SIP/2.0 487 Request Terminated\r\n\
                    Via: {via_str}\r\n\
                    From: {from_str}\r\n\
                    To: {to_str}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: {cseq_str}\r\n\
                    Content-Length: 0\r\n\r\n"
                );
                let _ = self.transport.send_to(req_term.as_bytes(), src_addr).await;

                let mut dialogs = self.dialogs.lock().await;
                dialogs.remove(&call_id);

                let _ = self
                    .engine_tx
                    .send(EngineEvent::Broadcast(Event::CallStateChanged(
                        CallStateChanged {
                            call_id,
                            state: CallState::Disconnected,
                            direction: CallDirection::Inbound,
                            remote_name: None,
                            remote_uri: None,
                            duration_secs: None,
                            code: Some(487),
                            reason: Some("Request Terminated".to_string()),
                        },
                    )));
            }

            // Call confirmed by ACK
            rsip::Method::Ack => {
                debug!(call_id = %call_id, "ACK received, call confirmed");
            }

            _ => {
                trace!(?method, "Unhandled SIP request method");
            }
        }
    }

    /// Execute the full SIP registration transaction (RFC 3261 + RFC 2617 Digest Auth).
    pub async fn register(&self) -> Result<u32> {
        let domain = if self.config.domain.is_empty() {
            &self.config.server
        } else {
            &self.config.domain
        };

        let server_port = if self.config.port == 0 {
            5060
        } else {
            self.config.port
        };
        let request_uri = format!("sip:{}:{}", self.config.server, server_port);
        let expires = self.config.register_refresh;

        let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
        {
            let mut waiters = self.response_waiters.lock().await;
            waiters.insert(self.registration_call_id.clone(), resp_tx);
        }

        // Send initial REGISTER (without Authorization)
        let initial_cseq = self.cseq.fetch_add(1, Ordering::SeqCst);
        let branch = format!("z9hG4bK-{}", rand_u32());
        let initial_req =
            self.build_register_request(&request_uri, domain, initial_cseq, &branch, expires, None);

        info!(
            server = %self.config.server,
            username = %self.config.username,
            cseq = initial_cseq,
            "Sending initial SIP REGISTER to Asterisk"
        );

        self.transport.send_raw(initial_req.as_bytes()).await?;

        // Wait for response (401 or 200)
        let response = match tokio::time::timeout(Duration::from_secs(5), resp_rx.recv()).await {
            Ok(Some(res)) => res,
            _ => {
                self.cleanup_waiter(&self.registration_call_id).await;
                return Err(anyhow!(
                    "SIP register timed out waiting for server response"
                ));
            }
        };

        let status_code = response.status_code.code();
        if status_code == 200 {
            self.cleanup_waiter(&self.registration_call_id).await;
            info!("SIP registration succeeded on first attempt (200 OK)");
            return Ok(expires);
        }

        if status_code != 401 && status_code != 407 {
            self.cleanup_waiter(&self.registration_call_id).await;
            return Err(anyhow!(
                "SIP registration rejected with status {}: {}",
                status_code,
                response.status_code
            ));
        }

        // Parse challenge
        let challenge = extract_challenge(&response)?
            .ok_or_else(|| anyhow!("401 response missing WWW-Authenticate header"))?;

        let nc = self.auth_nc.fetch_add(1, Ordering::SeqCst);
        let cnonce = format!("{:08x}", rand_u32());
        let auth_header = calculate_authorization(
            &self.config.username,
            &self.config.password,
            "REGISTER",
            &request_uri,
            &challenge,
            nc,
            &cnonce,
        );

        // Send authenticated REGISTER
        let auth_cseq = self.cseq.fetch_add(1, Ordering::SeqCst);
        let auth_branch = format!("z9hG4bK-{}", rand_u32());
        let auth_req = self.build_register_request(
            &request_uri,
            domain,
            auth_cseq,
            &auth_branch,
            expires,
            Some(&auth_header),
        );

        info!(
            server = %self.config.server,
            username = %self.config.username,
            cseq = auth_cseq,
            "Sending authenticated SIP REGISTER with Digest credentials"
        );

        self.transport.send_raw(auth_req.as_bytes()).await?;

        // Wait for final 200 OK
        let final_resp = match tokio::time::timeout(Duration::from_secs(5), resp_rx.recv()).await {
            Ok(Some(res)) => res,
            _ => {
                self.cleanup_waiter(&self.registration_call_id).await;
                return Err(anyhow!("SIP authenticated register timed out"));
            }
        };

        self.cleanup_waiter(&self.registration_call_id).await;

        let final_status = final_resp.status_code.code();
        if final_status == 200 {
            info!(
                username = %self.config.username,
                server = %self.config.server,
                "✅ Successfully registered on Asterisk PBX (200 OK)!"
            );
            Ok(expires)
        } else {
            Err(anyhow!(
                "SIP registration failed after authentication: {} {}",
                final_status,
                final_resp.status_code
            ))
        }
    }

    /// Initiate an outgoing call to `target` (e.g. "101").
    pub async fn dial(self: &Arc<Self>, target: &str) -> Result<String> {
        let domain = if self.config.domain.is_empty() {
            &self.config.server
        } else {
            &self.config.domain
        };
        let local_ip = self.transport.local_ip().to_string();
        let local_port = self.transport.local_port();

        let dialog = SipDialog::new_outbound(target, &local_ip);
        let call_id = dialog.call_id.clone();
        let local_tag = dialog.local_tag.clone();
        let target_str = target.to_string();
        let domain_str = domain.to_string();

        {
            let mut dialogs = self.dialogs.lock().await;
            dialogs.insert(call_id.clone(), dialog);
        }

        let (resp_tx, resp_rx) = mpsc::unbounded_channel();
        {
            let mut waiters = self.response_waiters.lock().await;
            waiters.insert(call_id.clone(), resp_tx);
        }

        // Spawn background task to drive the outgoing call transaction
        let client = self.clone();
        let cid = call_id.clone();
        tokio::spawn(async move {
            if let Err(e) = client
                .run_outbound_call_flow(
                    &cid,
                    &local_tag,
                    &target_str,
                    &domain_str,
                    &local_ip,
                    local_port,
                    resp_rx,
                )
                .await
            {
                warn!(call_id = %cid, "Outbound call failed: {}", e);
                let _ = client
                    .engine_tx
                    .send(EngineEvent::Broadcast(Event::CallStateChanged(
                        CallStateChanged {
                            call_id: cid.clone(),
                            state: CallState::Disconnected,
                            direction: CallDirection::Outbound,
                            remote_name: None,
                            remote_uri: Some(target_str),
                            duration_secs: None,
                            code: Some(500),
                            reason: Some(e.to_string()),
                        },
                    )));
                let mut dialogs = client.dialogs.lock().await;
                dialogs.remove(&cid);
            }
            client.cleanup_waiter(&cid).await;
        });

        Ok(call_id)
    }

    /// Drives the outgoing INVITE handshake with Asterisk.
    #[allow(clippy::too_many_arguments)]
    async fn run_outbound_call_flow(
        &self,
        call_id: &str,
        local_tag: &str,
        target: &str,
        domain: &str,
        local_ip: &str,
        local_port: u16,
        mut resp_rx: mpsc::UnboundedReceiver<rsip::Response>,
    ) -> Result<()> {
        let server_port = if self.config.port == 0 {
            5060
        } else {
            self.config.port
        };
        let request_uri = format!("sip:{}@{}:{}", target, domain, server_port);
        let sdp = build_sdp(local_ip, 10030);

        // Step 1: Send initial INVITE
        let branch = format!("z9hG4bK-{}", rand_u32());
        let initial_invite = self.build_invite_request(
            &request_uri,
            target,
            domain,
            call_id,
            local_tag,
            100,
            &branch,
            &sdp,
            None,
        );

        info!(%target, %call_id, "Sending SIP INVITE to Asterisk");
        self.transport.send_raw(initial_invite.as_bytes()).await?;

        // Wait for first response
        let resp = tokio::time::timeout(Duration::from_secs(5), resp_rx.recv())
            .await?
            .ok_or_else(|| anyhow!("No response to INVITE"))?;

        let status = resp.status_code.code();
        debug!(status, %call_id, "Received response to initial INVITE");

        // Step 2: Handle 401/407 challenge (as Asterisk does with auth)
        if status == 401 || status == 407 {
            // Must send ACK for the 401 response per RFC 3261
            let to_hdr = extract_header_val(resp.headers(), "To").unwrap_or_default();
            let ack = format!(
                "ACK {request_uri} SIP/2.0\r\n\
                Via: SIP/2.0/UDP {local_ip}:{local_port};branch={branch};rport\r\n\
                From: <sip:{username}@{domain}>;tag={local_tag}\r\n\
                To: {to_hdr}\r\n\
                Call-ID: {call_id}\r\n\
                CSeq: 100 ACK\r\n\
                Content-Length: 0\r\n\r\n",
                username = self.config.username,
            );
            self.transport.send_raw(ack.as_bytes()).await?;

            // Extract challenge
            let challenge = extract_challenge(&resp)?
                .ok_or_else(|| anyhow!("401 response missing WWW-Authenticate header"))?;

            let nc = self.auth_nc.fetch_add(1, Ordering::SeqCst);
            let cnonce = format!("{:08x}", rand_u32());
            let auth_header = calculate_authorization(
                &self.config.username,
                &self.config.password,
                "INVITE",
                &request_uri,
                &challenge,
                nc,
                &cnonce,
            );

            // Send second INVITE with auth
            let auth_branch = format!("z9hG4bK-{}", rand_u32());
            let auth_invite = self.build_invite_request(
                &request_uri,
                target,
                domain,
                call_id,
                local_tag,
                101,
                &auth_branch,
                &sdp,
                Some(&auth_header),
            );

            info!(%target, %call_id, "Sending authenticated SIP INVITE to Asterisk");
            self.transport.send_raw(auth_invite.as_bytes()).await?;
        }

        // Step 3: Loop for 180 Ringing and 200 OK
        loop {
            let res = tokio::time::timeout(Duration::from_secs(30), resp_rx.recv())
                .await?
                .ok_or_else(|| anyhow!("Call disconnected or timed out"))?;

            let code = res.status_code.code();
            debug!(code, %call_id, "Call progress response from Asterisk");

            if code == 100 {
                // Trying - continue waiting
                continue;
            } else if code == 180 || code == 183 {
                // Ringing
                info!(%target, %call_id, "🔔 Remote party is ringing (180 Ringing)!");
                let _ = self
                    .engine_tx
                    .send(EngineEvent::Broadcast(Event::CallStateChanged(
                        CallStateChanged {
                            call_id: call_id.to_string(),
                            state: CallState::Early,
                            direction: CallDirection::Outbound,
                            remote_name: None,
                            remote_uri: Some(target.to_string()),
                            duration_secs: None,
                            code: Some(code),
                            reason: Some("Ringing".to_string()),
                        },
                    )));
            } else if code == 200 {
                // Call Answered!
                info!(%target, %call_id, "🎉 Call answered by remote (200 OK)!");
                let to_hdr = extract_header_val(res.headers(), "To").unwrap_or_default();
                let remote_tag = extract_tag_from_header(&to_hdr);
                let contact_hdr = extract_header_val(res.headers(), "Contact");
                let remote_contact = contact_hdr.as_deref().map(extract_contact_uri);

                {
                    let mut dialogs = self.dialogs.lock().await;
                    if let Some(d) = dialogs.get_mut(call_id) {
                        d.remote_tag = remote_tag;
                        d.incoming_to = Some(to_hdr.clone());
                        if remote_contact.is_some() {
                            d.remote_contact = remote_contact.clone();
                        }
                    }
                }

                // Send ACK for 200 OK (to remote Contact URI if provided)
                let ack_target = remote_contact.as_deref().unwrap_or(&request_uri);
                let ack_branch = format!("z9hG4bK-{}", rand_u32());
                let ack = format!(
                    "ACK {ack_target} SIP/2.0\r\n\
                    Via: SIP/2.0/UDP {local_ip}:{local_port};branch={ack_branch};rport\r\n\
                    Max-Forwards: 70\r\n\
                    From: <sip:{username}@{domain}>;tag={local_tag}\r\n\
                    To: {to_hdr}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: 101 ACK\r\n\
                    Contact: <sip:{username}@{local_ip}:{local_port};transport=udp>\r\n\
                    Content-Length: 0\r\n\r\n",
                    username = self.config.username,
                );
                self.transport.send_raw(ack.as_bytes()).await?;

                let _ = self
                    .engine_tx
                    .send(EngineEvent::Broadcast(Event::CallStateChanged(
                        CallStateChanged {
                            call_id: call_id.to_string(),
                            state: CallState::Confirmed,
                            direction: CallDirection::Outbound,
                            remote_name: None,
                            remote_uri: Some(target.to_string()),
                            duration_secs: Some(0),
                            code: Some(200),
                            reason: Some("OK".to_string()),
                        },
                    )));

                let remote_rtp = parse_sdp_audio_endpoint(res.body());
                self.start_media_session(call_id, remote_rtp).await;
                break;
            } else if code >= 400 {
                // Call rejected / busy
                let to_hdr = extract_header_val(res.headers(), "To").unwrap_or_default();
                let ack = format!(
                    "ACK {request_uri} SIP/2.0\r\n\
                    Via: SIP/2.0/UDP {local_ip}:{local_port};branch={branch};rport\r\n\
                    From: <sip:{username}@{domain}>;tag={local_tag}\r\n\
                    To: {to_hdr}\r\n\
                    Call-ID: {call_id}\r\n\
                    CSeq: 101 ACK\r\n\
                    Content-Length: 0\r\n\r\n",
                    username = self.config.username,
                );
                let _ = self.transport.send_raw(ack.as_bytes()).await;

                return Err(anyhow!(
                    "Call failed with status {}: {}",
                    code,
                    res.status_code
                ));
            }
        }

        Ok(())
    }

    /// Answer an incoming call.
    pub async fn answer(&self, call_id: &str) -> Result<()> {
        let dialog = {
            let dialogs = self.dialogs.lock().await;
            dialogs
                .get(call_id)
                .cloned()
                .ok_or_else(|| anyhow!("Call {} not found", call_id))?
        };

        let local_ip = self.transport.local_ip().to_string();
        let local_port = self.transport.local_port();
        let sdp = build_sdp(&local_ip, 10030);
        let sdp_len = sdp.len();

        let via = dialog.incoming_via.unwrap_or_default();
        let from = dialog.incoming_from.unwrap_or_default();
        let to = dialog.incoming_to.unwrap_or_default();
        let local_tag = dialog.local_tag;

        let to_with_tag = if to.contains("tag=") {
            to
        } else {
            format!("{};tag={}", to, local_tag)
        };

        info!(call_id, "Answering incoming call with 200 OK + SDP");

        let reply = format!(
            "SIP/2.0 200 OK\r\n\
            Via: {via}\r\n\
            From: {from}\r\n\
            To: {to_with_tag}\r\n\
            Call-ID: {call_id}\r\n\
            CSeq: {remote_cseq} INVITE\r\n\
            Contact: <sip:{username}@{local_ip}:{local_port};transport=udp>\r\n\
            User-Agent: rustline/0.1.0\r\n\
            Content-Type: application/sdp\r\n\
            Content-Length: {sdp_len}\r\n\r\n\
            {sdp}",
            username = self.config.username,
            remote_cseq = dialog.remote_cseq,
        );

        self.transport.send_raw(reply.as_bytes()).await?;

        {
            let mut dialogs = self.dialogs.lock().await;
            if let Some(d) = dialogs.get_mut(call_id) {
                d.incoming_to = Some(to_with_tag);
            }
        }

        let _ = self
            .engine_tx
            .send(EngineEvent::Broadcast(Event::CallStateChanged(
                CallStateChanged {
                    call_id: call_id.to_string(),
                    state: CallState::Confirmed,
                    direction: CallDirection::Inbound,
                    remote_name: dialog.remote_name,
                    remote_uri: Some(dialog.remote_uri),
                    duration_secs: Some(0),
                    code: Some(200),
                    reason: Some("OK".to_string()),
                },
            )));

        let remote_rtp = dialog.remote_rtp_addr;
        self.start_media_session(call_id, remote_rtp).await;

        Ok(())
    }

    /// Hang up an active or ringing call.
    pub async fn hangup(&self, call_id: &str) -> Result<()> {
        self.stop_media_session(call_id).await;

        let dialog = {
            let mut dialogs = self.dialogs.lock().await;
            dialogs.remove(call_id)
        };

        let dialog = match dialog {
            Some(d) => d,
            None => return Ok(()),
        };

        let local_ip = self.transport.local_ip().to_string();
        let local_port = self.transport.local_port();
        let domain = if self.config.domain.is_empty() {
            &self.config.server
        } else {
            &self.config.domain
        };

        if dialog.is_inbound && dialog.remote_tag.is_none() {
            // Incoming call that wasn't answered yet -> reject with 486 Busy Here
            let via = dialog.incoming_via.unwrap_or_default();
            let from = dialog.incoming_from.unwrap_or_default();
            let to = dialog.incoming_to.unwrap_or_default();
            let to_with_tag = if to.contains("tag=") {
                to
            } else {
                format!("{};tag={}", to, dialog.local_tag)
            };
            let busy = format!(
                "SIP/2.0 486 Busy Here\r\n\
                Via: {via}\r\n\
                From: {from}\r\n\
                To: {to_with_tag}\r\n\
                Call-ID: {call_id}\r\n\
                CSeq: {remote_cseq} INVITE\r\n\
                Content-Length: 0\r\n\r\n",
                remote_cseq = dialog.remote_cseq,
            );
            self.transport.send_raw(busy.as_bytes()).await?;
        } else {
            // Established call -> Send BYE
            let branch = format!("z9hG4bK-{}", rand_u32());
            let server_port = if self.config.port == 0 {
                5060
            } else {
                self.config.port
            };

            // Request-URI of BYE: RFC 3261 12.2.1.1 specifies the remote target (Contact URI)
            let req_uri = dialog
                .remote_contact
                .unwrap_or_else(|| format!("sip:{}@{}:{}", dialog.remote_uri, domain, server_port));

            let (from_hdr, to_hdr) = if dialog.is_inbound {
                // Inbound call:
                // From is our local URI (the incoming To header with our local tag)
                // To is the remote caller URI (the incoming From header with remote tag)
                let local_to = dialog.incoming_to.unwrap_or_else(|| {
                    format!(
                        "<sip:{}@{}>;tag={}",
                        self.config.username, domain, dialog.local_tag
                    )
                });
                let remote_from = dialog
                    .incoming_from
                    .unwrap_or_else(|| format!("<sip:{}@{}>", dialog.remote_uri, domain));
                (local_to, remote_from)
            } else {
                // Outbound call:
                let remote_tag = dialog.remote_tag.unwrap_or_default();
                let to = if remote_tag.is_empty() {
                    format!("<sip:{}@{}>", dialog.remote_uri, domain)
                } else {
                    format!("<sip:{}@{}>;tag={}", dialog.remote_uri, domain, remote_tag)
                };
                let from = format!(
                    "<sip:{}@{}>;tag={}",
                    self.config.username, domain, dialog.local_tag
                );
                (from, to)
            };

            let bye = format!(
                "BYE {req_uri} SIP/2.0\r\n\
                Via: SIP/2.0/UDP {local_ip}:{local_port};branch={branch};rport\r\n\
                Max-Forwards: 70\r\n\
                From: {from_hdr}\r\n\
                To: {to_hdr}\r\n\
                Call-ID: {call_id}\r\n\
                CSeq: 105 BYE\r\n\
                User-Agent: rustline/0.1.0\r\n\
                Content-Length: 0\r\n\r\n"
            );
            self.transport.send_raw(bye.as_bytes()).await?;
        }

        let _ = self
            .engine_tx
            .send(EngineEvent::Broadcast(Event::CallStateChanged(
                CallStateChanged {
                    call_id: call_id.to_string(),
                    state: CallState::Disconnected,
                    direction: if dialog.is_inbound {
                        CallDirection::Inbound
                    } else {
                        CallDirection::Outbound
                    },
                    remote_name: dialog.remote_name,
                    remote_uri: Some(dialog.remote_uri),
                    duration_secs: None,
                    code: Some(200),
                    reason: Some("Normal clearing".to_string()),
                },
            )));

        Ok(())
    }

    async fn cleanup_waiter(&self, call_id: &str) {
        let mut waiters = self.response_waiters.lock().await;
        waiters.remove(call_id);
    }

    /// Build a raw RFC 3261 REGISTER string.
    fn build_register_request(
        &self,
        request_uri: &str,
        domain: &str,
        cseq: u32,
        branch: &str,
        expires: u32,
        auth_header: Option<&str>,
    ) -> String {
        let local_ip = self.transport.local_ip();
        let local_port = self.transport.local_port();
        let username = &self.config.username;

        let mut req = format!(
            "REGISTER {request_uri} SIP/2.0\r\n\
            Via: SIP/2.0/UDP {local_ip}:{local_port};branch={branch};rport\r\n\
            Max-Forwards: 70\r\n\
            From: <sip:{username}@{domain}>;tag={from_tag}\r\n\
            To: <sip:{username}@{domain}>\r\n\
            Call-ID: {call_id}\r\n\
            CSeq: {cseq} REGISTER\r\n\
            Contact: <sip:{username}@{local_ip}:{local_port};transport=udp>\r\n\
            Expires: {expires}\r\n\
            Allow: PRACK, INVITE, ACK, BYE, CANCEL, UPDATE, INFO, SUBSCRIBE, NOTIFY, REFER, MESSAGE, OPTIONS\r\n\
            User-Agent: rustline/0.1.0\r\n",
            from_tag = self.from_tag,
            call_id = self.registration_call_id,
        );

        if let Some(auth) = auth_header {
            req.push_str(&format!("Authorization: {}\r\n", auth));
        }

        req.push_str("Content-Length: 0\r\n\r\n");
        req
    }

    /// Build a raw RFC 3261 INVITE string with SDP.
    #[allow(clippy::too_many_arguments)]
    fn build_invite_request(
        &self,
        request_uri: &str,
        target: &str,
        domain: &str,
        call_id: &str,
        local_tag: &str,
        cseq: u32,
        branch: &str,
        sdp: &str,
        auth_header: Option<&str>,
    ) -> String {
        let local_ip = self.transport.local_ip();
        let local_port = self.transport.local_port();
        let username = &self.config.username;
        let sdp_len = sdp.len();

        let mut req = format!(
            "INVITE {request_uri} SIP/2.0\r\n\
            Via: SIP/2.0/UDP {local_ip}:{local_port};branch={branch};rport\r\n\
            Max-Forwards: 70\r\n\
            From: <sip:{username}@{domain}>;tag={local_tag}\r\n\
            To: <sip:{target}@{domain}>\r\n\
            Call-ID: {call_id}\r\n\
            CSeq: {cseq} INVITE\r\n\
            Contact: <sip:{username}@{local_ip}:{local_port};transport=udp>\r\n\
            Allow: PRACK, INVITE, ACK, BYE, CANCEL, UPDATE, INFO, SUBSCRIBE, NOTIFY, REFER, MESSAGE, OPTIONS\r\n\
            Supported: replaces, 100rel, timer\r\n\
            User-Agent: rustline/0.1.0\r\n",
        );

        if let Some(auth) = auth_header {
            req.push_str(&format!("Authorization: {}\r\n", auth));
        }

        req.push_str(&format!(
            "Content-Type: application/sdp\r\n\
            Content-Length: {sdp_len}\r\n\r\n\
            {sdp}"
        ));

        req
    }
}

/// Extract header value by name (case-insensitive) from rsip headers.
fn extract_header_val(headers: &rsip::Headers, name: &str) -> Option<String> {
    for h in headers.iter() {
        let s = h.to_string();
        if let Some((k, v)) = s.split_once(':')
            && k.trim().eq_ignore_ascii_case(name)
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Extract and parse `DigestChallenge` from a SIP response.
fn extract_challenge(resp: &rsip::Response) -> Result<Option<DigestChallenge>> {
    for header in resp.headers().iter() {
        if let rsip::Header::WwwAuthenticate(www_auth) = header {
            let header_str = www_auth.to_string();
            if let Some(chal) = DigestChallenge::parse(&header_str) {
                return Ok(Some(chal));
            }
        }
    }
    Ok(None)
}

/// Parse username/number from SIP From URI (e.g. `<sip:101@192.168.0.104>;tag=...` -> "101").
fn extract_uri_from_from(from: &str) -> String {
    if let Some(start) = from.find("sip:") {
        let rest = &from[start + 4..];
        let end = rest.find(['@', '>', ';']).unwrap_or(rest.len());
        return rest[..end].to_string();
    }
    from.to_string()
}

/// Extract display name if present (e.g. `"John Doe" <sip:...>` -> Some("John Doe")).
fn extract_display_name_from_from(from: &str) -> Option<String> {
    if let Some(start) = from.find('"')
        && let Some(end) = from[start + 1..].find('"')
    {
        return Some(from[start + 1..start + 1 + end].to_string());
    }
    None
}

/// Extract tag value from From or To header (e.g. `;tag=12345` -> Some("12345")).
fn extract_tag_from_header(hdr: &str) -> Option<String> {
    for param in hdr.split(';') {
        let trimmed = param.trim();
        if let Some(tag) = trimmed.strip_prefix("tag=") {
            return Some(tag.trim().to_string());
        }
    }
    None
}

/// Parse numeric CSeq (e.g. `27298 INVITE` -> 27298).
fn parse_cseq_num(cseq: &str) -> u32 {
    cseq.split_whitespace()
        .next()
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(1)
}

/// Extract clean SIP URI from Contact header (e.g. `<sip:asterisk@192.168.0.104:5060>` -> `sip:asterisk@192.168.0.104:5060`).
fn extract_contact_uri(contact: &str) -> String {
    if let Some(start) = contact.find('<')
        && let Some(end) = contact[start + 1..].find('>')
    {
        return contact[start + 1..start + 1 + end].trim().to_string();
    }
    let end = contact.find(';').unwrap_or(contact.len());
    contact[..end].trim().to_string()
}
