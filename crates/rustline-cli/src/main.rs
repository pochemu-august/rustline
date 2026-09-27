//! Reference CLI client for RustlineCore Control API.
//!
//! Connects to `ws://127.0.0.1:7890` (or specified URL), allows interactive
//! input, and prints all push events emitted by the daemon in real time.

use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

static REQ_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    REQ_COUNTER.fetch_add(1, Ordering::SeqCst).to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ws_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:7890".to_string());

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Rustline Reference CLI Client                               ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Connecting to: {:<45}║", ws_url);
    println!("╚══════════════════════════════════════════════════════════════╝");

    let (ws_stream, _) = match connect_async(&ws_url).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("\n❌ Failed to connect to daemon at {ws_url}: {e}");
            eprintln!("   Make sure 'rustlined' is running (cargo run -p rustline-daemon)");
            std::process::exit(1);
        }
    };

    println!("\n✅ Connected to daemon!\n");
    print_help();

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Background task: listen for incoming messages and print them
    let reader_task = tokio::spawn(async move {
        while let Some(msg_result) = ws_rx.next().await {
            match msg_result {
                Ok(Message::Text(text)) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(event_name) = v.get("event").and_then(|e| e.as_str()) {
                            println!("\n📡 [EVENT: {}] {}", event_name, text);
                        } else if let Some(ok) = v.get("ok").and_then(|o| o.as_bool()) {
                            if ok {
                                println!("\n✅ [RESPONSE] {}", text);
                            } else {
                                println!("\n❌ [ERROR RESPONSE] {}", text);
                            }
                        } else {
                            println!("\n📩 [MESSAGE] {}", text);
                        }
                    } else {
                        println!("\n📩 [RAW] {}", text);
                    }
                    print!("rustline> ");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                Ok(Message::Close(_)) => {
                    println!("\n🔌 Connection closed by daemon.");
                    break;
                }
                Err(e) => {
                    eprintln!("\n⚠️ WebSocket read error: {e}");
                    break;
                }
                _ => {}
            }
        }
    });

    // Read interactive commands from stdin
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    print!("rustline> ");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            print!("rustline> ");
            let _ = std::io::stdout().flush();
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let cmd = parts[0].to_ascii_lowercase();

        match cmd.as_str() {
            "help" | "?" => {
                print_help();
            }

            "auth" => {
                if parts.len() < 2 {
                    println!("Usage: auth <token>");
                } else {
                    let msg = json!({
                        "command": "auth",
                        "token": parts[1]
                    });
                    send_json(&mut ws_tx, msg).await?;
                }
            }

            "register" | "reg" => {
                if parts.len() < 5 {
                    println!("Usage: register <server> <port> <username> <password> [transport]");
                    println!("  Example: register 192.168.0.104 5060 100 100password udp");
                } else {
                    let server = parts[1];
                    let port: u16 = parts[2].parse().unwrap_or(5060);
                    let username = parts[3];
                    let password = parts[4];
                    let transport = if parts.len() >= 6 { parts[5] } else { "udp" };

                    let msg = json!({
                        "command": "register",
                        "id": next_id(),
                        "server": server,
                        "port": port,
                        "username": username,
                        "password": password,
                        "transport": transport
                    });
                    send_json(&mut ws_tx, msg).await?;
                }
            }

            "unregister" | "unreg" => {
                let msg = json!({
                    "command": "unregister",
                    "id": next_id()
                });
                send_json(&mut ws_tx, msg).await?;
            }

            "status" => {
                let msg = json!({
                    "command": "get_status",
                    "id": next_id()
                });
                send_json(&mut ws_tx, msg).await?;
            }

            "raw" => {
                // Send raw JSON string after "raw "
                if parts.len() > 1 {
                    let raw_str = &trimmed[4..].trim();
                    let _ = ws_tx.send(Message::Text((*raw_str).into())).await;
                } else {
                    println!("Usage: raw {{\"command\": \"...\"}}");
                }
            }

            "exit" | "quit" => {
                println!("Exiting...");
                break;
            }

            other => {
                println!("Unknown command: '{}'. Type 'help' for available commands.", other);
            }
        }

        print!("rustline> ");
        let _ = std::io::stdout().flush();
    }

    reader_task.abort();
    let _ = ws_tx.close().await;
    Ok(())
}

async fn send_json<S>(ws_tx: &mut S, value: serde_json::Value) -> Result<(), Box<dyn std::error::Error>>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::error::Error + 'static,
{
    let text = serde_json::to_string(&value)?;
    ws_tx.send(Message::Text(text.into())).await?;
    Ok(())
}

fn print_help() {
    println!("Available commands:");
    println!("  register <server> <port> <user> <pass> [udp|tls]  - Register on SIP server");
    println!("  unregister                                       - Unregister from SIP server");
    println!("  status                                           - Query daemon registration & call status");
    println!("  auth <token>                                     - Authenticate if daemon requires token");
    println!("  raw <json>                                       - Send raw JSON message");
    println!("  help / ?                                         - Show this help");
    println!("  exit / quit                                      - Disconnect and exit");
    println!();
}
