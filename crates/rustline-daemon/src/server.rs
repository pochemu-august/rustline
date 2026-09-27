//! WebSocket server managing incoming client connections.
//!
//! Each connected client:
//! 1. Performs WebSocket handshake.
//! 2. Must authenticate with `{"command": "auth", "token": "..."}` if auth is enabled.
//! 3. Can send JSON commands (`register`, `unregister`, `get_status`).
//! 4. Receives asynchronous push notifications for any `CoreEvent` emitted by the engine.

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use rustline_core::engine::EngineHandle;
use rustline_core::types::*;

use crate::config::Config;
use crate::protocol::{
    self, ClientMessage, EventMessage, ResponseMessage,
};

/// Starts the WebSocket control API server.
pub async fn run_server(
    config: Arc<Config>,
    handle: EngineHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = config.listen_endpoint();
    let listener = TcpListener::bind(&endpoint).await?;
    info!(listen = %endpoint, "WebSocket control API listening");

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(val) => val,
            Err(e) => {
                warn!(error = %e, "failed to accept TCP connection");
                continue;
            }
        };

        let config = Arc::clone(&config);
        let handle = handle.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer_addr, config, handle).await {
                debug!(peer = %peer_addr, error = %e, "client connection closed");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: Arc<Config>,
    engine: EngineHandle,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    debug!(peer = %peer_addr, "new TCP connection, upgrading to WebSocket");
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    info!(peer = %peer_addr, "WebSocket handshake completed");

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Internal channel for messages that must be sent to this specific client
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<String>(64);

    // Task that pumps outgoing messages from outgoing_rx to ws_tx
    let writer_task = tokio::spawn(async move {
        while let Some(text) = outgoing_rx.recv().await {
            if let Err(e) = ws_tx.send(Message::Text(text.into())).await {
                debug!(error = %e, "error sending WebSocket message");
                break;
            }
        }
        let _ = ws_tx.close().await;
    });

    // Authentication stage (if enabled)
    let mut authenticated = !config.auth_required();

    // Subscribe to engine broadcast events
    let mut event_rx = engine.subscribe_events();
    let outgoing_for_events = outgoing_tx.clone();

    // Event forwarder task (forwards CoreEvents -> JSON -> client)
    let event_forwarder = tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let event_msg: EventMessage = protocol::core_event_to_message(&event);
            if let Ok(json_str) = serde_json::to_string(&event_msg) {
                if outgoing_for_events.send(json_str).await.is_err() {
                    break;
                }
            }
        }
    });

    // Main read loop: process incoming messages from the client
    while let Some(msg_result) = ws_rx.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                debug!(peer = %peer_addr, error = %e, "WebSocket read error");
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let text_str = text.as_str();
                debug!(peer = %peer_addr, msg = text_str, "received client message");

                let client_msg: ClientMessage = match serde_json::from_str(text_str) {
                    Ok(m) => m,
                    Err(e) => {
                        let resp = ResponseMessage::err(None, format!("invalid JSON: {e}"));
                        send_response(&outgoing_tx, resp).await;
                        continue;
                    }
                };

                // Check auth
                if !authenticated {
                    match client_msg {
                        ClientMessage::Auth { token } => {
                            if token == config.auth_token {
                                authenticated = true;
                                info!(peer = %peer_addr, "client authenticated successfully");
                                send_response(&outgoing_tx, ResponseMessage::ok(None)).await;
                            } else {
                                warn!(peer = %peer_addr, "client provided invalid auth token");
                                send_response(
                                    &outgoing_tx,
                                    ResponseMessage::err(None, "invalid auth token"),
                                )
                                .await;
                                break;
                            }
                        }
                        _ => {
                            send_response(
                                &outgoing_tx,
                                ResponseMessage::err(
                                    None,
                                    "authentication required: send 'auth' command first",
                                ),
                            )
                            .await;
                            break;
                        }
                    }
                    continue;
                }

                // Client is authenticated, process command
                process_client_command(client_msg, &engine, &outgoing_tx).await;
            }
            Message::Ping(data) => {
                // Tungstenite automatically answers Pongs, but if needed we can handle it
                debug!(peer = %peer_addr, len = data.len(), "received Ping");
            }
            Message::Close(_) => {
                info!(peer = %peer_addr, "client requested connection close");
                break;
            }
            _ => {}
        }
    }

    // Cleanup
    event_forwarder.abort();
    drop(outgoing_tx);
    let _ = writer_task.await;

    info!(peer = %peer_addr, "client disconnected");
    Ok(())
}

async fn process_client_command(
    cmd: ClientMessage,
    engine: &EngineHandle,
    outgoing_tx: &mpsc::Sender<String>,
) {
    match cmd {
        ClientMessage::Auth { .. } => {
            // Already authenticated
            send_response(outgoing_tx, ResponseMessage::ok(None)).await;
        }

        ClientMessage::Register {
            id,
            server,
            port,
            username,
            password,
            transport,
        } => {
            let transport_type = match protocol::parse_transport(&transport) {
                Ok(t) => t,
                Err(e) => {
                    send_response(outgoing_tx, ResponseMessage::err(id, e)).await;
                    return;
                }
            };

            let (tx, rx) = oneshot::channel();
            let core_cmd = CoreCommand::Register {
                server,
                port,
                username,
                password,
                transport: transport_type,
                response_tx: tx,
            };

            if let Err(e) = engine.send_command(core_cmd).await {
                send_response(
                    outgoing_tx,
                    ResponseMessage::err(id, format!("engine channel error: {e}")),
                )
                .await;
                return;
            }

            match rx.await {
                Ok(Ok(())) => {
                    send_response(outgoing_tx, ResponseMessage::ok(id)).await;
                }
                Ok(Err(err)) => {
                    send_response(outgoing_tx, ResponseMessage::err(id, err)).await;
                }
                Err(_) => {
                    send_response(
                        outgoing_tx,
                        ResponseMessage::err(id, "engine dropped response channel"),
                    )
                    .await;
                }
            }
        }

        ClientMessage::Unregister { id } => {
            let (tx, rx) = oneshot::channel();
            let core_cmd = CoreCommand::Unregister { response_tx: tx };

            if let Err(e) = engine.send_command(core_cmd).await {
                send_response(
                    outgoing_tx,
                    ResponseMessage::err(id, format!("engine channel error: {e}")),
                )
                .await;
                return;
            }

            match rx.await {
                Ok(Ok(())) => {
                    send_response(outgoing_tx, ResponseMessage::ok(id)).await;
                }
                Ok(Err(err)) => {
                    send_response(outgoing_tx, ResponseMessage::err(id, err)).await;
                }
                Err(_) => {
                    send_response(
                        outgoing_tx,
                        ResponseMessage::err(id, "engine dropped response channel"),
                    )
                    .await;
                }
            }
        }

        ClientMessage::GetStatus { id } => {
            let (tx, rx) = oneshot::channel();
            let core_cmd = CoreCommand::GetStatus { response_tx: tx };

            if let Err(e) = engine.send_command(core_cmd).await {
                send_response(
                    outgoing_tx,
                    ResponseMessage::err(id, format!("engine channel error: {e}")),
                )
                .await;
                return;
            }

            match rx.await {
                Ok(status) => {
                    let json_data = protocol::status_to_json(&status);
                    send_response(outgoing_tx, ResponseMessage::ok_with_data(id, json_data))
                        .await;
                }
                Err(_) => {
                    send_response(
                        outgoing_tx,
                        ResponseMessage::err(id, "engine dropped response channel"),
                    )
                    .await;
                }
            }
        }

        ClientMessage::Call { id, .. } => {
            send_response(
                outgoing_tx,
                ResponseMessage::err(id, "call command not implemented yet"),
            )
            .await;
        }

        ClientMessage::Answer { id, .. } => {
            send_response(
                outgoing_tx,
                ResponseMessage::err(id, "answer command not implemented yet"),
            )
            .await;
        }

        ClientMessage::Hangup { id, .. } => {
            send_response(
                outgoing_tx,
                ResponseMessage::err(id, "hangup command not implemented yet"),
            )
            .await;
        }
    }
}

async fn send_response(outgoing_tx: &mpsc::Sender<String>, resp: ResponseMessage) {
    if let Ok(json_str) = serde_json::to_string(&resp) {
        let _ = outgoing_tx.send(json_str).await;
    }
}
