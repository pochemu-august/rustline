//! # rustline-proto
//!
//! Protocol types for the rustline WebSocket JSON-RPC API.
//!
//! This crate defines all request, response, and event structures that flow
//! between UI clients and the `rustline-daemon` over WebSocket.
//!
//! Wire format: **JSON-RPC 2.0** (simplified — no batch support yet).

pub mod commands;
pub mod events;
pub mod messages;

pub use commands::*;
pub use events::*;
pub use messages::*;
