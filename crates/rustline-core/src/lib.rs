//! # rustline-core
//!
//! The SIP engine for the rustline softphone.
//!
//! Contains:
//! - Account state machine (Unregistered → Registering → Registered)
//! - Call state machine (mirroring the PJSIP INVITE states from MicroSIP)
//! - SIP transport abstraction (future: backed by `rsip`)
//!
//! ## Architecture
//!
//! The [`Engine`] is the central coordinator. It owns the account state and
//! a list of active calls. The daemon sends [`Command`]s into the engine and
//! receives [`Event`]s back through an async channel.

pub mod account;
pub mod call;
pub mod engine;

pub use engine::Engine;
