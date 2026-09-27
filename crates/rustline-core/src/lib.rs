//! RustlineCore — SIP/VoIP engine library.
//!
//! This crate contains only the core SIP/RTP logic and an internal channel-based
//! API. It has **no** dependencies on WebSocket, JSON, or any presentation layer.
//!
//! # Architecture
//!
//! ```text
//!   ┌─────────────────────────────────────────────┐
//!   │          Control Layer (separate crate)      │
//!   │   WebSocket/JSON, gRPC, CLI — whatever       │
//!   └────────────┬──────────────┬─────────────────┘
//!         mpsc   │              │  broadcast
//!    CoreCommand │              │  CoreEvent
//!                ▼              │
//!   ┌────────────────────────────────────────────┐
//!   │               Engine (this crate)          │
//!   │  SIP REGISTER · INVITE · RTP · Codecs      │
//!   └────────────────────────────────────────────┘
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use rustline_core::engine::Engine;
//! use rustline_core::types::{CoreCommand, TransportType};
//!
//! #[tokio::main]
//! async fn main() {
//!     let (engine, handle) = Engine::new();
//!     tokio::spawn(engine.run());
//!
//!     let (tx, rx) = tokio::sync::oneshot::channel();
//!     handle.send_command(CoreCommand::Register {
//!         server: "pbx.example.com".into(),
//!         port: 5060,
//!         username: "alice".into(),
//!         password: "secret".into(),
//!         transport: TransportType::Udp,
//!         response_tx: tx,
//!     }).await.unwrap();
//!
//!     match rx.await.unwrap() {
//!         Ok(()) => println!("Registered!"),
//!         Err(e) => eprintln!("Failed: {e}"),
//!     }
//! }
//! ```

pub mod engine;
pub mod sip;
pub mod types;
