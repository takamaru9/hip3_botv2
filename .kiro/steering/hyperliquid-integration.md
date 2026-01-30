# Hyperliquid Integration

Technical specifications for integrating with Hyperliquid's HIP-3 markets via WebSocket.

## Endpoints

| Environment | WebSocket | REST (Info) |
|-------------|-----------|-------------|
| Mainnet | `wss://api.hyperliquid.xyz/ws` | `https://api.hyperliquid.xyz/info` |
| Testnet | `wss://api.hyperliquid-testnet.xyz/ws` | `https://api.hyperliquid-testnet.xyz/info` |

## HIP-3 Market Structure

HIP-3 uses a **dual-key identification** system:

```
MarketKey = (DexId, AssetId)
         = (perp dex, asset within dex)
```

**Critical**: Symbol names are display-only. Always use `MarketKey` for identification.

### xyz DEX
- The primary HIP-3 DEX for this bot
- Auto-discovered via `/info` API with `xyz_pattern` config
- Each asset has `assetIdx` (numeric) and `coin` (display name)

### clearinghouseState API (Position Sync)

**Critical**: For HIP-3 perpDex positions, you **MUST** include the `dex` parameter:

```json
{
  "type": "clearinghouseState",
  "user": "0x...",
  "dex": "xyz"
}
```

Without `dex`, the API only returns L1 perp positions, not perpDex positions (BUG-005 fix).

## WebSocket Protocol

### Heartbeat (Keep-alive)
- Server closes connections **idle for 60 seconds**
- Client must send ping within 60 seconds of no outbound traffic
- Implementation: Send ping at **45 seconds** of no outbound

```json
// Request
{ "method": "ping" }

// Response
{ "channel": "pong" }
```

### Subscriptions

**Format**:
```json
{
  "method": "subscribe",
  "subscription": { "type": "<type>", ... }
}
```

**Required Channels for HIP-3 Bot**:

| Channel | Purpose | READY Condition |
|---------|---------|-----------------|
| `bbo` | Best bid/ask | First data received |
| `activeAssetCtx` | Oracle/mark price | First data received |
| `orderUpdates` | Order state changes | Subscription ACK (array format) |
| `userFills` | Fill notifications | Subscription ACK (stream only, no snapshot) |

### userFills Handling

**Important**: The bot does NOT use `userFills` snapshot for position reconstruction.

| Behavior | Reason |
|----------|--------|
| Skip `isSnapshot: true` | Snapshot may contain stale fills from previous sessions |
| Stream only | New fills during session are processed immediately |
| Position sync | Use REST `/info` clearinghouse API for startup sync |

### orderUpdates Format

The `orderUpdates` channel sends updates as an **array**:
```json
{
  "channel": "orderUpdates",
  "data": [
    { "order": {...}, "status": "filled" },
    { "order": {...}, "status": "cancelled" }
  ]
}
```

### Post Requests (WS-based REST)

Execute info/action requests through WebSocket with correlation ID:

```json
// Request
{
  "method": "post",
  "id": 123,
  "request": {
    "type": "info" | "action",
    "payload": { ... }
  }
}

// Response
{
  "channel": "post",
  "data": {
    "id": 123,
    "response": { "type": "...", "payload": { ... } }
  }
}
```

## Rate Limits (IP-based, all connections combined)

| Limit | Threshold | Implementation |
|-------|-----------|----------------|
| WS Messages | 2000/minute | Token bucket rate limiter |
| Inflight POSTs | 100 concurrent | Semaphore |
| Subscriptions | Per-connection limit | Track count |
| User channels | Unique user limit | One user per bot |

**Implementation Strategy**:
- Queue all outbound messages
- Apply token bucket for send rate
- Semaphore for POST concurrency
- Never send directly to WebSocket

## Order Signing

Orders require cryptographic signature using Ethereum-style signing:

### Signature Flow
```
1. Build order payload (OrderWire)
2. Create action with phantom agent
3. Compute action hash
4. Sign with private key (alloy signer)
5. Submit via POST
```

### Critical: `preserve_order` in serde_json
- **MUST use `serde_json` with `preserve_order` feature**
- Without it, JSON field order is alphabetized
- This causes action hash mismatch and signature verification failure

### Builder Info (HIP-3 specific)
```rust
BuilderInfo {
    builder: Address,  // DEX builder address
    fee: Decimal,      // Builder fee
}
```

## Fee Structure (HIP-3 Specific)

HIP-3 markets have **2x base fees** compared to validator-operated perps:

| Fee Type | Multiplier | Notes |
|----------|------------|-------|
| Maker | 2x base | HIP-3 deployer takes 50% share |
| Taker | 2x base | Same structure |
| Growth Mode | -90% | For new market liquidity bootstrap |

**Fee Calculation** (in `hip3-detector`):
```
effective_fee = base_fee * HIP3_FEE_MULTIPLIER (2.0)
```

## Message Formats

### BBO Update
```json
{
  "channel": "bbo",
  "data": {
    "coin": "BTC",
    "time": 1234567890123,
    "bbo": {
      "bid": ["50000", "1.5"],
      "ask": ["50010", "2.0"]
    }
  }
}
```

### AssetCtx Update
```json
{
  "channel": "activeAssetCtx",
  "data": {
    "coin": "BTC",
    "ctx": {
      "oraclePx": "50005",
      "markPx": "50005",
      "openInterest": "1000000",
      "funding": "0.0001"
    }
  }
}
```

### Order Update (Array Format)
```json
{
  "channel": "orderUpdates",
  "data": [{
    "order": {
      "coin": "BTC",
      "side": "B",
      "limitPx": "50000",
      "sz": "0.1",
      "oid": 12345,
      "cloid": "abc123"
    },
    "status": "filled",
    "statusTimestamp": 1234567890123
  }]
}
```
**Note**: `data` is always an array, even for single updates.

### User Fill (Stream Format)
```json
{
  "channel": "userFills",
  "data": {
    "fills": [{
      "coin": "BTC",
      "px": "50000",
      "sz": "0.1",
      "side": "B",
      "time": 1234567890123,
      "startPosition": "0.0",
      "closedPnl": "0.0"
    }]
  }
}
```
**Note**: Streaming updates do NOT include `isSnapshot` field.

## Reconnection Strategy

```
1. Detect disconnect (error, close, pong timeout)
2. Activate HardStop (block new orders)
3. Exponential backoff with jitter:
   delay = min(base * 2^attempt + random(0-1000ms), 60s)
4. Reconnect
5. Resubscribe to all channels
6. Wait for READY conditions
7. Release HardStop
```

## Primary Documentation

| Topic | URL |
|-------|-----|
| WebSocket | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket |
| Subscriptions | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions |
| Post Requests | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/post-requests |
| Heartbeat | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/heartbeat |
| Rate Limits | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits |
| HIP-3 | https://hyperliquid.gitbook.io/hyperliquid-docs/trading/hips/hip-3 |

---
_Document protocol specifics and constraints, not implementation code_
