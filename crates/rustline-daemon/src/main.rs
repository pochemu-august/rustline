//! # rustline-daemon
//!
//! The headless WebSocket JSON-RPC server for the rustline softphone.
//!
//! Listens on `ws://127.0.0.1:7890` and routes JSON-RPC commands
//! to the [`rustline_core::Engine`], broadcasting events to all
//! connected UI clients.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use rustline_core::engine::{Engine, EngineEvent};
use rustline_proto::messages::*;

/// Default bind address for the WebSocket server.
const BIND_ADDR: &str = "127.0.0.1:7890";

/// A unique ID for each connected WebSocket client.
type ClientId = u64;

/// Sender half for pushing messages to a specific client.
type ClientSender = mpsc::UnboundedSender<Message>;

/// Shared state: the engine + connected clients.
struct AppState {
    engine: Mutex<Engine>,
    clients: RwLock<HashMap<ClientId, ClientSender>>,
    next_client_id: Mutex<ClientId>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing (logs to stderr).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustline_daemon=info,rustline_core=info".into()),
        )
        .init();

    info!("rustline-daemon v{}", env!("CARGO_PKG_VERSION"));
    info!(
        "Media subsystem available: {}",
        rustline_media::is_available()
    );

    // Create the engine ↔ daemon event channel.
    let (event_tx, event_rx) = mpsc::unbounded_channel::<EngineEvent>();

    let state = Arc::new(AppState {
        engine: Mutex::new(Engine::new(event_tx)),
        clients: RwLock::new(HashMap::new()),
        next_client_id: Mutex::new(1),
    });

    // Spawn the event broadcaster.
    let state_clone = Arc::clone(&state);
    tokio::spawn(event_broadcaster(state_clone, event_rx));

    // Bind the TCP listener.
    let listener = TcpListener::bind(BIND_ADDR).await?;
    info!("WebSocket server listening on ws://{}", BIND_ADDR);

    loop {
        let (stream, addr) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(handle_connection(state, stream, addr));
    }
}

/// Broadcast engine events to all connected WebSocket clients.
async fn event_broadcaster(
    state: Arc<AppState>,
    mut event_rx: mpsc::UnboundedReceiver<EngineEvent>,
) {
    while let Some(event) = event_rx.recv().await {
        match event {
            EngineEvent::Broadcast(evt) => {
                let notification = JsonRpcNotification::new(
                    "event",
                    serde_json::to_value(&evt).unwrap_or(Value::Null),
                );
                let msg_text = serde_json::to_string(&notification).unwrap_or_default();
                let msg = Message::Text(msg_text.into());

                let clients = state.clients.read().await;
                for (id, sender) in clients.iter() {
                    if sender.send(msg.clone()).is_err() {
                        warn!(client_id = id, "Failed to send event, client disconnected");
                    }
                }
            }
        }
    }
}

/// Handle a single WebSocket connection.
async fn handle_connection(state: Arc<AppState>, stream: TcpStream, addr: SocketAddr) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            error!("WebSocket handshake failed for {}: {}", addr, e);
            return;
        }
    };

    // Assign a client ID.
    let client_id = {
        let mut id = state.next_client_id.lock().await;
        let cid = *id;
        *id += 1;
        cid
    };

    info!(client_id, %addr, "Client connected");

    // Create an mpsc channel for sending messages to this client.
    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<Message>();

    // Register the client.
    {
        let mut clients = state.clients.write().await;
        clients.insert(client_id, client_tx);
    }

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Spawn a task to forward messages from our channel to the WebSocket.
    let send_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Process incoming messages from the WebSocket.
    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!(client_id, "WebSocket error: {}", e);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let response = process_request(&state, &text).await;
                let response_text = serde_json::to_string(&response).unwrap_or_else(|_| {
                    r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal serialization error"}}"#.to_string()
                });

                // Send response directly to this client.
                let clients = state.clients.read().await;
                if let Some(sender) = clients.get(&client_id) {
                    let _ = sender.send(Message::Text(response_text.into()));
                }
            }
            Message::Ping(data) => {
                let clients = state.clients.read().await;
                if let Some(sender) = clients.get(&client_id) {
                    let _ = sender.send(Message::Pong(data));
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Cleanup.
    info!(client_id, %addr, "Client disconnected");
    {
        let mut clients = state.clients.write().await;
        clients.remove(&client_id);
    }
    send_task.abort();
}

/// Parse a JSON-RPC request and dispatch it to the engine.
async fn process_request(state: &AppState, text: &str) -> Value {
    // 1. Parse the JSON.
    let request: JsonRpcRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            return serde_json::to_value(JsonRpcErrorResponse::new(
                Value::Null,
                PARSE_ERROR,
                format!("Parse error: {e}"),
            ))
            .unwrap_or(Value::Null);
        }
    };

    let id = request.id.clone();

    // 2. Dispatch by method name.
    let result = {
        let mut engine = state.engine.lock().await;

        match request.method.as_str() {
            "register" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_register(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "unregister" => engine.handle_unregister().await,
            "dial" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_dial(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "answer" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_answer(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "hangup" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_hangup(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "hold" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_hold(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "mute_mic" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_mute_mic(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "mute_speaker" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_mute_speaker(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "dtmf" => match serde_json::from_value(request.params) {
                Ok(params) => engine.handle_dtmf(params).await,
                Err(e) => Err(format!("Invalid params: {e}")),
            },
            "get_status" => engine.handle_get_status().await,
            other => Err(format!("Unknown method: {other}")),
        }
    };

    // 3. Build the response.
    match result {
        Ok(value) => {
            serde_json::to_value(JsonRpcResponse::success(id, value)).unwrap_or(Value::Null)
        }
        Err(msg) => {
            let code = if msg.starts_with("Unknown method") {
                METHOD_NOT_FOUND
            } else if msg.starts_with("Invalid params") {
                INVALID_PARAMS
            } else {
                INTERNAL_ERROR
            };
            serde_json::to_value(JsonRpcErrorResponse::new(id, code, msg)).unwrap_or(Value::Null)
        }
    }
}
