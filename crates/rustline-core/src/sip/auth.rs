//! SIP Digest Authentication (RFC 2617 / RFC 3261).
//!
//! Provides parsing for `WWW-Authenticate` challenge headers and
//! calculation of `Authorization` response headers using MD5.

use md5::{Digest, Md5};

/// Parsed parameters from a `WWW-Authenticate: Digest ...` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    pub domain: Option<String>,
    pub opaque: Option<String>,
    pub stale: Option<bool>,
    pub algorithm: String,
    pub qop: Option<String>,
}

impl DigestChallenge {
    /// Parse a `WWW-Authenticate` header value (e.g. `Digest realm="asterisk", nonce="12345", algorithm=MD5, qop="auth"`).
    pub fn parse(header_val: &str) -> Option<Self> {
        let lower = header_val.to_ascii_lowercase();
        let digest_pos = lower.find("digest ")?;
        let stripped = header_val[digest_pos + 7..].trim();

        let mut challenge = Self {
            algorithm: "MD5".to_string(),
            ..Default::default()
        };

        // Split key-value pairs separated by commas
        for param in split_params(stripped) {
            let (k, v) = match param.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim().trim_matches('"')),
                None => continue,
            };

            match k.to_ascii_lowercase().as_str() {
                "realm" => challenge.realm = v.to_string(),
                "nonce" => challenge.nonce = v.to_string(),
                "domain" => challenge.domain = Some(v.to_string()),
                "opaque" => challenge.opaque = Some(v.to_string()),
                "stale" => challenge.stale = Some(v.eq_ignore_ascii_case("true")),
                "algorithm" => challenge.algorithm = v.to_string(),
                "qop" => challenge.qop = Some(v.to_string()),
                _ => {}
            }
        }

        if challenge.realm.is_empty() || challenge.nonce.is_empty() {
            None
        } else {
            Some(challenge)
        }
    }
}

/// Helper to split comma-separated parameters while respecting quoted strings.
fn split_params(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;

    for (idx, ch) in input.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                parts.push(input[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }

    if start < input.len() {
        parts.push(input[start..].trim());
    }

    parts
}

/// Helper to calculate MD5 hex digest of a byte slice / string.
fn md5_hex(data: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Build an `Authorization` header value for a request.
pub fn calculate_authorization(
    username: &str,
    password: &str,
    method: &str,
    uri: &str,
    challenge: &DigestChallenge,
    nc: u32,
    cnonce: &str,
) -> String {
    // HA1 = MD5(username:realm:password)
    let ha1 = md5_hex(&format!("{}:{}:{}", username, challenge.realm, password));

    // HA2 = MD5(method:digestURI)
    let ha2 = md5_hex(&format!("{}:{}", method, uri));

    // Determine if qop includes "auth"
    let has_auth_qop = challenge
        .qop
        .as_deref()
        .map(|q| {
            q.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("auth"))
        })
        .unwrap_or(false);

    let (response, qop_str) = if has_auth_qop {
        let nc_str = format!("{:08x}", nc);
        // response = MD5(HA1:nonce:nc:cnonce:qop:HA2)
        let resp = md5_hex(&format!(
            "{}:{}:{}:{}:auth:{}",
            ha1, challenge.nonce, nc_str, cnonce, ha2
        ));
        (resp, Some(("auth", nc_str)))
    } else {
        // response = MD5(HA1:nonce:HA2)
        let resp = md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, ha2));
        (resp, None)
    };

    let mut auth_hdr = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", algorithm={}",
        username, challenge.realm, challenge.nonce, uri, response, challenge.algorithm
    );

    if let Some((qop, nc_str)) = qop_str {
        auth_hdr.push_str(&format!(
            ", cnonce=\"{}\", nc={}, qop={}",
            cnonce, nc_str, qop
        ));
    }

    if let Some(ref opaque) = challenge.opaque {
        auth_hdr.push_str(&format!(", opaque=\"{}\"", opaque));
    }

    auth_hdr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_challenge() {
        let raw = r#"Digest realm="asterisk", nonce="1727632900/12345", algorithm=MD5, qop="auth""#;
        let chal = DigestChallenge::parse(raw).expect("should parse");
        assert_eq!(chal.realm, "asterisk");
        assert_eq!(chal.nonce, "1727632900/12345");
        assert_eq!(chal.algorithm, "MD5");
        assert_eq!(chal.qop, Some("auth".to_string()));
    }

    #[test]
    fn test_auth_calculation_without_qop() {
        let chal = DigestChallenge {
            realm: "asterisk".to_string(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_string(),
            algorithm: "MD5".to_string(),
            ..Default::default()
        };

        let auth = calculate_authorization(
            "100",
            "100password",
            "REGISTER",
            "sip:192.168.0.104",
            &chal,
            1,
            "0a4f113b",
        );

        assert!(auth.contains("username=\"100\""));
        assert!(auth.contains("realm=\"asterisk\""));
        assert!(auth.contains("response="));
    }

    #[test]
    fn test_auth_calculation_with_qop() {
        let chal = DigestChallenge {
            realm: "asterisk".to_string(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_string(),
            algorithm: "MD5".to_string(),
            qop: Some("auth".to_string()),
            ..Default::default()
        };

        let auth = calculate_authorization(
            "100",
            "100password",
            "REGISTER",
            "sip:192.168.0.104",
            &chal,
            1,
            "0a4f113b",
        );

        assert!(auth.contains("qop=auth"));
        assert!(auth.contains("nc=00000001"));
        assert!(auth.contains("cnonce=\"0a4f113b\""));
    }
}
