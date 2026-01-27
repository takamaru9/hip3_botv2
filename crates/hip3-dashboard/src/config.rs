//! Dashboard configuration.

use serde::{Deserialize, Serialize};

/// Dashboard server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    /// Enable dashboard server.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Update interval in milliseconds for WebSocket broadcasts.
    #[serde(default = "default_update_interval_ms")]
    pub update_interval_ms: u64,
    /// Maximum concurrent WebSocket connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Basic auth username (empty = disabled).
    #[serde(default)]
    pub username: String,
    /// Basic auth password (empty = disabled).
    #[serde(default)]
    pub password: String,
}

fn default_enabled() -> bool {
    false
}

fn default_port() -> u16 {
    8080
}

fn default_update_interval_ms() -> u64 {
    100
}

fn default_max_connections() -> usize {
    10
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            port: default_port(),
            update_interval_ms: default_update_interval_ms(),
            max_connections: default_max_connections(),
            username: String::new(),
            password: String::new(),
        }
    }
}

impl DashboardConfig {
    /// Check if basic auth is enabled.
    pub fn auth_enabled(&self) -> bool {
        !self.username.is_empty() && !self.password.is_empty()
    }
}
