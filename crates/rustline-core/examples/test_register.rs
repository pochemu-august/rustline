//! Standalone test binary: registers on a real SIP server via the core engine.
//!
//! Usage:
//!   cargo run --example test_register -- <server> <port> <username> <password>
//!
//! Example (Asterisk on local network):
//!   cargo run --example test_register -- 192.168.1.10 5060 1001 secret123
//!
//! What to expect:
//!   - "Event: RegistrationStateChanged { state: Registering }" printed first
//!   - Then either:
//!     • "Registration successful!" + "Event: RegistrationStateChanged { state: Registered { expires: ... } }"
//!     • "Registration failed: ..." + "Event: RegistrationStateChanged { state: Failed(...) }"
//!   - The program exits after printing the result.

use rustline_core::engine::Engine;
use rustline_core::types::{CoreCommand, CoreEvent, TransportType};
use tokio::sync::oneshot;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Initialize tracing: set RUST_LOG=debug for verbose SIP packet dumps.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "Usage: {} <server> <port> <username> <password>",
            args[0]
        );
        eprintln!("  Example: {} 192.168.1.10 5060 1001 secret123", args[0]);
        std::process::exit(1);
    }

    let server = args[1].clone();
    let port: u16 = args[2].parse().expect("port must be a number (e.g. 5060)");
    let username = args[3].clone();
    let password = args[4].clone();

    println!("╔══════════════════════════════════════════════╗");
    println!("║  RustlineCore — REGISTER test                ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Server:   {:<34}║", format!("{server}:{port}"));
    println!("║  Username: {:<34}║", username);
    println!("║  Transport: UDP                              ║");
    println!("╚══════════════════════════════════════════════╝");
    println!();

    // ── Create and start engine ─────────────────────────────────────────
    let (engine, handle) = Engine::new();
    let engine_task = tokio::spawn(engine.run());

    // ── Subscribe to events (print them in background) ──────────────────
    let mut events = handle.subscribe_events();
    let events_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match &event {
                CoreEvent::RegistrationStateChanged { state } => {
                    println!("  📡 Event: RegistrationStateChanged → {state:?}");
                }
                CoreEvent::CallStateChanged { call_id, state } => {
                    println!("  📞 Event: CallStateChanged (call_id={call_id}) → {state:?}");
                }
                CoreEvent::IncomingCall { call_id, from } => {
                    println!("  🔔 Event: IncomingCall (call_id={call_id}) from {from}");
                }
                CoreEvent::Error { call_id, message } => {
                    println!("  ❌ Event: Error (call_id={call_id:?}) → {message}");
                }
            }
        }
    });

    // ── Send REGISTER command ───────────────────────────────────────────
    let (tx, rx) = oneshot::channel();
    handle
        .send_command(CoreCommand::Register {
            server,
            port,
            username,
            password,
            transport: TransportType::Udp,
            response_tx: tx,
        })
        .await
        .expect("engine channel closed unexpectedly");

    // ── Wait for result ─────────────────────────────────────────────────
    match rx.await {
        Ok(Ok(())) => {
            println!();
            println!("  ✅ Registration successful!");
        }
        Ok(Err(e)) => {
            println!();
            println!("  ❌ Registration failed: {e}");
        }
        Err(_) => {
            println!();
            println!("  ❌ Engine dropped the response channel (internal error)");
        }
    }

    // ── Cleanup ─────────────────────────────────────────────────────────
    // Give events a moment to print, then shut down.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let (tx, _rx) = oneshot::channel();
    let _ = handle
        .send_command(CoreCommand::Unregister { response_tx: tx })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let _ = handle.send_command(CoreCommand::Shutdown).await;
    let _ = engine_task.await;
    events_task.abort();

    println!();
    println!("  Done.");
}
