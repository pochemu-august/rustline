//! Digest authentication per RFC 2617.
//!
//! Supports both `qop=auth` (with cnonce/nc) and legacy mode (no qop).
//! Algorithm: MD5 only (sufficient for SIP, SHA-256 can be added if needed).

use md5::{Digest, Md5};
use rand::Rng;
use thiserror::Error;

// ── Challenge (parsed from WWW-Authenticate) ────────────────────────────────

/// Parsed fields from a `WWW-Authenticate: Digest ...` header.
#[derive(Debug, Clone)]
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    pub algorithm: Option<String>,
    pub qop: Option<String>,
    pub opaque: Option<String>,
}

#[derive(Debug, Error)]
pub enum DigestParseError {
    #[error("missing 'Digest' scheme prefix")]
    NotDigest,
    #[error("required field '{0}' not found")]
    MissingField(&'static str),
}

impl DigestChallenge {
    /// Parse a `WWW-Authenticate` header value.
    ///
    /// Example input:
    /// ```text
    /// Digest realm="asterisk",nonce="4f1c...",algorithm=MD5,qop="auth"
    /// ```
    pub fn parse(header_value: &str) -> Result<Self, DigestParseError> {
        let trimmed = header_value.trim();

        // Strip the "Digest " prefix (case-insensitive).
        let params_str = if trimmed.len() >= 7
            && trimmed[..7].eq_ignore_ascii_case("Digest ")
        {
            &trimmed[7..]
        } else {
            return Err(DigestParseError::NotDigest);
        };

        let params = parse_params(params_str);

        let realm = params
            .get("realm")
            .ok_or(DigestParseError::MissingField("realm"))?
            .clone();
        let nonce = params
            .get("nonce")
            .ok_or(DigestParseError::MissingField("nonce"))?
            .clone();

        Ok(DigestChallenge {
            realm,
            nonce,
            algorithm: params.get("algorithm").cloned(),
            qop: params.get("qop").cloned(),
            opaque: params.get("opaque").cloned(),
        })
    }
}

// ── Computed Digest Response ────────────────────────────────────────────────

/// Holds all fields needed to build an `Authorization` header value.
#[derive(Debug, Clone)]
pub struct DigestResponse {
    pub username: String,
    pub realm: String,
    pub nonce: String,
    pub uri: String,
    pub response: String,
    pub algorithm: String,
    pub qop: Option<String>,
    pub nc: Option<String>,
    pub cnonce: Option<String>,
    pub opaque: Option<String>,
}

impl DigestResponse {
    /// Compute the digest response for the given challenge and credentials.
    ///
    /// `method` — SIP method string, e.g. `"REGISTER"`.
    /// `uri`    — digest-uri, e.g. `"sip:proxy.example.com"`.
    pub fn compute(
        challenge: &DigestChallenge,
        username: &str,
        password: &str,
        method: &str,
        uri: &str,
    ) -> Self {
        let algorithm = challenge
            .algorithm
            .as_deref()
            .unwrap_or("MD5")
            .to_owned();

        // HA1 = MD5(username:realm:password)
        let ha1 = md5_hex(&format!("{username}:{}:{password}", challenge.realm));

        // HA2 = MD5(method:uri)
        let ha2 = md5_hex(&format!("{method}:{uri}"));

        // Determine qop handling
        let (response_hash, qop_out, nc_out, cnonce_out) =
            if let Some(ref qop_value) = challenge.qop {
                // qop="auth" → response = MD5(HA1:nonce:nc:cnonce:qop:HA2)
                let cnonce = generate_cnonce();
                let nc = "00000001".to_owned();
                // Pick the first qop token (could be "auth,auth-int")
                let qop_token = qop_value
                    .split(',')
                    .map(str::trim)
                    .find(|q| *q == "auth")
                    .unwrap_or("auth")
                    .to_owned();

                let hash = md5_hex(&format!(
                    "{ha1}:{}:{nc}:{cnonce}:{qop_token}:{ha2}",
                    challenge.nonce
                ));
                (hash, Some(qop_token), Some(nc), Some(cnonce))
            } else {
                // No qop → response = MD5(HA1:nonce:HA2)
                let hash =
                    md5_hex(&format!("{ha1}:{}:{ha2}", challenge.nonce));
                (hash, None, None, None)
            };

        DigestResponse {
            username: username.to_owned(),
            realm: challenge.realm.clone(),
            nonce: challenge.nonce.clone(),
            uri: uri.to_owned(),
            response: response_hash,
            algorithm,
            qop: qop_out,
            nc: nc_out,
            cnonce: cnonce_out,
            opaque: challenge.opaque.clone(),
        }
    }

    /// Serialize into a value suitable for the `Authorization` header.
    pub fn to_header_value(&self) -> String {
        let mut parts = Vec::with_capacity(10);
        parts.push(format!("Digest username=\"{}\"", self.username));
        parts.push(format!("realm=\"{}\"", self.realm));
        parts.push(format!("nonce=\"{}\"", self.nonce));
        parts.push(format!("uri=\"{}\"", self.uri));
        parts.push(format!("response=\"{}\"", self.response));
        parts.push(format!("algorithm={}", self.algorithm));

        if let Some(ref qop) = self.qop {
            parts.push(format!("qop={qop}"));
        }
        if let Some(ref nc) = self.nc {
            parts.push(format!("nc={nc}"));
        }
        if let Some(ref cnonce) = self.cnonce {
            parts.push(format!("cnonce=\"{cnonce}\""));
        }
        if let Some(ref opaque) = self.opaque {
            parts.push(format!("opaque=\"{opaque}\""));
        }

        parts.join(", ")
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Compute the hex-encoded MD5 of a string.
fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// Generate a random 16-hex-char cnonce.
fn generate_cnonce() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 8] = rng.gen();
    hex::encode(bytes)
}

/// Parse a comma-separated `key=value` or `key="value"` parameter string
/// into a case-insensitive map.
fn parse_params(input: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();

    // We cannot simply split on ',' because quoted values may contain commas.
    // Simple state-machine approach:
    let mut key = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '=' if !in_value && !in_quotes => {
                in_value = true;
            }
            '"' if in_value => {
                in_quotes = !in_quotes;
            }
            ',' if !in_quotes => {
                // End of param
                let k = key.trim().to_ascii_lowercase();
                let v = value.trim().to_owned();
                if !k.is_empty() {
                    map.insert(k, v);
                }
                key.clear();
                value.clear();
                in_value = false;
            }
            _ => {
                if in_value {
                    value.push(ch);
                } else {
                    key.push(ch);
                }
            }
        }
    }

    // Last param
    let k = key.trim().to_ascii_lowercase();
    let v = value.trim().to_owned();
    if !k.is_empty() {
        map.insert(k, v);
    }

    map
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_www_authenticate() {
        let hdr =
            r#"Digest realm="asterisk", nonce="4f1caa2e", algorithm=MD5, qop="auth""#;
        let ch = DigestChallenge::parse(hdr).unwrap();
        assert_eq!(ch.realm, "asterisk");
        assert_eq!(ch.nonce, "4f1caa2e");
        assert_eq!(ch.algorithm.as_deref(), Some("MD5"));
        assert_eq!(ch.qop.as_deref(), Some("auth"));
    }

    #[test]
    fn digest_rfc2617_no_qop() {
        // Reference values from RFC 2617 §3.5 (adapted for SIP).
        let challenge = DigestChallenge {
            realm: "testrealm@host.com".into(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".into(),
            algorithm: Some("MD5".into()),
            qop: None,
            opaque: None,
        };
        let resp =
            DigestResponse::compute(&challenge, "Mufasa", "Circle Of Life", "REGISTER", "sip:host.com");
        // HA1 = MD5("Mufasa:testrealm@host.com:Circle Of Life")
        let ha1 = "939e7578ed9e3c518a452acee763bce9";
        assert_eq!(md5_hex("Mufasa:testrealm@host.com:Circle Of Life"), ha1);
        // HA2 = MD5("REGISTER:sip:host.com")
        let ha2 = md5_hex("REGISTER:sip:host.com");
        // response = MD5(HA1:nonce:HA2)
        let expected = md5_hex(&format!(
            "{ha1}:dcd98b7102dd2f0e8b11d0f600bfb0c093:{ha2}"
        ));
        assert_eq!(resp.response, expected);
    }

    #[test]
    fn authorization_header_format() {
        let challenge = DigestChallenge {
            realm: "asterisk".into(),
            nonce: "abc123".into(),
            algorithm: Some("MD5".into()),
            qop: None,
            opaque: None,
        };
        let resp = DigestResponse::compute(&challenge, "user", "pass", "REGISTER", "sip:pbx.local");
        let hdr = resp.to_header_value();
        assert!(hdr.starts_with("Digest "));
        assert!(hdr.contains("username=\"user\""));
        assert!(hdr.contains("realm=\"asterisk\""));
        assert!(hdr.contains("response=\""));
    }
}
