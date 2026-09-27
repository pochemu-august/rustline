//! SIP message parsing and serialization.
//!
//! Implements just enough of RFC 3261 to handle REGISTER/INVITE/BYE/ACK/CANCEL
//! transactions. We parse/build messages from raw bytes — no third-party SIP
//! crate is used so that we have full control over the wire format.

use std::fmt;
use std::str::FromStr;
use thiserror::Error;

// ── SIP Method ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SipMethod {
    Register,
    Invite,
    Ack,
    Bye,
    Cancel,
    Options,
    Other(String),
}

impl fmt::Display for SipMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register => f.write_str("REGISTER"),
            Self::Invite => f.write_str("INVITE"),
            Self::Ack => f.write_str("ACK"),
            Self::Bye => f.write_str("BYE"),
            Self::Cancel => f.write_str("CANCEL"),
            Self::Options => f.write_str("OPTIONS"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

impl FromStr for SipMethod {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "REGISTER" => Self::Register,
            "INVITE" => Self::Invite,
            "ACK" => Self::Ack,
            "BYE" => Self::Bye,
            "CANCEL" => Self::Cancel,
            "OPTIONS" => Self::Options,
            other => Self::Other(other.to_owned()),
        })
    }
}

// ── First-line variant ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SipStartLine {
    /// e.g. `REGISTER sip:proxy.example.com SIP/2.0`
    Request {
        method: SipMethod,
        uri: String,
    },
    /// e.g. `SIP/2.0 200 OK`
    Response {
        status_code: u16,
        reason: String,
    },
}

// ── SIP Message ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SipMessage {
    pub start_line: SipStartLine,
    /// Headers stored as `(name, value)` pairs in original order.
    /// Name comparison is case-insensitive (RFC 3261 §7.3.1).
    pub headers: Vec<(String, String)>,
    /// Optional body (SDP or other content).
    pub body: String,
}

#[derive(Debug, Error)]
pub enum SipParseError {
    #[error("empty message")]
    Empty,
    #[error("invalid start line: {0}")]
    InvalidStartLine(String),
    #[error("invalid header line: {0}")]
    InvalidHeader(String),
    #[error("not valid UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
}

impl SipMessage {
    // ── Constructors ────────────────────────────────────────────────────

    /// Create a new SIP request with the given method and Request-URI.
    pub fn new_request(method: SipMethod, uri: impl Into<String>) -> Self {
        Self {
            start_line: SipStartLine::Request {
                method,
                uri: uri.into(),
            },
            headers: Vec::new(),
            body: String::new(),
        }
    }

    /// Create a new SIP response with the given status code and reason phrase.
    pub fn new_response(status_code: u16, reason: impl Into<String>) -> Self {
        Self {
            start_line: SipStartLine::Response {
                status_code,
                reason: reason.into(),
            },
            headers: Vec::new(),
            body: String::new(),
        }
    }

    // ── Header access (case-insensitive) ────────────────────────────────

    /// Add a header. Name is stored as-is but compared case-insensitively.
    pub fn add_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers.push((name.into(), value.into()));
    }

    /// Get the first header value matching `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        let name_lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| n.to_ascii_lowercase() == name_lower)
            .map(|(_, v)| v.as_str())
    }

    /// Get all header values matching `name` (case-insensitive).
    pub fn headers_all(&self, name: &str) -> Vec<&str> {
        let name_lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .filter(|(n, _)| n.to_ascii_lowercase() == name_lower)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Remove all headers matching `name` (case-insensitive).
    pub fn remove_headers(&mut self, name: &str) {
        let name_lower = name.to_ascii_lowercase();
        self.headers
            .retain(|(n, _)| n.to_ascii_lowercase() != name_lower);
    }

    // ── Convenience accessors ───────────────────────────────────────────

    /// Returns the method if this is a request.
    pub fn method(&self) -> Option<&SipMethod> {
        match &self.start_line {
            SipStartLine::Request { method, .. } => Some(method),
            _ => None,
        }
    }

    /// Returns the Request-URI if this is a request.
    pub fn request_uri(&self) -> Option<&str> {
        match &self.start_line {
            SipStartLine::Request { uri, .. } => Some(uri),
            _ => None,
        }
    }

    /// Returns the status code if this is a response.
    pub fn status_code(&self) -> Option<u16> {
        match &self.start_line {
            SipStartLine::Response { status_code, .. } => Some(*status_code),
            _ => None,
        }
    }

    /// Returns true if this is a response.
    pub fn is_response(&self) -> bool {
        matches!(&self.start_line, SipStartLine::Response { .. })
    }

    /// Returns the CSeq method from the CSeq header (e.g. "REGISTER" from "1 REGISTER").
    pub fn cseq_method(&self) -> Option<SipMethod> {
        let cseq = self.header("CSeq")?;
        let method_str = cseq.split_whitespace().nth(1)?;
        SipMethod::from_str(method_str).ok()
    }

    /// Returns the CSeq sequence number.
    pub fn cseq_number(&self) -> Option<u32> {
        let cseq = self.header("CSeq")?;
        cseq.split_whitespace().next()?.parse().ok()
    }

    // ── Serialization ───────────────────────────────────────────────────

    /// Serialize the message to a byte buffer ready to send over the wire.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = String::with_capacity(1024);

        // Start line
        match &self.start_line {
            SipStartLine::Request { method, uri } => {
                buf.push_str(&format!("{method} {uri} SIP/2.0\r\n"));
            }
            SipStartLine::Response {
                status_code,
                reason,
            } => {
                buf.push_str(&format!("SIP/2.0 {status_code} {reason}\r\n"));
            }
        }

        // Headers
        for (name, value) in &self.headers {
            buf.push_str(name);
            buf.push_str(": ");
            buf.push_str(value);
            buf.push_str("\r\n");
        }

        // Ensure Content-Length is present (required by RFC 3261 §20.14).
        // If the caller didn't add it, we append it here.
        if self.header("Content-Length").is_none() {
            buf.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }

        // Blank line separating headers from body
        buf.push_str("\r\n");

        // Body
        if !self.body.is_empty() {
            buf.push_str(&self.body);
        }

        buf.into_bytes()
    }

    // ── Parsing ─────────────────────────────────────────────────────────

    /// Parse a SIP message from raw bytes.
    pub fn parse(data: &[u8]) -> Result<Self, SipParseError> {
        let text = std::str::from_utf8(data)?;
        Self::parse_str(text)
    }

    /// Parse from a `&str`.
    pub fn parse_str(text: &str) -> Result<Self, SipParseError> {
        if text.is_empty() {
            return Err(SipParseError::Empty);
        }

        // Split into header-section and body at the first blank line (\r\n\r\n).
        let (header_section, body) = match text.find("\r\n\r\n") {
            Some(pos) => (&text[..pos], text[pos + 4..].to_owned()),
            None => {
                // Tolerate \n\n as well (some implementations send LF only).
                match text.find("\n\n") {
                    Some(pos) => (&text[..pos], text[pos + 2..].to_owned()),
                    None => (text, String::new()),
                }
            }
        };

        let mut lines = header_section.lines();

        // ── Start line ──────────────────────────────────────────────────
        let first_line = lines.next().ok_or(SipParseError::Empty)?;
        let start_line = Self::parse_start_line(first_line)?;

        // ── Headers ─────────────────────────────────────────────────────
        let mut headers = Vec::new();
        for line in lines {
            // Header continuation (folding): lines starting with SP or HT
            // are appended to the previous header value (RFC 3261 §7.3.1).
            if (line.starts_with(' ') || line.starts_with('\t')) && !headers.is_empty() {
                let last: &mut (String, String) = headers.last_mut().unwrap();
                last.1.push(' ');
                last.1.push_str(line.trim());
                continue;
            }

            // Normal "Name: Value" line
            if let Some(colon_pos) = line.find(':') {
                let name = line[..colon_pos].trim().to_owned();
                let value = line[colon_pos + 1..].trim().to_owned();
                headers.push((name, value));
            }
            // Silently skip lines that don't look like headers (e.g. empty).
        }

        Ok(SipMessage {
            start_line,
            headers,
            body,
        })
    }

    /// Parse the first line into a [`SipStartLine`].
    fn parse_start_line(line: &str) -> Result<SipStartLine, SipParseError> {
        let line = line.trim_end_matches('\r');

        if line.starts_with("SIP/") {
            // Response: "SIP/2.0 200 OK"
            let mut parts = line.splitn(3, ' ');
            let _version = parts
                .next()
                .ok_or_else(|| SipParseError::InvalidStartLine(line.to_owned()))?;
            let code_str = parts
                .next()
                .ok_or_else(|| SipParseError::InvalidStartLine(line.to_owned()))?;
            let reason = parts.next().unwrap_or("").to_owned();
            let status_code: u16 = code_str
                .parse()
                .map_err(|_| SipParseError::InvalidStartLine(line.to_owned()))?;
            Ok(SipStartLine::Response {
                status_code,
                reason,
            })
        } else {
            // Request: "REGISTER sip:proxy.example.com SIP/2.0"
            let mut parts = line.splitn(3, ' ');
            let method_str = parts
                .next()
                .ok_or_else(|| SipParseError::InvalidStartLine(line.to_owned()))?;
            let uri = parts
                .next()
                .ok_or_else(|| SipParseError::InvalidStartLine(line.to_owned()))?
                .to_owned();
            // Ignore version part
            let method = SipMethod::from_str(method_str).unwrap();
            Ok(SipStartLine::Request { method, uri })
        }
    }
}

// ── Display ─────────────────────────────────────────────────────────────────

impl fmt::Display for SipMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.to_bytes()))
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_request() {
        let mut msg = SipMessage::new_request(
            SipMethod::Register,
            "sip:proxy.example.com",
        );
        msg.add_header("Via", "SIP/2.0/UDP 192.168.1.1:5060;branch=z9hG4bK-test");
        msg.add_header("To", "<sip:alice@example.com>");
        msg.add_header("From", "<sip:alice@example.com>;tag=abc123");
        msg.add_header("Call-ID", "unique-id@192.168.1.1");
        msg.add_header("CSeq", "1 REGISTER");

        let bytes = msg.to_bytes();
        let parsed = SipMessage::parse(&bytes).unwrap();

        assert_eq!(parsed.method(), Some(&SipMethod::Register));
        assert_eq!(parsed.request_uri(), Some("sip:proxy.example.com"));
        assert_eq!(
            parsed.header("Via"),
            Some("SIP/2.0/UDP 192.168.1.1:5060;branch=z9hG4bK-test")
        );
        assert_eq!(parsed.cseq_method(), Some(SipMethod::Register));
        assert_eq!(parsed.cseq_number(), Some(1));
    }

    #[test]
    fn parse_response() {
        let raw = b"SIP/2.0 401 Unauthorized\r\n\
            Via: SIP/2.0/UDP 192.168.1.1:5060;branch=z9hG4bK-test\r\n\
            WWW-Authenticate: Digest realm=\"asterisk\",nonce=\"abc\"\r\n\
            Content-Length: 0\r\n\
            \r\n";
        let msg = SipMessage::parse(raw).unwrap();
        assert_eq!(msg.status_code(), Some(401));
        assert!(msg.header("WWW-Authenticate").is_some());
    }
}
