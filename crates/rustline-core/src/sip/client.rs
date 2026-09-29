//! High-level SIP Client handling Registration, Digest Auth, and Keep-Alive.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use rsip::message::HasHeaders;
use tracing::{debug, info};
use uuid::Uuid;

use super::auth::{DigestChallenge, calculate_authorization};
use super::transport::SipTransport;
use crate::account::AccountConfig;

/// Handles SIP registration and session maintenance.
pub struct SipClient {
    config: AccountConfig,
    transport: SipTransport,
    call_id: String,
    from_tag: String,
    cseq: AtomicU32,
    auth_nc: AtomicU32,
}

impl SipClient {
    /// Create a new SIP client with account configuration and bound UDP transport.
    pub async fn new(config: AccountConfig) -> Result<Self> {
        let port = if config.port == 0 { 5060 } else { config.port };
        let transport = SipTransport::new(&config.server, port).await?;
        let call_id = format!("{}@{}", Uuid::new_v4(), transport.local_ip());
        let from_tag = format!("{:x}", rand_u32());

        Ok(Self {
            config,
            transport,
            call_id,
            from_tag,
            cseq: AtomicU32::new(1),
            auth_nc: AtomicU32::new(1),
        })
    }

    /// Access the underlying transport.
    pub fn transport(&self) -> &SipTransport {
        &self.transport
    }

    /// Execute the full SIP registration transaction (RFC 3261 + RFC 2617 Digest Auth).
    ///
    /// Returns the granted expiration time in seconds on success.
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

        // Step 1: Send initial REGISTER (without Authorization)
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

        // Step 2: Wait for response (expect 401 Unauthorized or 200 OK)
        let (msg, _) = self.transport.recv_message(Duration::from_secs(5)).await?;
        let response = match msg {
            rsip::SipMessage::Response(res) => res,
            _ => {
                return Err(anyhow!(
                    "Received unexpected SIP request instead of response"
                ));
            }
        };

        let status_code = response.status_code.code();
        debug!(
            status = status_code,
            "Received SIP response to initial REGISTER"
        );

        if status_code == 200 {
            info!("SIP registration succeeded on first attempt (200 OK)");
            return Ok(expires);
        }

        if status_code != 401 && status_code != 407 {
            return Err(anyhow!(
                "SIP registration rejected with status {}: {}",
                status_code,
                response.status_code
            ));
        }

        // Step 3: Parse WWW-Authenticate header
        let challenge = extract_challenge(&response)?
            .ok_or_else(|| anyhow!("401 response missing WWW-Authenticate header"))?;

        debug!(realm = %challenge.realm, nonce = %challenge.nonce, "Parsed Digest challenge from Asterisk");

        // Step 4: Calculate Authorization header
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

        // Step 5: Send authenticated REGISTER
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

        // Step 6: Wait for final 200 OK
        let (final_msg, _) = self.transport.recv_message(Duration::from_secs(5)).await?;
        let final_resp = match final_msg {
            rsip::SipMessage::Response(res) => res,
            _ => return Err(anyhow!("Received unexpected SIP request instead of 200 OK")),
        };

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
            call_id = self.call_id,
        );

        if let Some(auth) = auth_header {
            req.push_str(&format!("Authorization: {}\r\n", auth));
        }

        req.push_str("Content-Length: 0\r\n\r\n");
        req
    }
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

/// Helper to generate a random 32-bit unsigned integer using uuid.
fn rand_u32() -> u32 {
    let bytes = *Uuid::new_v4().as_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_challenge() {
        let raw = b"SIP/2.0 401 Unauthorized\r\nVia: SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-123\r\nFrom: <sip:100@127.0.0.1>;tag=1\r\nTo: <sip:100@127.0.0.1>\r\nCall-ID: abc@127.0.0.1\r\nCSeq: 1 REGISTER\r\nWWW-Authenticate: Digest realm=\"asterisk\", nonce=\"12345\"\r\nContent-Length: 0\r\n\r\n";
        let msg = rsip::SipMessage::try_from(raw.as_ref()).unwrap();
        if let rsip::SipMessage::Response(res) = msg {
            let chal = extract_challenge(&res).unwrap();
            assert!(chal.is_some());
        }
    }
}
