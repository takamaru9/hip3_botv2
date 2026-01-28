# Product Overview

**hip3_botv2** is a high-frequency trading bot for Hyperliquid's HIP-3 (Builder-deployed perpetuals) markets, specializing in oracle/mark price dislocation arbitrage.

## Core Capabilities

1. **Dislocation Detection**: Identifies when best bid/ask crosses oracle price with sufficient edge to cover fees and slippage
2. **Risk Management**: Multi-layer gate checks (8 gates) ensuring trades only execute under safe conditions
3. **Execution Engine**: IOC order submission with idempotency guarantees, rate limiting, and circuit breakers
4. **Real-time Monitoring**: WebSocket-based market data feed with automatic reconnection and heartbeat monitoring

## Target Use Cases

- **Oracle Arbitrage**: Exploit temporary divergence between oracle price and mark/mid price on HIP-3 markets
- **Observation Mode**: Collect signal data for strategy validation before live trading (Phase A)
- **Trading Mode**: Execute trades based on detected dislocations (Phase B)

## Value Proposition

- **HIP-3 Specialized**: Purpose-built for Hyperliquid's unique HIP-3 market structure (DEX + Asset dual identification)
- **Safety-First**: 8 risk gates + HardStop circuit breaker prevents trading under adverse conditions
- **Low Latency**: Rust implementation with async WebSocket handling for sub-100ms response times
- **Auditability**: Comprehensive signal logging and execution tracking for post-analysis

---
_Focus on patterns and purpose, not exhaustive feature lists_
