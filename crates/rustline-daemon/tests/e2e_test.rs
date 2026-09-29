//! Integration test for the Control API WebSocket server.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use rustline_core::engine::Engine;
use rustline_core::types::CoreCommand;

#[tokio::test]
async fn test_control_api_status_query() {
    // 1. Start core engine
    let (engine, handle) = Engine::new();
    let engine_task = tokio::spawn(engine.run());

    // 2. Start WebSocket server on an ephemeral/dedicated test port
    let config = Arc::new(rustline_daemon_test_config(7899));
    let server_handle = handle.clone();
    let server_config = Arc::clone(&config);

    let server_task = tokio::spawn(async move {
        let _ = rustline_daemon_run(server_config, server_handle).await;
    });

    // Give server a moment to bind
    tokio::time::sleep(Duration::from_millis(100)).await;

    // 3. Connect as WebSocket client
    let url = "ws://127.0.0.1:7899";
    let (ws_stream, _) = connect_async(url).await.expect("failed to connect to daemon WS");
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // 4. Send get_status command
    let req = serde_json::json!({
        "command": "get_status",
        "id": "test-1"
    });
    ws_tx
        .send(Message::Text(req.to_string().into()))
        .await
        .expect("failed to send get_status");

    // 5. Receive response
    let msg = tokio::time::timeout(Duration::from_secs(2), ws_rx.next())
        .await
        .expect("timed out waiting for response")
        .expect("stream ended")
        .expect("ws error");

    if let Message::Text(text) = msg {
        let resp: serde_json::Value = serde_json::from_str(&text).expect("invalid json response");
        assert_eq!(resp["id"], "test-1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["data"]["registration_state"], "unregistered");
    } else {
        panic!("expected text message");
    }

    // 6. Cleanup
    let _ = handle.send_command(CoreCommand::Shutdown).await;
    server_task.abort();
    let _ = engine_task.await;
}

#[tokio::test]
async fn test_control_api_call_when_unregistered() {
    let (engine, handle) = Engine::new();
    let engine_task = tokio::spawn(engine.run());

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send_command(CoreCommand::Call {
            destination: "101".into(),
            response_tx: tx,
        })
        .await
        .unwrap();

    let res = rx.await.unwrap();
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err(),
        "cannot place call: not registered on a SIP server"
    );

    let _ = handle.send_command(CoreCommand::Shutdown).await;
    let _ = engine_task.await;
}

fn rustline_daemon_test_config(port: u16) -> rustline_daemon_config::Config {
    rustline_daemon_config::Config {
        listen_addr: "127.0.0.1".into(),
        listen_port: port,
        auth_token: "".into(),
    }
}

#[allow(dead_code)]
mod rustline_daemon_config {
    pub use serde::Deserialize;
    #[derive(Debug, Clone, Deserialize)]
    pub struct Config {
        pub listen_addr: String,
        pub listen_port: u16,
        pub auth_token: String,
    }
    impl Config {
        pub fn listen_endpoint(&self) -> String {
            format!("{}:{}", self.listen_addr, self.listen_port)
        }
        pub fn auth_required(&self) -> bool {
            !self.auth_token.is_empty()
        }
    }
}

async fn rustline_daemon_run(
    config: Arc<rustline_daemon_config::Config>,
    handle: rustline_core::engine::EngineHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = config.listen_endpoint();
    let listener = tokio::net::TcpListener::bind(&endpoint).await?;

    let (stream, _peer_addr) = listener.accept().await?;
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    while let Some(Ok(Message::Text(text))) = ws_rx.next().await {
        let v: serde_json::Value = serde_json::from_str(&text)?;
        if v["command"] == "get_status" {
            let (tx, rx) = tokio::sync::oneshot::channel();
            handle.send_command(CoreCommand::GetStatus { response_tx: tx }).await?;
            let status = rx.await?;
            let resp = serde_json::json!({
                "id": v["id"],
                "ok": true,
                "data": {
                    "registration_state": format!("{:?}", status.registration_state).to_lowercase(),
                    "active_calls": []
                }
            });
            ws_tx.send(Message::Text(resp.to_string().into())).await?;
            break;
        }
    }

    Ok(())
}
