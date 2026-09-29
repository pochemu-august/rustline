//! Account state machine.
//!
//! Models the SIP registration lifecycle extracted from MicroSIP:
//! - `registerRefresh` = 300 s (REGISTER Expires header)
//! - `keepAlive` = 15 s (UDP CRLF keep-alive)
//! - Fallback to PJSUA_REG_INTERVAL (300 s) when user sets ≤ 0

use rustline_proto::events::RegistrationState;
use serde::{Deserialize, Serialize};

/// Default SIP REGISTER refresh interval in seconds.
/// Corresponds to `PJSUA_REG_INTERVAL` / MicroSIP `registerRefresh` default.
pub const DEFAULT_REGISTER_REFRESH: u32 = 300;

/// Default UDP keep-alive interval in seconds.
/// Corresponds to MicroSIP `keepAlive` default.
pub const DEFAULT_KEEP_ALIVE: u32 = 15;

/// Minimum allowed register refresh (MicroSIP clamps to 10).
pub const MIN_REGISTER_REFRESH: u32 = 10;

/// Minimum allowed keep-alive (MicroSIP replaces 1 with 2).
pub const MIN_KEEP_ALIVE: u32 = 2;

/// SIP account configuration, modeled after MicroSIP's `Account` struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub server: String,
    pub username: String,
    pub password: String,
    pub domain: String,
    pub display_name: String,
    pub transport: String,
    pub port: u16,
    pub register_refresh: u32,
    pub keep_alive: u32,
}

impl AccountConfig {
    /// Clamp the timers to sane minimums (as MicroSIP does).
    pub fn sanitize(&mut self) {
        if self.register_refresh < MIN_REGISTER_REFRESH {
            self.register_refresh = DEFAULT_REGISTER_REFRESH;
        }
        if self.keep_alive < MIN_KEEP_ALIVE {
            self.keep_alive = DEFAULT_KEEP_ALIVE;
        }
        if self.domain.is_empty() {
            self.domain = self.server.clone();
        }
        if self.port == 0 {
            self.port = if self.transport == "tls" { 5061 } else { 5060 };
        }
    }
}

/// The current state of the SIP account.
#[derive(Debug)]
pub struct AccountState {
    pub config: Option<AccountConfig>,
    pub registration: RegistrationState,
    pub last_error: Option<String>,
}

impl Default for AccountState {
    fn default() -> Self {
        Self {
            config: None,
            registration: RegistrationState::Unregistered,
            last_error: None,
        }
    }
}

impl AccountState {
    /// Transition to `Registering`.
    pub fn start_registering(&mut self, config: AccountConfig) {
        self.config = Some(config);
        self.registration = RegistrationState::Registering;
        self.last_error = None;
    }

    /// Transition to `Registered` (200 OK received).
    pub fn mark_registered(&mut self) {
        self.registration = RegistrationState::Registered;
        self.last_error = None;
    }

    /// Transition to `Failed`.
    pub fn mark_failed(&mut self, reason: String) {
        self.registration = RegistrationState::Failed;
        self.last_error = Some(reason);
    }

    /// Transition to `Unregistered`.
    pub fn mark_unregistered(&mut self) {
        self.registration = RegistrationState::Unregistered;
        self.config = None;
        self.last_error = None;
    }
}
