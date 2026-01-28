# Technology Stack

## Architecture

**Event-driven async pipeline**: WebSocket → Feed Aggregation → Risk Gates → Detector → Executor

Modular crate-based design with clear separation of concerns:
- Core types shared across all crates
- Each domain (risk, detection, execution) in isolated crates
- Single orchestrating application crate (`hip3-bot`)

## Core Technologies

- **Language**: Rust (Edition 2021, MSRV 1.75+)
- **Runtime**: Tokio async runtime with full features
- **WebSocket**: tokio-tungstenite with native-tls

## Key Libraries

| Library | Purpose | Critical Note |
|---------|---------|---------------|
| `rust_decimal` | Price/size precision | Serde with string serialization |
| `serde_json` | JSON parsing | **Must use `preserve_order`** for signature verification |
| `alloy` | Ethereum signing | For Hyperliquid authentication |
| `tracing` | Structured logging | With env-filter and JSON output |
| `parquet` + `arrow` | Signal persistence | Async writes to columnar storage |
| `axum` | HTTP/WebSocket server | For real-time dashboard (port 8080) |

## Development Standards

### Type Safety
- `Price` and `Size` wrapper types for numeric precision
- `MarketKey` composite type for HIP-3 dual-key markets
- Exhaustive match on critical enums

### Code Quality
```bash
cargo fmt        # Formatting
cargo clippy -- -D warnings  # Lint (warnings as errors)
cargo check      # Type verification
```

### Testing
- Unit tests in each crate
- Integration tests in `tests/` directory
- Mock implementations for WebSocket sender

## Development Environment

### Required Tools
- Rust 1.85+ (Edition 2024 support)
- Python 3.11+ (for analysis scripts)
- Docker (for deployment)

### Common Commands
```bash
# Dev: Run in observation mode
cargo run -- --config config/default.toml

# Build: Release build
cargo build --release

# Test: All workspace tests
cargo test --workspace
```

## Key Technical Decisions

| Decision | Rationale |
|----------|-----------|
| Rust over TypeScript | Low latency requirements, memory safety |
| `preserve_order` in serde_json | Signature hash depends on field order |
| Workspace with crates | Clear dependency boundaries, faster incremental builds |
| Tokio channels for events | Async message passing, no shared mutable state |
| TOML configuration | Human-readable, supports nested structures |

---
_Document standards and patterns, not every dependency_
