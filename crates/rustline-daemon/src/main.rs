//! Rustline daemon binary (`rustlined`).
//!
//! Spawns the SIP core engine in the background and runs a WebSocket server
//! allowing any external client (CLI, TUI, GUI, Web) to control telephony
//! and receive push notifications in real time.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use rustline_core::engine::Engine;
use rustline_core::types::CoreCommand;

mod config;
mod protocol;
mod server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging: RUST_LOG=info (or debug for verbose packets)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,rustline_daemon=debug,rustline_core=debug")),
        )
        .with_target(false)
        .init();

    info!("Starting Rustline Daemon (rustlined)...");

    // Load config from file argument or default 'rustline.json'
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rustline.json"));

    let config = Arc::new(config::Config::load(&config_path)?);

    // 1. Initialize core engine
    let (engine, handle) = Engine::new();
    let engine_task = tokio::spawn(engine.run());

    // 2. Clone handle for WebSocket server
    let server_handle = handle.clone();
    let server_config = Arc::clone(&config);

    let server_task = tokio::spawn(async move {
        if let Err(e) = server::run_server(server_config, server_handle).await {
            error!(error = %e, "WebSocket server stopped unexpectedly");
        }
    });

    info!(
        endpoint = %config.listen_endpoint(),
        auth = if config.auth_required() { "enabled" } else { "disabled" },
        "Rustline daemon is ready and accepting WebSocket connections"
    );

    // 3. Graceful shutdown on Ctrl+C
    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received, shutting down gracefully...");

    let _ = handle.send_command(CoreCommand::Shutdown).await;
    server_task.abort();
    let _ = engine_task.await;

    info!("Rustline daemon exited cleanly.");
    Ok(())
}
