//! WebSocket connection manager.
//!
//! Handles connection lifecycle, automatic reconnection with exponential backoff,
//! and subscription restoration after reconnection.

use crate::error::{WsError, WsResult};
use crate::heartbeat::HeartbeatManager;
use crate::message::{WsMessage, WsRequest};
use crate::rate_limiter::RateLimiter;
use crate::subscription::{ReadyState, SubscriptionManager};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// Subscription target for a market.
#[derive(Debug, Clone)]
pub struct SubscriptionTarget {
    /// Coin symbol (e.g., "BTC", "ETH", "SOL").
    pub coin: String,
    /// Asset index for internal tracking.
    pub asset_idx: u16,
}

/// Connection configuration.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// WebSocket URL.
    pub url: String,
    /// Maximum reconnection attempts (0 = infinite).
    pub max_reconnect_attempts: u32,
    /// Base delay for exponential backoff.
    pub reconnect_base_delay_ms: u64,
    /// Maximum delay for exponential backoff.
    pub reconnect_max_delay_ms: u64,
    /// Heartbeat interval.
    pub heartbeat_interval_ms: u64,
    /// Heartbeat timeout (pong must arrive within this).
    pub heartbeat_timeout_ms: u64,
    /// Markets to subscribe to (coin symbols with asset indices).
    pub subscriptions: Vec<SubscriptionTarget>,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_reconnect_attempts: 0, // Infinite
            reconnect_base_delay_ms: 1000,
            reconnect_max_delay_ms: 60000,
            heartbeat_interval_ms: 45000,
            heartbeat_timeout_ms: 10000,
            subscriptions: Vec::new(),
        }
    }
}

/// Connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

/// WebSocket connection manager.
pub struct ConnectionManager {
    config: ConnectionConfig,
    state: Arc<RwLock<ConnectionState>>,
    subscriptions: Arc<SubscriptionManager>,
    /// Rate limiter for Phase B (execution). Reserved for future use.
    _rate_limiter: Arc<RateLimiter>,
    heartbeat: Arc<HeartbeatManager>,
    message_tx: mpsc::Sender<WsMessage>,
    reconnect_count: Arc<RwLock<u32>>,
}

impl ConnectionManager {
    /// Create a new connection manager.
    pub fn new(config: ConnectionConfig, message_tx: mpsc::Sender<WsMessage>) -> Self {
        Self {
            config: config.clone(),
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            subscriptions: Arc::new(SubscriptionManager::new()),
            _rate_limiter: Arc::new(RateLimiter::new(2000, 60)), // 2000 msg/min
            heartbeat: Arc::new(HeartbeatManager::new(
                config.heartbeat_interval_ms,
                config.heartbeat_timeout_ms,
            )),
            message_tx,
            reconnect_count: Arc::new(RwLock::new(0)),
        }
    }

    /// Get current connection state.
    pub fn state(&self) -> ConnectionState {
        *self.state.read()
    }

    /// Get ready state (all subscriptions ready).
    pub fn ready_state(&self) -> ReadyState {
        self.subscriptions.ready_state()
    }

    /// Check if connection is ready for trading.
    pub fn is_ready(&self) -> bool {
        self.state() == ConnectionState::Connected && self.subscriptions.is_ready()
    }

    /// Connect to WebSocket and run message loop.
    pub async fn connect(&self) -> WsResult<()> {
        self.connect_with_retry().await
    }

    async fn connect_with_retry(&self) -> WsResult<()> {
        let mut attempt = 0u32;

        loop {
            *self.state.write() = ConnectionState::Connecting;

            match self.try_connect().await {
                Ok(()) => {
                    // Connection closed normally
                    info!("WebSocket connection closed");
                }
                Err(e) => {
                    error!(?e, "WebSocket connection error");
                }
            }

            // Check if we should reconnect
            attempt += 1;
            *self.reconnect_count.write() = attempt;

            if self.config.max_reconnect_attempts > 0
                && attempt >= self.config.max_reconnect_attempts
            {
                error!(attempt, "Max reconnection attempts reached");
                return Err(WsError::ConnectionFailed(
                    "Max reconnection attempts reached".to_string(),
                ));
            }

            *self.state.write() = ConnectionState::Reconnecting;

            // Calculate backoff delay with jitter
            let delay = self.calculate_backoff_delay(attempt);
            warn!(attempt, delay_ms = delay.as_millis(), "Reconnecting");
            tokio::time::sleep(delay).await;

            // Reset subscriptions ready state
            self.subscriptions.reset_ready_state();
        }
    }

    async fn try_connect(&self) -> WsResult<()> {
        info!(url = %self.config.url, "Connecting to WebSocket");

        let (ws_stream, _response) = connect_async(&self.config.url).await?;
        let (mut write, mut read) = ws_stream.split();

        *self.state.write() = ConnectionState::Connected;
        *self.reconnect_count.write() = 0;
        info!("WebSocket connected");

        // Restore subscriptions (pass both write and read to handle responses)
        self.restore_subscriptions(&mut write, &mut read).await?;

        // Start heartbeat
        self.heartbeat.reset();

        // Message loop
        loop {
            tokio::select! {
                // Incoming message
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            self.handle_text_message(&text).await?;
                        }
                        Some(Ok(Message::Ping(data))) => {
                            debug!("Received ping, sending pong");
                            write.send(Message::Pong(data)).await?;
                        }
                        Some(Ok(Message::Pong(_))) => {
                            debug!("Received pong");
                            self.heartbeat.record_pong();
                        }
                        Some(Ok(Message::Close(frame))) => {
                            let (code, reason) = frame
                                .map(|f| (f.code.into(), f.reason.to_string()))
                                .unwrap_or((1000, "Normal close".to_string()));
                            warn!(code, %reason, "WebSocket closed by server");
                            return Err(WsError::ConnectionClosed { code, reason });
                        }
                        Some(Err(e)) => {
                            error!(?e, "WebSocket read error");
                            return Err(e.into());
                        }
                        None => {
                            warn!("WebSocket stream ended");
                            return Ok(());
                        }
                        _ => {}
                    }
                }

                // Heartbeat check
                _ = self.heartbeat.wait_for_check() => {
                    if self.heartbeat.is_timed_out() {
                        error!("Heartbeat timeout");
                        return Err(WsError::HeartbeatTimeout);
                    }

                    // P0-3: Only send ping if actually needed (not waiting for pong and no recent messages)
                    if self.heartbeat.should_send_heartbeat() {
                        let ping = WsRequest::ping();
                        let msg = serde_json::to_string(&ping)?;
                        write.send(Message::Text(msg)).await?;
                        self.heartbeat.record_ping();
                        debug!("Sent heartbeat ping");
                    }
                }
            }
        }
    }

    async fn handle_text_message(&self, text: &str) -> WsResult<()> {
        self.heartbeat.record_message();

        // Parse message
        let msg: WsMessage = serde_json::from_str(text)?;

        // Handle different message types
        match &msg {
            WsMessage::Pong(pong_msg) => {
                // Application-level pong from Hyperliquid
                if pong_msg.is_pong() {
                    debug!("Received application-level pong");
                    self.heartbeat.record_pong();
                }
                // Don't forward pong to message channel
                return Ok(());
            }
            WsMessage::Channel(channel_msg) => {
                // Update subscription state
                self.subscriptions.handle_message(&channel_msg.channel);
            }
            WsMessage::Response(response_msg) => {
                // Subscription response or other responses
                debug!(channel = %response_msg.channel, "Received response");
            }
        }

        // Forward data messages to message channel
        if self.message_tx.send(msg).await.is_err() {
            warn!("Message receiver dropped");
        }

        Ok(())
    }

    async fn restore_subscriptions(
        &self,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        read: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) -> WsResult<()> {
        info!(
            count = self.config.subscriptions.len(),
            "Restoring subscriptions"
        );

        // Wait a moment after connection before sending subscriptions
        tokio::time::sleep(Duration::from_millis(1000)).await;
        info!("Starting subscriptions after initial delay");

        let total_subs = self.config.subscriptions.len() * 2; // bbo + activeAssetCtx per target
        let mut subs_sent = 0;

        for target in self.config.subscriptions.iter() {
            // Subscribe to BBO for this coin
            let bbo_sub = serde_json::json!({
                "type": "bbo",
                "coin": target.coin
            });
            let bbo_req = WsRequest::subscribe(bbo_sub);
            let bbo_msg = serde_json::to_string(&bbo_req)?;
            write.send(Message::Text(bbo_msg)).await?;
            subs_sent += 1;

            // Track subscriptions
            self.subscriptions
                .add_subscription(format!("bbo:{}", target.coin));

            // Drain response and wait before next subscription
            self.drain_and_wait(write, read, 100).await?;

            // Subscribe to activeAssetCtx for this coin
            let ctx_sub = serde_json::json!({
                "type": "activeAssetCtx",
                "coin": target.coin
            });
            let ctx_req = WsRequest::subscribe(ctx_sub);
            let ctx_msg = serde_json::to_string(&ctx_req)?;
            write.send(Message::Text(ctx_msg)).await?;
            subs_sent += 1;

            self.subscriptions
                .add_subscription(format!("activeAssetCtx:{}", target.coin));

            // Drain response and wait before next subscription
            self.drain_and_wait(write, read, 100).await?;

            if subs_sent % 10 == 0 {
                info!(
                    progress = format!("{}/{}", subs_sent, total_subs),
                    coin = %target.coin,
                    "Subscription progress"
                );
            }
        }

        info!(
            total = total_subs,
            "All subscriptions sent and responses drained"
        );
        Ok(())
    }

    /// Drain pending messages and wait for a short duration.
    /// This prevents buffer overflow by reading responses between sends.
    async fn drain_and_wait(
        &self,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        read: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
        wait_ms: u64,
    ) -> WsResult<()> {
        let drain_timeout = Duration::from_millis(wait_ms);
        let drain_start = std::time::Instant::now();

        loop {
            let remaining = drain_timeout.saturating_sub(drain_start.elapsed());
            if remaining.is_zero() {
                break;
            }

            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            // Forward to message channel if it's data
                            if let Ok(ws_msg) = serde_json::from_str::<WsMessage>(&text) {
                                if !matches!(ws_msg, WsMessage::Pong(_)) {
                                    let _ = self.message_tx.send(ws_msg).await;
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            write.send(Message::Pong(data)).await?;
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(frame))) => {
                            let (code, reason) = frame
                                .map(|f| (f.code.into(), f.reason.to_string()))
                                .unwrap_or((1000, "Close during subscription".to_string()));
                            warn!(code, %reason, "WebSocket closed during subscription");
                            return Err(WsError::ConnectionClosed { code, reason });
                        }
                        Some(Err(e)) => {
                            error!(?e, "Error during subscription drain");
                            return Err(e.into());
                        }
                        None => {
                            warn!("WebSocket stream ended during subscription");
                            return Err(WsError::ConnectionClosed {
                                code: 1006,
                                reason: "Stream ended during subscription".to_string(),
                            });
                        }
                        _ => {}
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    // No more messages pending, wait the remaining time then continue
                    let remaining = drain_timeout.saturating_sub(drain_start.elapsed());
                    if !remaining.is_zero() {
                        tokio::time::sleep(remaining).await;
                    }
                    break;
                }
            }
        }

        Ok(())
    }

    fn calculate_backoff_delay(&self, attempt: u32) -> Duration {
        let base = self.config.reconnect_base_delay_ms;
        let max = self.config.reconnect_max_delay_ms;

        // P2-3: Exponential backoff: base * 2^(attempt-1)
        // attempt=1 -> base * 2^0 = base
        // attempt=2 -> base * 2^1 = 2*base
        // attempt=3 -> base * 2^2 = 4*base
        let exponent = attempt.saturating_sub(1).min(10);
        let delay = base.saturating_mul(1u64 << exponent);
        let delay = delay.min(max);

        // Add jitter (0-1000ms)
        let jitter = rand_jitter();
        Duration::from_millis(delay + jitter)
    }
}

/// Generate random jitter (0-1000ms).
fn rand_jitter() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    (nanos % 1000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ConnectionConfig::default();
        assert_eq!(config.max_reconnect_attempts, 0); // Infinite
        assert_eq!(config.heartbeat_interval_ms, 45000);
    }
}
