//! WebSocket connection manager.
//!
//! Handles connection lifecycle, automatic reconnection with exponential backoff,
//! and subscription restoration after reconnection.

use crate::error::{WsError, WsResult};
use crate::heartbeat::HeartbeatManager;
use crate::message::{extract_subscription_type, WsMessage, WsRequest};
use crate::rate_limiter::RateLimiter;
use crate::subscription::{ReadyState, SubscriptionManager};
use crate::ws_write_handle::{WsOutbound, WsWriteHandle};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Subscription target for a market.
#[derive(Debug, Clone)]
pub struct SubscriptionTarget {
    /// Coin symbol (e.g., "BTC", "ETH", "xyz:SILVER").
    pub coin: String,
    /// Asset index for internal tracking.
    /// For xyz markets: 100000 + perp_dex_id * 10000 + local_index.
    pub asset_idx: u32,
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
    /// User address for trading subscriptions (orderUpdates, userFills).
    /// If None, trading subscriptions are skipped and READY-TRADING cannot be achieved.
    pub user_address: Option<String>,
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
            user_address: None,
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
    /// Rate limiter for Phase B (execution).
    rate_limiter: Arc<RateLimiter>,
    heartbeat: Arc<HeartbeatManager>,
    message_tx: mpsc::Sender<WsMessage>,
    reconnect_count: Arc<RwLock<u32>>,
    /// Outbound message sender (for WsWriteHandle).
    outbound_tx: mpsc::Sender<WsOutbound>,
    /// Outbound message receiver (consumed by message loop).
    outbound_rx: Arc<TokioMutex<mpsc::Receiver<WsOutbound>>>,
    /// Cancellation token for graceful shutdown.
    shutdown_token: CancellationToken,
}

impl ConnectionManager {
    /// Create a new connection manager.
    pub fn new(config: ConnectionConfig, message_tx: mpsc::Sender<WsMessage>) -> Self {
        let (outbound_tx, outbound_rx) = mpsc::channel(100);
        Self {
            config: config.clone(),
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            subscriptions: Arc::new(SubscriptionManager::new()),
            rate_limiter: Arc::new(RateLimiter::new(2000, 60)), // 2000 msg/min
            heartbeat: Arc::new(HeartbeatManager::new(
                config.heartbeat_interval_ms,
                config.heartbeat_timeout_ms,
            )),
            message_tx,
            reconnect_count: Arc::new(RwLock::new(0)),
            outbound_tx,
            outbound_rx: Arc::new(TokioMutex::new(outbound_rx)),
            shutdown_token: CancellationToken::new(),
        }
    }

    /// Get a write handle for sending messages.
    ///
    /// The write handle can be cloned and shared across tasks.
    /// It provides a channel-based API that is reconnect-safe.
    pub fn write_handle(&self) -> WsWriteHandle {
        WsWriteHandle::new(
            self.outbound_tx.clone(),
            self.rate_limiter.clone(),
            self.state.clone(),
            self.subscriptions.clone(),
        )
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

    /// Signal graceful shutdown.
    ///
    /// Cancels the shutdown token, which will cause both the message loop
    /// and reconnect loop to exit promptly.
    pub fn shutdown(&self) {
        info!("ConnectionManager shutdown requested");
        self.shutdown_token.cancel();
    }

    /// Check if shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown_token.is_cancelled()
    }

    /// Connect to WebSocket and run message loop.
    pub async fn connect(&self) -> WsResult<()> {
        self.connect_with_retry().await
    }

    async fn connect_with_retry(&self) -> WsResult<()> {
        let mut attempt = 0u32;

        loop {
            // Check shutdown flag at start of loop
            if self.is_shutdown() {
                info!("Shutdown requested, exiting connect loop");
                *self.state.write() = ConnectionState::Disconnected;
                return Ok(());
            }

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

            // Check shutdown flag before reconnect attempt
            if self.is_shutdown() {
                info!("Shutdown requested after disconnect, not reconnecting");
                *self.state.write() = ConnectionState::Disconnected;
                return Ok(());
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

            // Wait for delay OR shutdown signal (cancellation-aware sleep)
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = self.shutdown_token.cancelled() => {
                    info!("Shutdown requested during backoff, exiting");
                    *self.state.write() = ConnectionState::Disconnected;
                    return Ok(());
                }
            }

            // Reset subscriptions ready state
            self.subscriptions.reset_ready_state();
            // Reset inflight counter (pending posts will never get responses)
            self.rate_limiter.reset_inflight();
        }
    }

    async fn try_connect(&self) -> WsResult<()> {
        info!(url = %self.config.url, "Connecting to WebSocket");

        // P2-8: TCP_NODELAY for lower latency (disable Nagle's algorithm)
        let (ws_stream, _response) =
            connect_async_tls_with_config(&self.config.url, None, true, None).await?;
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
            // Lock outbound_rx for the select! block
            let outbound_recv = async { self.outbound_rx.lock().await.recv().await };

            tokio::select! {
                // Shutdown signal - highest priority (biased)
                () = self.shutdown_token.cancelled() => {
                    info!("Shutdown signal received in message loop");
                    // Send WebSocket Close frame for graceful disconnect
                    if let Err(e) = write.send(Message::Close(None)).await {
                        warn!(?e, "Failed to send Close frame during shutdown");
                    }
                    // Reset inflight on shutdown
                    self.rate_limiter.reset_inflight();
                    *self.state.write() = ConnectionState::Disconnected;
                    return Ok(());
                }

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
                            // Reset inflight on disconnect
                            self.rate_limiter.reset_inflight();
                            return Err(WsError::ConnectionClosed { code, reason });
                        }
                        Some(Err(e)) => {
                            error!(?e, "WebSocket read error");
                            // Reset inflight on error
                            self.rate_limiter.reset_inflight();
                            return Err(e.into());
                        }
                        None => {
                            warn!("WebSocket stream ended");
                            // Reset inflight on stream end
                            self.rate_limiter.reset_inflight();
                            return Ok(());
                        }
                        _ => {}
                    }
                }

                // Outbound message
                outbound = outbound_recv => {
                    if let Some(msg) = outbound {
                        match msg {
                            WsOutbound::Text(text) => {
                                write.send(Message::Text(text)).await?;
                            }
                            WsOutbound::Post { post_id, payload } => {
                                write.send(Message::Text(payload)).await?;
                                debug!(post_id, "Post sent to WebSocket");
                            }
                        }
                    }
                }

                // Heartbeat check
                _ = self.heartbeat.wait_for_check() => {
                    if self.heartbeat.is_timed_out() {
                        error!("Heartbeat timeout");
                        // Reset inflight on heartbeat timeout
                        self.rate_limiter.reset_inflight();
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
                // Handle subscriptionResponse: internal ACK only
                if channel_msg.channel == "subscriptionResponse" {
                    debug!(?channel_msg.data, "Received subscription response");

                    // Process ACK using extracted function
                    // Note: Arc<SubscriptionManager> から &SubscriptionManager を取得
                    process_subscription_response(&channel_msg.data, &self.subscriptions);

                    // IMPORTANT: subscriptionResponse は内部 ACK 専用
                    // → message_tx.send() には送らない（downstream 転送しない）
                    return Ok(());
                }

                // Handle error channel (log only, but still forward downstream)
                if channel_msg.channel == "error" {
                    warn!(?channel_msg.data, "Received error channel message");
                    // Note: error は forward する（アプリ側で処理する可能性あり）
                }

                // Decrement inflight on post response (transport responsibility)
                if channel_msg.channel == "post" {
                    self.rate_limiter.record_post_response();
                    debug!("Post response received, inflight decremented");
                }
                // Update subscription state for market data channels
                self.subscriptions.handle_message(&channel_msg.channel);
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
            "All market data subscriptions sent and responses drained"
        );

        // Subscribe to trading channels if user_address is configured
        if let Some(ref user_address) = self.config.user_address {
            self.subscribe_trading_channels(write, read, user_address)
                .await?;
        } else {
            info!("No user_address configured, skipping trading subscriptions");
        }

        Ok(())
    }

    /// Subscribe to orderUpdates and userFills for a user.
    /// Call after market data subscriptions to achieve READY-TRADING.
    async fn subscribe_trading_channels(
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
        user_address: &str,
    ) -> WsResult<()> {
        info!(user = %user_address, "Subscribing to trading channels");

        // Subscribe to orderUpdates
        let order_updates_req =
            SubscriptionManager::order_updates_subscription_request(user_address);
        write.send(Message::Text(order_updates_req)).await?;
        self.subscriptions
            .add_subscription(format!("orderUpdates:user:{}", user_address));

        // Drain response and wait
        self.drain_and_wait(write, read, 100).await?;

        // Subscribe to userFills
        let user_fills_req = SubscriptionManager::user_fills_subscription_request(user_address);
        write.send(Message::Text(user_fills_req)).await?;
        self.subscriptions.add_subscription("userFills".to_string());

        // Drain response and wait
        self.drain_and_wait(write, read, 100).await?;

        info!(user = %user_address, "Trading subscriptions sent");
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
                            // Use the same handler as the main loop to ensure consistent state updates
                            // (heartbeat, pong, subscriptions, inflight, message forwarding)
                            // Propagate errors to caller for proper reconnection handling
                            self.handle_text_message(&text).await?;
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
                    // No more messages pending, wait the remaining time then exit
                    let final_wait = drain_timeout.saturating_sub(drain_start.elapsed());
                    tokio::time::sleep(final_wait).await;
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
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 1000) as u64
}

/// Process subscriptionResponse ACK and update subscription state.
///
/// Returns `true` if orderUpdates ACK was processed.
/// Extracted as separate function for testability.
///
/// Note: SubscriptionManager uses internal RwLock, so &self is sufficient.
fn process_subscription_response(
    data: &serde_json::Value,
    subscriptions: &SubscriptionManager,
) -> bool {
    // Guard: only mark Ready for subscribe ACKs (not unsubscribe/error)
    let is_subscribe = data
        .get("method")
        .and_then(|v| v.as_str())
        .is_some_and(|m| m == "subscribe");

    if !is_subscribe {
        return false;
    }

    let subscription_type = extract_subscription_type(data);

    if subscription_type.is_some_and(|t| t == "orderUpdates") {
        subscriptions.mark_order_updates_ready();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_default_config() {
        let config = ConnectionConfig::default();
        assert_eq!(config.max_reconnect_attempts, 0); // Infinite
        assert_eq!(config.heartbeat_interval_ms, 45000);
    }

    // ========================================================================
    // process_subscription_response tests
    // ========================================================================

    #[test]
    fn test_process_subscription_response_official_format() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "subscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234"
            }
        });

        let result = process_subscription_response(&data, &subs);

        assert!(result, "Should return true for orderUpdates ACK");
        assert!(
            subs.ready_state().order_updates_ready,
            "Should mark order_updates_ready"
        );
    }

    #[test]
    fn test_process_subscription_response_unsubscribe_ignored() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "unsubscribe",
            "subscription": {
                "type": "orderUpdates",
                "user": "0x1234"
            }
        });

        let result = process_subscription_response(&data, &subs);

        assert!(!result, "Should return false for unsubscribe");
        assert!(
            !subs.ready_state().order_updates_ready,
            "Should NOT mark ready for unsubscribe"
        );
    }

    #[test]
    fn test_process_subscription_response_other_type() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "subscribe",
            "subscription": {
                "type": "allMids"
            }
        });

        let result = process_subscription_response(&data, &subs);

        assert!(!result, "Should return false for non-orderUpdates");
        assert!(
            !subs.ready_state().order_updates_ready,
            "Should NOT mark ready for other types"
        );
    }

    #[test]
    fn test_process_subscription_response_fallback_format() {
        let subs = SubscriptionManager::new();
        let data = json!({
            "method": "subscribe",
            "type": "orderUpdates",
            "user": "0x1234"
        });

        let result = process_subscription_response(&data, &subs);

        assert!(result, "Should handle fallback format");
        assert!(subs.ready_state().order_updates_ready);
    }
}
