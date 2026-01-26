//! Mock WebSocket server for integration tests.
//!
//! Provides a simple WebSocket server that can:
//! - Accept connections
//! - Echo subscription confirmations
//! - Record received messages

use futures_util::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

/// A mock WebSocket server for testing.
pub struct MockWsServer {
    addr: SocketAddr,
    shutdown_tx: mpsc::Sender<()>,
    messages: Arc<Mutex<VecDeque<String>>>,
    connections: Arc<Mutex<u32>>,
}

impl MockWsServer {
    /// Start a new mock WebSocket server on an available port.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let messages: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let connections: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        let messages_clone = messages.clone();
        let connections_clone = connections.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Ok((stream, _)) = listener.accept() => {
                        let messages = messages_clone.clone();
                        let connections = connections_clone.clone();
                        tokio::spawn(handle_connection(stream, messages, connections));
                    }
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        });

        Self {
            addr,
            shutdown_tx,
            messages,
            connections,
        }
    }

    /// Get the server's WebSocket URL.
    pub fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Get the number of connections received.
    pub async fn connection_count(&self) -> u32 {
        *self.connections.lock().await
    }

    /// Get all received messages.
    pub async fn received_messages(&self) -> Vec<String> {
        self.messages.lock().await.iter().cloned().collect()
    }

    /// Shutdown the server.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(()).await;
    }
}

async fn handle_connection(
    stream: TcpStream,
    messages: Arc<Mutex<VecDeque<String>>>,
    connections: Arc<Mutex<u32>>,
) {
    // Increment connection count
    {
        let mut count = connections.lock().await;
        *count += 1;
    }

    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("WebSocket handshake failed: {}", e);
            return;
        }
    };

    let (mut write, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // Record the message
                {
                    let mut msgs = messages.lock().await;
                    msgs.push_back(text.clone());
                }

                // Parse and respond to subscriptions
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    if parsed.get("method") == Some(&serde_json::json!("subscribe")) {
                        // Echo subscription confirmation
                        if let Some(subscription) = parsed.get("subscription") {
                            let response = serde_json::json!({
                                "channel": "subscriptionResponse",
                                "data": {
                                    "method": "subscribe",
                                    "subscription": subscription
                                }
                            });
                            let _ = write.send(Message::Text(response.to_string())).await;
                        }
                    }
                }
            }
            Ok(Message::Ping(data)) => {
                let _ = write.send(Message::Pong(data)).await;
            }
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_server_starts() {
        let server = MockWsServer::start().await;
        assert!(server.url().starts_with("ws://127.0.0.1:"));
        server.shutdown().await;
    }
}
