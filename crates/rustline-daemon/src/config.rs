//! Daemon configuration.
//!
//! Config is loaded from a JSON file (default: `rustline.json` in CWD).
//! All fields have sensible defaults so the file can be minimal.

use serde::Deserialize;
use std::path::Path;
use tracing::info;

/// Top-level daemon configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Address to bind the WebSocket control API.
    pub listen_addr: String,
    /// Port for the WebSocket control API.
    pub listen_port: u16,
    /// Authentication token. Clients must send this as the first message.
    /// If empty, authentication is disabled (NOT recommended for production).
    pub auth_token: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1".into(),
            listen_port: 7890,
            auth_token: String::new(),
        }
    }
}

impl Config {
    /// Load configuration from a JSON file.
    ///
    /// If the file doesn't exist, returns the default config.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            info!(
                path = %path.display(),
                "config file not found, using defaults"
            );
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&contents)?;
        info!(
            path = %path.display(),
            listen = %format!("{}:{}", config.listen_addr, config.listen_port),
            auth = if config.auth_token.is_empty() { "disabled" } else { "enabled" },
            "config loaded"
        );
        Ok(config)
    }

    /// Full listen address as "addr:port".
    pub fn listen_endpoint(&self) -> String {
        format!("{}:{}", self.listen_addr, self.listen_port)
    }

    /// Whether token authentication is required.
    pub fn auth_required(&self) -> bool {
        !self.auth_token.is_empty()
    }
}
