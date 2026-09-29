//! SIP protocol stack implementation for rustline.
//!
//! Includes UDP transport, Digest authentication, and SIP transaction handling.

pub mod auth;
pub mod client;
pub mod transport;

pub use client::SipClient;
pub use transport::SipTransport;
