//! HTTP server implementation using axum.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use futures_util::stream::StreamExt;
use futures_util::SinkExt;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::config::DashboardConfig;
use crate::state::DashboardState;
use crate::types::DashboardMessage;

/// Connection limiter to prevent too many concurrent WebSocket connections.
pub struct ConnectionLimiter {
    current: AtomicUsize,
    max: usize,
}

impl ConnectionLimiter {
    pub fn new(max: usize) -> Self {
        Self {
            current: AtomicUsize::new(0),
            max,
        }
    }

    pub fn try_acquire(&self) -> Option<ConnectionGuard<'_>> {
        loop {
            let current = self.current.load(Ordering::Acquire);
            if current >= self.max {
                return None;
            }
            if self
                .current
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(ConnectionGuard { limiter: self });
            }
        }
    }

    pub fn current_count(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }
}

pub struct ConnectionGuard<'a> {
    limiter: &'a ConnectionLimiter,
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.limiter.current.fetch_sub(1, Ordering::Release);
    }
}

/// Shared application state for axum handlers.
#[derive(Clone)]
pub struct AppState {
    dashboard_state: DashboardState,
    broadcast_tx: broadcast::Sender<String>,
    connection_limiter: Arc<ConnectionLimiter>,
    config: DashboardConfig,
}

impl AppState {
    pub fn new(
        dashboard_state: DashboardState,
        broadcast_tx: broadcast::Sender<String>,
        config: DashboardConfig,
    ) -> Self {
        Self {
            dashboard_state,
            broadcast_tx,
            connection_limiter: Arc::new(ConnectionLimiter::new(config.max_connections)),
            config,
        }
    }
}

/// Create the axum router.
pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/api/snapshot", get(get_snapshot))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

/// Serve the index HTML page.
async fn serve_index(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Html<&'static str>, Response> {
    // Check basic auth if enabled
    if state.config.auth_enabled() && !check_basic_auth(&headers, &state.config) {
        return Err(unauthorized_response());
    }
    Ok(Html(include_str!("../static/index.html")))
}

/// Get current state snapshot as JSON.
async fn get_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<crate::types::DashboardSnapshot>, Response> {
    // Check basic auth if enabled
    if state.config.auth_enabled() && !check_basic_auth(&headers, &state.config) {
        return Err(unauthorized_response());
    }

    let snapshot = state.dashboard_state.collect_snapshot();
    Ok(Json(snapshot))
}

/// WebSocket upgrade handler.
async fn ws_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Check basic auth if enabled
    if state.config.auth_enabled() && !check_basic_auth(&headers, &state.config) {
        return unauthorized_response();
    }

    // Check connection limit
    let guard = match state.connection_limiter.try_acquire() {
        Some(guard) => guard,
        None => {
            warn!(
                current = state.connection_limiter.current_count(),
                max = state.config.max_connections,
                "WebSocket connection limit reached"
            );
            return (StatusCode::SERVICE_UNAVAILABLE, "Too many connections").into_response();
        }
    };

    info!(
        connections = state.connection_limiter.current_count(),
        "New WebSocket connection"
    );

    // We need to move the guard into the async block to keep the connection counted
    // However, we can't easily pass it through the closure. Instead, we'll increment/decrement
    // in the handle function itself. Drop the guard here and manage manually.
    drop(guard);

    ws.on_upgrade(move |socket| handle_ws_connection(socket, state))
}

/// Handle a WebSocket connection.
async fn handle_ws_connection(socket: WebSocket, state: AppState) {
    // Try to acquire connection slot
    let _guard = match state.connection_limiter.try_acquire() {
        Some(guard) => guard,
        None => {
            warn!("Connection limit reached during upgrade");
            return;
        }
    };

    let (mut sender, mut receiver) = socket.split();

    // Subscribe to broadcast channel
    let mut broadcast_rx = state.broadcast_tx.subscribe();

    // Send initial snapshot
    let initial_snapshot = state.dashboard_state.collect_snapshot();
    let initial_msg = DashboardMessage::Snapshot(initial_snapshot);
    if let Ok(json) = serde_json::to_string(&initial_msg) {
        if sender.send(Message::Text(json.into())).await.is_err() {
            debug!("Failed to send initial snapshot, client disconnected");
            return;
        }
    }

    // Spawn task to handle incoming messages (for ping/pong and close)
    let mut incoming_task = tokio::spawn(async move {
        while let Some(result) = receiver.next().await {
            match result {
                Ok(Message::Ping(data)) => {
                    debug!("Received ping from client");
                    // Pong is handled automatically by axum
                    let _ = data; // Suppress unused warning
                }
                Ok(Message::Close(_)) => {
                    debug!("Client sent close frame");
                    break;
                }
                Err(e) => {
                    debug!(error = %e, "WebSocket receive error");
                    break;
                }
                _ => {}
            }
        }
    });

    // Main loop: forward broadcast messages to WebSocket
    loop {
        tokio::select! {
            result = broadcast_rx.recv() => {
                match result {
                    Ok(msg) => {
                        if sender.send(Message::Text(msg.into())).await.is_err() {
                            debug!("Failed to send message, client disconnected");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "WebSocket client lagged, catching up");
                        // Continue - tokio advances to latest
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!("Broadcast channel closed");
                        break;
                    }
                }
            }
            _ = &mut incoming_task => {
                debug!("Incoming task completed, closing connection");
                break;
            }
        }
    }

    info!(
        connections = state.connection_limiter.current_count().saturating_sub(1),
        "WebSocket connection closed"
    );
}

/// Check basic authentication.
fn check_basic_auth(headers: &HeaderMap, config: &DashboardConfig) -> bool {
    let auth_header = match headers.get(header::AUTHORIZATION) {
        Some(h) => h,
        None => return false,
    };

    let auth_str = match auth_header.to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };

    if !auth_str.starts_with("Basic ") {
        return false;
    }

    let encoded = &auth_str[6..];
    let decoded = match base64_decode(encoded) {
        Some(d) => d,
        None => return false,
    };

    let expected = format!("{}:{}", config.username, config.password);
    decoded == expected
}

/// Simple base64 decode for basic auth.
fn base64_decode(input: &str) -> Option<String> {
    // Simple base64 decoding without external crate
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn decode_char(c: u8) -> Option<u8> {
        ALPHABET.iter().position(|&x| x == c).map(|p| p as u8)
    }

    let input = input.trim_end_matches('=');
    let mut result = Vec::new();
    let bytes: Vec<u8> = input.bytes().collect();

    for chunk in bytes.chunks(4) {
        let mut buf = 0u32;
        let mut bits = 0;

        for &c in chunk {
            if let Some(val) = decode_char(c) {
                buf = (buf << 6) | (val as u32);
                bits += 6;
            }
        }

        while bits >= 8 {
            bits -= 8;
            result.push(((buf >> bits) & 0xFF) as u8);
        }
    }

    String::from_utf8(result).ok()
}

/// Create an unauthorized response.
fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"Dashboard\"")],
        "Unauthorized",
    )
        .into_response()
}

/// Run the dashboard HTTP server.
pub async fn run_server(
    dashboard_state: DashboardState,
    config: DashboardConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Create broadcast channel
    // Capacity: update_interval_ms = 100ms = 10 updates/sec
    // Buffer for slow clients: 32 messages (3.2 sec worth)
    let (broadcast_tx, _) = broadcast::channel::<String>(32);

    // Create app state
    let state = AppState::new(
        dashboard_state.clone(),
        broadcast_tx.clone(),
        config.clone(),
    );

    // Create router
    let app = create_router(state);

    // Spawn broadcaster task
    let broadcaster_state = dashboard_state;
    let broadcaster_tx = broadcast_tx;
    let update_interval_ms = config.update_interval_ms;

    tokio::spawn(async move {
        crate::broadcast::run_broadcaster(broadcaster_state, broadcaster_tx, update_interval_ms)
            .await;
    });

    // Bind and serve
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    info!(port = config.port, "Starting dashboard server");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
