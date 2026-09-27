//! SIP REGISTER transaction logic.
//!
//! Implements the full registration flow:
//! 1. Send unauthenticated REGISTER
//! 2. Receive 401 → parse Digest challenge
//! 3. Send authenticated REGISTER
//! 4. Receive 200 OK → registration complete
//!
//! The module is self-contained: it takes a transport + config and returns
//! success/failure. It does NOT spawn tasks or manage timers — the engine
//! handles re-registration scheduling.

use std::net::SocketAddr;
use std::time::Duration;

use rand::Rng;
use thiserror::Error;
use tracing::{debug, info, warn};

use super::auth::{DigestChallenge, DigestResponse};
use super::message::{SipMessage, SipMethod};
use super::transport::SipTransport;

// ── Configuration ───────────────────────────────────────────────────────────

/// Parameters needed to perform a SIP REGISTER.
#[derive(Debug, Clone)]
pub struct RegisterConfig {
    /// SIP server hostname or IP (e.g. "pbx.example.com").
    pub server: String,
    /// SIP server port (typically 5060 for UDP, 5061 for TLS).
    pub port: u16,
    /// SIP username (auth user).
    pub username: String,
    /// SIP password.
    pub password: String,
}

/// Outcome of a successful registration.
#[derive(Debug, Clone)]
pub struct RegisterResult {
    /// Granted expiration time in seconds.
    pub expires: u32,
}

#[derive(Debug, Error)]
pub enum RegisterError {
    #[error("transport error: {0}")]
    Transport(#[from] super::transport::TransportError),
    #[error("SIP parse error: {0}")]
    Parse(#[from] super::message::SipParseError),
    #[error("authentication failed: server returned {0}")]
    AuthFailed(u16),
    #[error("no WWW-Authenticate header in 401 response")]
    NoChallenge,
    #[error("failed to parse digest challenge: {0}")]
    DigestParse(#[from] super::auth::DigestParseError),
    #[error("unexpected response: {0} {1}")]
    UnexpectedResponse(u16, String),
    #[error("response timeout after {0:?}")]
    Timeout(Duration),
}

// ── REGISTER transaction ────────────────────────────────────────────────────

const RECV_BUF_SIZE: usize = 4096;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_EXPIRES: u32 = 3600;

/// Execute a full SIP REGISTER transaction.
///
/// Returns the granted `expires` on success. The caller is responsible for
/// scheduling re-registration before the timer runs out.
pub async fn do_register(
    config: &RegisterConfig,
    transport: &SipTransport,
    remote_addr: SocketAddr,
) -> Result<RegisterResult, RegisterError> {
    let mut local_addr = transport.local_addr()?;
    if local_addr.ip().is_unspecified() {
        let real_ip = super::transport::discover_local_ip(remote_addr).await?;
        local_addr.set_ip(real_ip);
    }
    let transport_param = transport.transport_param();

    // Shared transaction state
    let call_id = generate_call_id(&local_addr);
    let from_tag = generate_tag();
    let branch_base = generate_branch();

    // ── Step 1: Unauthenticated REGISTER ────────────────────────────────
    let cseq = 1u32;
    let register = build_register(
        config,
        &call_id,
        &from_tag,
        &format!("{branch_base}-1"),
        cseq,
        local_addr,
        transport_param,
        None, // no auth
    );

    info!(server = %config.server, username = %config.username, "sending initial REGISTER");
    debug!(">>> SIP >>>\n{register}");
    transport.send_to(&register.to_bytes(), remote_addr).await?;

    // ── Receive first response ──────────────────────────────────────────
    let response = recv_response(transport, RESPONSE_TIMEOUT).await?;
    debug!("<<< SIP <<<\n{response}");

    let status = response.status_code().unwrap_or(0);

    match status {
        200 => {
            // Some servers accept without auth (rare but valid).
            let expires = parse_expires(&response);
            info!(expires, "registered (no auth required)");
            return Ok(RegisterResult { expires });
        }
        401 | 407 => {
            // Expected: need authentication.
            debug!(status, "received challenge, authenticating");
        }
        _ => {
            let reason = match &response.start_line {
                super::message::SipStartLine::Response { reason, .. } => reason.clone(),
                _ => "unknown".to_owned(),
            };
            return Err(RegisterError::UnexpectedResponse(status, reason));
        }
    }

    // ── Step 2: Parse challenge ─────────────────────────────────────────
    let auth_header_name = if status == 407 {
        "Proxy-Authenticate"
    } else {
        "WWW-Authenticate"
    };
    let challenge_str = response
        .header(auth_header_name)
        .ok_or(RegisterError::NoChallenge)?;
    let challenge = DigestChallenge::parse(challenge_str)?;

    // ── Step 3: Authenticated REGISTER ──────────────────────────────────
    let cseq = 2u32;
    let digest_uri = format!("sip:{}", config.server);
    let digest = DigestResponse::compute(
        &challenge,
        &config.username,
        &config.password,
        "REGISTER",
        &digest_uri,
    );

    let register = build_register(
        config,
        &call_id,
        &from_tag,
        &format!("{branch_base}-2"),
        cseq,
        local_addr,
        transport_param,
        Some(&digest),
    );

    info!("sending authenticated REGISTER");
    debug!(">>> SIP >>>\n{register}");
    transport.send_to(&register.to_bytes(), remote_addr).await?;

    // ── Receive final response ──────────────────────────────────────────
    let response = recv_response(transport, RESPONSE_TIMEOUT).await?;
    debug!("<<< SIP <<<\n{response}");

    let status = response.status_code().unwrap_or(0);
    match status {
        200 => {
            let expires = parse_expires(&response);
            info!(expires, "registration successful");
            Ok(RegisterResult { expires })
        }
        401 | 403 => {
            warn!(status, "authentication failed");
            Err(RegisterError::AuthFailed(status))
        }
        _ => {
            let reason = match &response.start_line {
                super::message::SipStartLine::Response { reason, .. } => reason.clone(),
                _ => "unknown".to_owned(),
            };
            Err(RegisterError::UnexpectedResponse(status, reason))
        }
    }
}

/// Send REGISTER with Expires: 0 to unregister.
pub async fn do_unregister(
    config: &RegisterConfig,
    transport: &SipTransport,
    remote_addr: SocketAddr,
) -> Result<(), RegisterError> {
    let mut local_addr = transport.local_addr()?;
    if local_addr.ip().is_unspecified() {
        let real_ip = super::transport::discover_local_ip(remote_addr).await?;
        local_addr.set_ip(real_ip);
    }
    let transport_param = transport.transport_param();
    let call_id = generate_call_id(&local_addr);
    let from_tag = generate_tag();
    let branch = generate_branch();

    // Build REGISTER with Expires: 0
    let mut msg = build_register(
        config,
        &call_id,
        &from_tag,
        &format!("{branch}-1"),
        1,
        local_addr,
        transport_param,
        None,
    );
    // Override the Contact and Expires headers per RFC 3261 §10.2.2
    msg.remove_headers("Contact");
    msg.add_header("Contact", "*");
    msg.remove_headers("Expires");
    msg.add_header("Expires", "0");

    info!("sending UNREGISTER (Contact: *, Expires: 0)");
    transport.send_to(&msg.to_bytes(), remote_addr).await?;

    // We attempt to receive a response but don't fail hard if it times out
    match recv_response(transport, RESPONSE_TIMEOUT).await {
        Ok(resp) => {
            let status = resp.status_code().unwrap_or(0);
            if status == 401 || status == 407 {
                // Need auth for unregister too — handle it
                let auth_header_name = if status == 407 {
                    "Proxy-Authenticate"
                } else {
                    "WWW-Authenticate"
                };
                if let Some(challenge_str) = resp.header(auth_header_name) {
                    if let Ok(challenge) = DigestChallenge::parse(challenge_str) {
                        let digest_uri = format!("sip:{}", config.server);
                        let digest = DigestResponse::compute(
                            &challenge,
                            &config.username,
                            &config.password,
                            "REGISTER",
                            &digest_uri,
                        );
                        let mut msg2 = build_register(
                            config,
                            &call_id,
                            &from_tag,
                            &format!("{branch}-2"),
                            2,
                            local_addr,
                            transport_param,
                            Some(&digest),
                        );
                        msg2.remove_headers("Contact");
                        msg2.add_header("Contact", "*");
                        msg2.remove_headers("Expires");
                        msg2.add_header("Expires", "0");
                        transport.send_to(&msg2.to_bytes(), remote_addr).await?;
                        if let Ok(final_resp) = recv_response(transport, RESPONSE_TIMEOUT).await {
                            let final_status = final_resp.status_code().unwrap_or(0);
                            info!(status = final_status, "authenticated unregister response received");
                            return Ok(());
                        }
                    }
                }
            }
            info!(status, "unregister response received");
        }
        Err(RegisterError::Timeout(_)) => {
            warn!("no response to unregister, continuing");
        }
        Err(e) => return Err(e),
    }

    Ok(())
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Build a REGISTER request message.
fn build_register(
    config: &RegisterConfig,
    call_id: &str,
    from_tag: &str,
    branch: &str,
    cseq: u32,
    local_addr: SocketAddr,
    transport_param: &str,
    auth: Option<&DigestResponse>,
) -> SipMessage {
    let request_uri = format!("sip:{}", config.server);
    let mut msg = SipMessage::new_request(SipMethod::Register, &request_uri);

    // Via: SIP/2.0/UDP local_ip:local_port;branch=z9hG4bK...;rport
    msg.add_header(
        "Via",
        format!(
            "SIP/2.0/{transport_param} {local_addr};branch={branch};rport"
        ),
    );

    // Max-Forwards
    msg.add_header("Max-Forwards", "70");

    // To / From
    let aor = format!("sip:{}@{}", config.username, config.server);
    msg.add_header("To", format!("<{aor}>"));
    msg.add_header("From", format!("<{aor}>;tag={from_tag}"));

    // Call-ID, CSeq
    msg.add_header("Call-ID", call_id);
    msg.add_header("CSeq", format!("{cseq} REGISTER"));

    // Contact — where the registrar should send requests for this AoR.
    msg.add_header(
        "Contact",
        format!("<sip:{}@{local_addr};transport={}>", config.username, transport_param.to_ascii_lowercase()),
    );

    // Expires
    msg.add_header("Expires", DEFAULT_EXPIRES.to_string());

    // Allow header — capabilities we advertise
    msg.add_header("Allow", "INVITE, ACK, CANCEL, BYE, OPTIONS");

    // User-Agent
    msg.add_header("User-Agent", "RustlineCore/0.1");

    // Authorization (if we have a digest response)
    if let Some(digest) = auth {
        msg.add_header("Authorization", digest.to_header_value());
    }

    msg
}

/// Receive a SIP response with a timeout.
async fn recv_response(
    transport: &SipTransport,
    timeout: Duration,
) -> Result<SipMessage, RegisterError> {
    let mut buf = vec![0u8; RECV_BUF_SIZE];

    let result = tokio::time::timeout(timeout, transport.recv_from(&mut buf)).await;

    match result {
        Ok(Ok((n, _addr))) => {
            let msg = SipMessage::parse(&buf[..n])?;
            Ok(msg)
        }
        Ok(Err(e)) => Err(RegisterError::Transport(e)),
        Err(_) => Err(RegisterError::Timeout(timeout)),
    }
}

/// Extract the granted `expires` from a 200 OK response.
///
/// Checks `Contact` header `expires` param first, then `Expires` header,
/// then falls back to the default.
fn parse_expires(response: &SipMessage) -> u32 {
    // Try Contact header's expires parameter
    if let Some(contact) = response.header("Contact") {
        if let Some(pos) = contact.to_ascii_lowercase().find("expires=") {
            let rest = &contact[pos + 8..];
            if let Some(val) = rest.split(|c: char| !c.is_ascii_digit()).next() {
                if let Ok(e) = val.parse::<u32>() {
                    return e;
                }
            }
        }
    }

    // Try Expires header
    if let Some(expires) = response.header("Expires") {
        if let Ok(e) = expires.trim().parse::<u32>() {
            return e;
        }
    }

    DEFAULT_EXPIRES
}

// ── Identifier generators ───────────────────────────────────────────────────

/// Generate a unique Call-ID.
fn generate_call_id(local_addr: &SocketAddr) -> String {
    let mut rng = rand::thread_rng();
    let random: u64 = rng.gen();
    format!("{random:016x}@{}", local_addr.ip())
}

/// Generate a Via branch parameter (must start with "z9hG4bK" per RFC 3261 §8.1.1.7).
fn generate_branch() -> String {
    let mut rng = rand::thread_rng();
    let random: u64 = rng.gen();
    format!("z9hG4bK{random:016x}")
}

/// Generate a random tag for From/To headers.
fn generate_tag() -> String {
    let mut rng = rand::thread_rng();
    let random: u32 = rng.gen();
    format!("{random:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_message_structure() {
        let config = RegisterConfig {
            server: "pbx.example.com".into(),
            port: 5060,
            username: "alice".into(),
            password: "secret".into(),
        };

        let local_addr: SocketAddr = "192.168.1.100:5060".parse().unwrap();
        let msg = build_register(
            &config,
            "test-call-id@192.168.1.100",
            "tag123",
            "z9hG4bK-test",
            1,
            local_addr,
            "UDP",
            None,
        );

        assert_eq!(msg.method(), Some(&SipMethod::Register));
        assert_eq!(msg.request_uri(), Some("sip:pbx.example.com"));

        let via = msg.header("Via").unwrap();
        assert!(via.contains("SIP/2.0/UDP"));
        assert!(via.contains("192.168.1.100:5060"));

        assert!(msg.header("Contact").unwrap().contains("alice"));
        assert_eq!(msg.header("CSeq"), Some("1 REGISTER"));
        assert_eq!(msg.header("Expires"), Some("3600"));
    }

    #[test]
    fn parse_expires_from_response() {
        let mut resp = SipMessage::new_response(200, "OK");
        resp.add_header("Expires", "1800");
        assert_eq!(parse_expires(&resp), 1800);

        let mut resp2 = SipMessage::new_response(200, "OK");
        resp2.add_header("Contact", "<sip:a@b>;expires=900");
        assert_eq!(parse_expires(&resp2), 900);
    }
}
