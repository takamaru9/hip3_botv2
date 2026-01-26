//! HIP-3 Oracle/Mark Dislocation Taker Bot - Entry Point
//!
//! Phase A: Observation mode (signal detection and recording only)
//! Phase B: Execution mode (IOC taker with risk gates)

use anyhow::Result;
use clap::Parser;
use tracing::info;

/// HIP-3 Oracle/Mark Dislocation Taker Bot
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Configuration file path (can also be set via HIP3_CONFIG env var)
    #[arg(short, long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize TLS crypto provider (must be before any WS connections)
    hip3_ws::init_crypto();

    // Parse command line arguments
    let args = Args::parse();

    // Initialize logging
    hip3_telemetry::init_logging()?;

    info!("Starting HIP-3 Bot v{}", env!("CARGO_PKG_VERSION"));

    // Determine config path: CLI arg > HIP3_CONFIG env var > default
    let config_path = args
        .config
        .or_else(|| std::env::var("HIP3_CONFIG").ok())
        .unwrap_or_else(|| "config/default.toml".to_string());

    info!(config_path = %config_path, "Loading configuration");

    // Load configuration from specified file
    let config = hip3_bot::AppConfig::from_file(&config_path)?;
    info!(?config.mode, info_url = %config.info_url, "Configuration loaded");

    // Create application
    let mut app = hip3_bot::Application::new(config)?;

    // Run preflight validation (P0-15, P0-26, P0-27)
    // This discovers xyz markets from perpDexs if not specified in config
    info!("Running preflight validation...");
    app.run_preflight().await?;

    // Run main application loop
    app.run().await?;

    Ok(())
}
