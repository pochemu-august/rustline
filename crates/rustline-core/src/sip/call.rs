//! Outgoing and incoming SIP call management (INVITE / ACK / BYE / CANCEL).
//!
//! Handles the full dialog state machine:
//! `Calling` -> `Ringing` -> `Active` (media flowing) -> `Ended`.

use std::net::SocketAddr;
use std::time::Duration;

use rand::Rng;
use thiserror::Error;
use tokio::net::UdpSocket;
use tracing::{debug, info};

use super::auth::{DigestChallenge, DigestResponse};
use super::message::{SipMessage, SipMethod};
use super::register::RegisterConfig;
use super::sdp::SdpSession;
use super::transport::{self, SipTransport};
use crate::rtp::RtpStream;
use crate::types::{CallState, CoreEvent};
use tokio::sync::broadcast;

#[derive(Debug, Error)]
pub enum CallError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("transport error: {0}")]
    Transport(#[from] super::transport::TransportError),
    #[error("SIP parse error: {0}")]
    Parse(#[from] super::message::SipParseError),
    #[error("SDP error: {0}")]
    Sdp(#[from] super::sdp::SdpError),
    #[error("RTP error: {0}")]
    Rtp(#[from] crate::rtp::RtpError),
    #[error("authentication failed: server returned {0}")]
    AuthFailed(u16),
    #[error("call rejected by remote: {0} {1}")]
    Rejected(u16, String),
    #[error("call timeout waiting for response")]
    Timeout,
    #[error("missing To tag in 200 OK response")]
    MissingToTag,
}

/// Represents an active or pending SIP call leg.
pub struct ActiveCall {
    pub call_id: String,
    pub destination: String,
    pub state: CallState,
    pub from_tag: String,
    pub to_tag: Option<String>,
    pub remote_addr: SocketAddr,
    pub cseq: u32,
    pub rtp_stream: Option<RtpStream>,
    /// The dedicated UDP transport for this call (owns the INVITE socket).
    pub call_transport: Option<std::sync::Arc<SipTransport>>,
    pub call_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl ActiveCall {
    /// Hangs up the call (sends BYE if active).
    pub async fn hangup(
        &mut self,
        config: &RegisterConfig,
        default_transport: Option<&SipTransport>,
    ) -> Result<(), CallError> {
        if let Some(ref stop) = self.call_stop {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        let transport = match (&self.call_transport, default_transport) {
            (Some(t), _) => t.as_ref(),
            (None, Some(t)) => t,
            (None, None) => {
                // No transport — nothing to send, just mark ended.
                if let Some(ref rtp) = self.rtp_stream {
                    rtp.stop();
                }
                self.rtp_stream = None;
                self.state = CallState::Ended;
                return Ok(());
            }
        };

        let local_addr = transport.local_addr()?;
        let transport_param = transport.transport_param();

        if self.state == CallState::Active {
            self.cseq += 1;
            let mut bye = SipMessage::new_request(
                SipMethod::Bye,
                format!("sip:{}@{}", self.destination, config.server),
            );

            let branch = generate_branch();
            bye.add_header(
                "Via",
                format!("SIP/2.0/{transport_param} {local_addr};branch={branch};rport"),
            );
            bye.add_header("Max-Forwards", "70");

            let to_hdr = if let Some(ref tag) = self.to_tag {
                format!("<sip:{}@{}>;tag={}", self.destination, config.server, tag)
            } else {
                format!("<sip:{}@{}>", self.destination, config.server)
            };

            bye.add_header("To", to_hdr);
            bye.add_header(
                "From",
                format!("<sip:{}@{}>;tag={}", config.username, config.server, self.from_tag),
            );
            bye.add_header("Call-ID", &self.call_id);
            bye.add_header("CSeq", format!("{} BYE", self.cseq));
            bye.add_header("User-Agent", "RustlineCore/0.1");

            info!(call_id = %self.call_id, "sending BYE");
            transport.send_to(&bye.to_bytes(), self.remote_addr).await?;
        }

        // Stop media stream
        if let Some(ref rtp) = self.rtp_stream {
            rtp.stop();
        }
        self.rtp_stream = None;
        self.state = CallState::Ended;
        Ok(())
    }
}

/// Initiates an outgoing call by sending an INVITE with an SDP offer.
pub async fn start_outgoing_call(
    destination: &str,
    config: &RegisterConfig,
    transport: &SipTransport,
    remote_addr: SocketAddr,
    event_tx: &broadcast::Sender<CoreEvent>,
) -> Result<ActiveCall, CallError> {
    let mut local_addr = transport.local_addr()?;
    if local_addr.ip().is_unspecified() {
        let real_ip = transport::discover_local_ip(remote_addr).await?;
        local_addr.set_ip(real_ip);
    }

    let transport_param = transport.transport_param();
    let call_id = generate_call_id(&local_addr);
    let from_tag = generate_tag();
    let branch_base = generate_branch();

    // 1. Bind local RTP socket directly (kept open so port never changes)
    let rtp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    let local_rtp_port = rtp_socket.local_addr()?.port();

    let session_id: u64 = rand::thread_rng().gen();
    let sdp_offer = SdpSession::build_offer(local_addr.ip(), local_rtp_port, session_id);

    // 2. Build and send initial unauthenticated INVITE
    let mut cseq = 1u32;
    let invite1 = build_invite(
        destination,
        config,
        &call_id,
        &from_tag,
        &format!("{branch_base}-1"),
        cseq,
        local_addr,
        transport_param,
        &sdp_offer,
        None,
    );

    info!(destination, call_id = %call_id, "sending initial INVITE");
    debug!(">>> SIP >>>\n{invite1}");
    let _ = event_tx.send(CoreEvent::CallStateChanged {
        call_id: call_id.clone(),
        state: CallState::Calling,
    });
    transport.send_to(&invite1.to_bytes(), remote_addr).await?;

    // 3. Receive initial responses (may receive 100 Trying, 401/407 Challenge, etc.)
    let response = loop {
        let resp = recv_call_response(transport, Duration::from_secs(10), &call_id).await?;
        debug!("<<< SIP <<<\n{resp}");

        let status = resp.status_code().unwrap_or(0);
        if status == 100 {
            debug!("received 100 Trying");
            continue;
        }

        if status == 401 || status == 407 {
            // Need authentication
            let auth_header_name = if status == 407 {
                "Proxy-Authenticate"
            } else {
                "WWW-Authenticate"
            };
            let challenge_str = resp
                .header(auth_header_name)
                .ok_or(CallError::AuthFailed(status))?;
            let challenge = DigestChallenge::parse(challenge_str)
                .map_err(|_| CallError::AuthFailed(status))?;

            cseq += 1;
            let digest_uri = format!("sip:{}@{}", destination, config.server);
            let digest = DigestResponse::compute(
                &challenge,
                &config.username,
                &config.password,
                "INVITE",
                &digest_uri,
            );

            let invite2 = build_invite(
                destination,
                config,
                &call_id,
                &from_tag,
                &format!("{branch_base}-2"),
                cseq,
                local_addr,
                transport_param,
                &sdp_offer,
                Some(&digest),
            );

            info!("sending authenticated INVITE");
            debug!(">>> SIP >>>\n{invite2}");
            transport.send_to(&invite2.to_bytes(), remote_addr).await?;
            continue;
        }

        break resp;
    };

    // 4. Handle call progression (180 Ringing / 183 Session Progress / 200 OK)
    let final_ok_response = match response.status_code().unwrap_or(0) {
        180 | 183 => {
            info!(destination, call_id = %call_id, "remote is ringing (180/183)");
            let _ = event_tx.send(CoreEvent::CallStateChanged {
                call_id: call_id.clone(),
                state: CallState::Ringing,
            });

            // Loop until 200 OK (or cancellation/rejection)
            loop {
                let resp2 = recv_call_response(transport, Duration::from_secs(60), &call_id).await?;
                debug!("<<< SIP <<<\n{resp2}");
                let code = resp2.status_code().unwrap_or(0);
                if code == 200 {
                    break resp2;
                } else if code >= 400 {
                    let _ = event_tx.send(CoreEvent::CallStateChanged {
                        call_id: call_id.clone(),
                        state: CallState::Ended,
                    });
                    let reason = extract_reason(&resp2);
                    return Err(CallError::Rejected(code, reason));
                }
            }
        }
        200 => response,
        code => {
            let _ = event_tx.send(CoreEvent::CallStateChanged {
                call_id: call_id.clone(),
                state: CallState::Ended,
            });
            let reason = extract_reason(&response);
            return Err(CallError::Rejected(code, reason));
        }
    };

    // 5. Call answered! (200 OK)
    let to_tag = extract_to_tag(&final_ok_response);
    let to_tag_val = to_tag.clone().ok_or(CallError::MissingToTag)?;

    // Parse remote SDP
    let remote_sdp = SdpSession::parse(&final_ok_response.body)?;
    let remote_rtp_addr = SocketAddr::new(remote_sdp.connection_ip, remote_sdp.media_port);
    info!(remote_rtp = %remote_rtp_addr, pt = remote_sdp.payload_type, "call answered, starting RTP");

    // 6. Send ACK
    let ack = build_ack(
        destination,
        config,
        &call_id,
        &from_tag,
        &to_tag_val,
        cseq,
        local_addr,
        transport_param,
    );
    debug!(">>> SIP >>>\n{ack}");
    transport.send_to(&ack.to_bytes(), remote_addr).await?;

    // 7. Start RTP audio stream
    let rtp_stream = RtpStream::start_with_socket(rtp_socket, remote_rtp_addr, remote_sdp.payload_type).await?;

    Ok(ActiveCall {
        call_id,
        destination: destination.to_string(),
        state: CallState::Active,
        from_tag,
        to_tag,
        remote_addr,
        cseq,
        rtp_stream: Some(rtp_stream),
        call_transport: None, // engine fills this in after the call is established
        call_stop: None,
    })
}

// ── Incoming Call ───────────────────────────────────────────────────────────

/// All data extracted from an incoming INVITE, needed to answer or decline.
pub struct IncomingInvite {
    /// SIP Call-ID from the INVITE.
    pub call_id: String,
    /// The From header's user part (who is calling us).
    pub from_user: String,
    /// The full From header value (needed for To in our response).
    pub from_header: String,
    /// The full To header value.
    pub to_header: String,
    /// Via header(s) — we must echo them back verbatim.
    pub via_headers: Vec<String>,
    /// CSeq value from the INVITE.
    pub cseq: u32,
    /// Contact URI of the remote party.
    pub contact: Option<String>,
    /// The SDP body from the INVITE (remote offer).
    pub sdp_offer: String,
    /// Where to send the response.
    pub remote_addr: SocketAddr,
    /// The From tag.
    pub from_tag: String,
}

impl IncomingInvite {
    /// Try to extract all relevant fields from an incoming INVITE request.
    pub fn from_sip_message(msg: &SipMessage, remote_addr: SocketAddr) -> Option<Self> {
        // Must be an INVITE request
        if msg.method() != Some(&SipMethod::Invite) {
            return None;
        }

        let call_id = msg.header("Call-ID")?.trim().to_string();
        let from_header = msg.header("From")?.to_string();
        let to_header = msg.header("To")?.to_string();
        let via_headers: Vec<String> = msg.headers_all("Via").iter().map(|v| v.to_string()).collect();
        let cseq = msg.cseq_number().unwrap_or(1);
        let contact = msg.header("Contact").map(|s| s.to_string());

        // Extract user part from From: <sip:USER@host>;tag=...
        let from_user = extract_user_from_uri(&from_header).unwrap_or_else(|| "unknown".into());

        // Extract From tag
        let from_tag = if let Some(pos) = from_header.to_ascii_lowercase().find("tag=") {
            let tag = &from_header[pos + 4..];
            tag.split(';').next().unwrap_or("").trim().to_string()
        } else {
            String::new()
        };

        Some(IncomingInvite {
            call_id,
            from_user,
            from_header,
            to_header,
            via_headers,
            cseq,
            contact,
            sdp_offer: msg.body.clone(),
            remote_addr,
            from_tag,
        })
    }
}

/// Sends a 180 Ringing response to an incoming INVITE.
pub fn build_ringing_response(invite: &IncomingInvite, local_addr: SocketAddr, transport_param: &str) -> SipMessage {
    let mut resp = SipMessage::new_response(180, "Ringing");

    // Echo back Via headers verbatim (RFC 3261 §8.2.6.2)
    for via in &invite.via_headers {
        resp.add_header("Via", via.clone());
    }

    // To header with our tag
    let our_tag = generate_tag();
    resp.add_header("To", format!("{};tag={}", invite.to_header, our_tag));
    resp.add_header("From", invite.from_header.clone());
    resp.add_header("Call-ID", invite.call_id.clone());
    resp.add_header("CSeq", format!("{} INVITE", invite.cseq));
    resp.add_header(
        "Contact",
        format!("<sip:{}@{};transport={}>", "rustline", local_addr, transport_param.to_ascii_lowercase()),
    );
    resp.add_header("User-Agent", "RustlineCore/0.1");

    resp
}

/// Answers an incoming call: sends 200 OK with SDP answer, waits for ACK, starts RTP.
pub async fn answer_incoming_call(
    invite: &IncomingInvite,
    config: &RegisterConfig,
    transport: &SipTransport,
) -> Result<ActiveCall, CallError> {
    let mut local_addr = transport.local_addr()?;
    if local_addr.ip().is_unspecified() {
        let real_ip = transport::discover_local_ip(invite.remote_addr).await?;
        local_addr.set_ip(real_ip);
    }
    let transport_param = transport.transport_param();

    // Parse the remote SDP offer
    let remote_sdp = SdpSession::parse(&invite.sdp_offer)?;
    let remote_rtp_addr = SocketAddr::new(remote_sdp.connection_ip, remote_sdp.media_port);

    // Bind local RTP socket directly (kept open so port never changes)
    let rtp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    let local_rtp_port = rtp_socket.local_addr()?.port();

    let session_id: u64 = rand::thread_rng().gen();
    let sdp_answer = SdpSession::build_offer(local_addr.ip(), local_rtp_port, session_id);

    // Generate our To tag
    let to_tag = generate_tag();

    // Build 200 OK response
    let mut ok_resp = SipMessage::new_response(200, "OK");

    // Echo Via headers verbatim
    for via in &invite.via_headers {
        ok_resp.add_header("Via", via.clone());
    }

    ok_resp.add_header("To", format!("{};tag={}", invite.to_header, to_tag));
    ok_resp.add_header("From", invite.from_header.clone());
    ok_resp.add_header("Call-ID", invite.call_id.clone());
    ok_resp.add_header("CSeq", format!("{} INVITE", invite.cseq));
    ok_resp.add_header(
        "Contact",
        format!(
            "<sip:{}@{};transport={}>",
            config.username,
            local_addr,
            transport_param.to_ascii_lowercase()
        ),
    );
    ok_resp.add_header("Allow", "INVITE, ACK, CANCEL, BYE, OPTIONS");
    ok_resp.add_header("Supported", "replaces, timer");
    ok_resp.add_header("Content-Type", "application/sdp");
    ok_resp.add_header("User-Agent", "RustlineCore/0.1");
    ok_resp.body = sdp_answer;

    info!(call_id = %invite.call_id, "sending 200 OK to incoming INVITE");
    debug!(">>> SIP >>>\n{ok_resp}");
    transport
        .send_to(&ok_resp.to_bytes(), invite.remote_addr)
        .await?;

    // Wait for ACK (with timeout and retransmission of 200 OK)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(32);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return Err(CallError::Timeout);
        }

        let mut buf = vec![0u8; 4096];
        let result = tokio::time::timeout(remaining, transport.recv_from(&mut buf)).await;

        match result {
            Ok(Ok((n, _addr))) => {
                let msg = SipMessage::parse(&buf[..n])?;
                // Check if this is an ACK for our dialog
                if msg.method() == Some(&SipMethod::Ack) {
                    if let Some(cid) = msg.header("Call-ID") {
                        if cid.trim() == invite.call_id {
                            debug!("<<< SIP <<<\n{msg}");
                            info!(call_id = %invite.call_id, "ACK received, call is active");
                            break;
                        }
                    }
                }
                // Might be a retransmitted INVITE — resend 200 OK
                if msg.method() == Some(&SipMethod::Invite) {
                    if let Some(cid) = msg.header("Call-ID") {
                        if cid.trim() == invite.call_id {
                            debug!("retransmitted INVITE, resending 200 OK");
                            transport
                                .send_to(&ok_resp.to_bytes(), invite.remote_addr)
                                .await?;
                        }
                    }
                }
            }
            Ok(Err(e)) => return Err(CallError::Transport(e)),
            Err(_) => return Err(CallError::Timeout),
        }
    }

    // Start RTP
    let rtp_stream = RtpStream::start_with_socket(rtp_socket, remote_rtp_addr, remote_sdp.payload_type).await?;
    info!(remote_rtp = %remote_rtp_addr, pt = remote_sdp.payload_type, "incoming call answered, RTP active");

    Ok(ActiveCall {
        call_id: invite.call_id.clone(),
        destination: invite.from_user.clone(),
        state: CallState::Active,
        from_tag: to_tag,
        to_tag: Some(invite.from_tag.clone()),
        remote_addr: invite.remote_addr,
        cseq: invite.cseq,
        rtp_stream: Some(rtp_stream),
        call_transport: None,
        call_stop: None,
    })
}

/// Extract user part from a SIP URI in a header like `<sip:100@192.168.0.104>;tag=xxx`
fn extract_user_from_uri(header_value: &str) -> Option<String> {
    // Find content between < and >
    let start = header_value.find("sip:")? + 4;
    let at = header_value[start..].find('@')?;
    Some(header_value[start..start + at].to_string())
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn build_invite(
    destination: &str,
    config: &RegisterConfig,
    call_id: &str,
    from_tag: &str,
    branch: &str,
    cseq: u32,
    local_addr: SocketAddr,
    transport_param: &str,
    sdp_body: &str,
    auth: Option<&DigestResponse>,
) -> SipMessage {
    let request_uri = format!("sip:{}@{}", destination, config.server);
    let mut msg = SipMessage::new_request(SipMethod::Invite, &request_uri);

    msg.add_header(
        "Via",
        format!("SIP/2.0/{transport_param} {local_addr};branch={branch};rport"),
    );
    msg.add_header("Max-Forwards", "70");

    let aor = format!("sip:{}@{}", config.username, config.server);
    msg.add_header("To", format!("<sip:{}@{}>", destination, config.server));
    msg.add_header("From", format!("<{aor}>;tag={from_tag}"));

    msg.add_header("Call-ID", call_id);
    msg.add_header("CSeq", format!("{cseq} INVITE"));
    msg.add_header(
        "Contact",
        format!(
            "<sip:{}@{local_addr};transport={}>",
            config.username,
            transport_param.to_ascii_lowercase()
        ),
    );

    msg.add_header("Allow", "INVITE, ACK, CANCEL, BYE, OPTIONS");
    msg.add_header("Content-Type", "application/sdp");
    msg.add_header("User-Agent", "RustlineCore/0.1");

    if let Some(digest) = auth {
        msg.add_header("Authorization", digest.to_header_value());
    }

    msg.body = sdp_body.to_string();
    msg
}

fn build_ack(
    destination: &str,
    config: &RegisterConfig,
    call_id: &str,
    from_tag: &str,
    to_tag: &str,
    cseq: u32,
    local_addr: SocketAddr,
    transport_param: &str,
) -> SipMessage {
    let request_uri = format!("sip:{}@{}", destination, config.server);
    let mut msg = SipMessage::new_request(SipMethod::Ack, &request_uri);

    let branch = generate_branch();
    msg.add_header(
        "Via",
        format!("SIP/2.0/{transport_param} {local_addr};branch={branch};rport"),
    );
    msg.add_header("Max-Forwards", "70");
    msg.add_header(
        "To",
        format!("<sip:{}@{}>;tag={to_tag}", destination, config.server),
    );
    msg.add_header(
        "From",
        format!("<sip:{}@{}>;tag={from_tag}", config.username, config.server),
    );
    msg.add_header("Call-ID", call_id);
    msg.add_header("CSeq", format!("{cseq} ACK"));
    msg.add_header("User-Agent", "RustlineCore/0.1");

    msg
}

/// Receives the next SIP response that belongs to `expected_call_id`.
/// Any packets for other Call-IDs (e.g. stale responses from a previous dialog)
/// are silently discarded so they don't corrupt a new dialog's state machine.
async fn recv_call_response(
    transport: &SipTransport,
    timeout: Duration,
    expected_call_id: &str,
) -> Result<SipMessage, CallError> {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return Err(CallError::Timeout);
        }

        let mut buf = vec![0u8; 4096];
        let result = tokio::time::timeout(remaining, transport.recv_from(&mut buf)).await;

        match result {
            Ok(Ok((n, _addr))) => {
                let msg = SipMessage::parse(&buf[..n])?;
                // Filter: only accept messages belonging to this dialog.
                if let Some(cid) = msg.header("Call-ID") {
                    if cid.trim() == expected_call_id {
                        return Ok(msg);
                    }
                    debug!(
                        call_id = expected_call_id,
                        received_call_id = cid.trim(),
                        "discarding SIP message with mismatched Call-ID"
                    );
                }
                // No Call-ID or wrong Call-ID — discard and keep waiting.
            }
            Ok(Err(e)) => return Err(CallError::Transport(e)),
            Err(_) => return Err(CallError::Timeout),
        }
    }
}

fn extract_to_tag(msg: &SipMessage) -> Option<String> {
    let to_val = msg.header("To")?;
    if let Some(pos) = to_val.to_ascii_lowercase().find("tag=") {
        let tag = &to_val[pos + 4..];
        let tag_clean = tag.split(';').next()?.trim();
        return Some(tag_clean.to_string());
    }
    None
}

fn extract_reason(msg: &SipMessage) -> String {
    match &msg.start_line {
        super::message::SipStartLine::Response { reason, .. } => reason.clone(),
        _ => "Unknown".to_string(),
    }
}

fn generate_call_id(local_addr: &SocketAddr) -> String {
    let mut rng = rand::thread_rng();
    let random: u64 = rng.gen();
    format!("{random:016x}@{}", local_addr.ip())
}

fn generate_branch() -> String {
    let mut rng = rand::thread_rng();
    let random: u64 = rng.gen();
    format!("z9hG4bK{random:016x}")
}

fn generate_tag() -> String {
    let mut rng = rand::thread_rng();
    let random: u32 = rng.gen();
    format!("{random:08x}")
}
