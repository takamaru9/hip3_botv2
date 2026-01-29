# BUG-005: perpDex Position Sync Fix

## Metadata

| Item | Value |
|------|-------|
| Bug ID | BUG-005 |
| Date | 2026-01-29 |
| Status | `[COMPLETED]` |
| Severity | Critical |
| Component | hip3-registry/client.rs, hip3-bot/app.rs |

## Problem Description

### Symptom
- Fill API shows positions (e.g., -0.18 SHORT SILVER)
- clearinghouseState API shows 0 positions
- Bot's internal position tracker shows 0 positions after resync
- "Old positions remain" - position tracking inconsistency

### Root Cause
The `fetch_clearinghouse_state` API call was missing the `dex` parameter required for perpDex (xyz) positions.

According to [Hyperliquid API documentation](https://docs.chainstack.com/reference/hyperliquid-info-clearinghousestate):
> **dex** (string, optional) — Perp dex name. Defaults to the empty string which represents the first perp dex.

Without the `dex` parameter, the API only returns positions from the **default L1 perp**, not the **xyz perpDex** where the bot trades.

### Evidence
```
# Fill API shows position:
Fill 7: dir="Open Short", startPosition="0.0", sz="0.09" → Position = -0.09
Fill 8: dir="Open Short", startPosition="-0.09", sz="0.09" → Position = -0.18

# clearinghouseState (without dex param) shows no positions:
totalNtlPos="0.0", assetPositions=[]
```

## Solution

### Approach
Add `dex` parameter to `clearinghouseState` API request to fetch perpDex positions.

### Implementation

**File:** `crates/hip3-registry/src/client.rs`

1. Added new request struct with optional `dex` field:
```rust
#[derive(Debug, Serialize)]
struct InfoRequestWithUserAndDex {
    #[serde(rename = "type")]
    request_type: String,
    user: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dex: Option<String>,
}
```

2. Updated `fetch_clearinghouse_state` signature:
```rust
pub async fn fetch_clearinghouse_state(
    &self,
    user_address: &str,
    dex: Option<&str>,  // NEW: Added dex parameter
) -> RegistryResult<ClearinghouseStateResponse>
```

**File:** `crates/hip3-bot/src/app.rs`

3. Updated caller to pass dex name from config:
```rust
// BUG-005: Pass dex name to fetch perpDex positions
let dex_name = Some(self.config.xyz_pattern.as_str());

let state = client
    .fetch_clearinghouse_state(user_address, dex_name)
    .await
```

### API Request Comparison

| Before (Broken) | After (Fixed) |
|-----------------|---------------|
| `{"type": "clearinghouseState", "user": "0x..."}` | `{"type": "clearinghouseState", "user": "0x...", "dex": "xyz"}` |
| Returns: L1 perp positions only | Returns: xyz perpDex positions |

## Files Modified

| File | Changes |
|------|---------|
| `crates/hip3-registry/src/client.rs` | Added `InfoRequestWithUserAndDex`, updated `fetch_clearinghouse_state` |
| `crates/hip3-bot/src/app.rs` | Pass `dex_name` to fetch_clearinghouse_state |

## Tests

```bash
cargo test -p hip3-registry
# Result: 22 passed, 0 failed
```

## Verification

After deployment, check logs for:
```
INFO Syncing positions from Hyperliquid API
INFO Fetching clearinghouseState from exchange dex=Some("xyz")
INFO Found existing position from API market=... side=... size=...
```

If positions exist on perpDex, they should now appear in the sync.

## Related Issues

| Issue | Relationship |
|-------|-------------|
| BUG-004 | Duplicate flatten orders (separate issue, now fixed) |
| Position Tracking | This bug caused position tracking to be out of sync |

## Post-Deployment Observations

### BUG-005 Fix Verified (2026-01-29 09:34 UTC)

The `dex` parameter fix is working correctly. When API returns valid data:
```
INFO Fetching clearinghouseState from exchange dex=Some("xyz")
INFO Fetched clearinghouseState successfully, positions=2
INFO Found existing position from API market=dex_1:110030 side=Sell size=1.74
INFO Found existing position from API market=dex_1:110035 side=Sell size=0.01
```

### Known Issue: Hyperliquid API Inconsistency

**Symptom:** API intermittently returns `positions=0, balance=0.0` even when positions exist.

**Verification:** Direct curl from VPS confirms this is an API issue, not code bug:
```bash
# Same API call returns different results
10:45:23 - "accountValue":"0.0", positions=0  ← API Error
10:45:33 - "accountValue":"0.0", positions=0  ← API Error
10:45:53 - "accountValue":"7.32836", positions=1  ← Correct
```

**Observed Pattern:**
| Time | positions | balance | Status |
|------|-----------|---------|--------|
| 09:46:31 | 2 | $5.10 | ✓ Correct |
| 09:47:31 | 0 | $0.0 | ✗ API Issue |
| 09:48:31 | 2 | $X.XX | ✓ Recovered |

**Impact Assessment:**
- **Dynamic Sizing:** Falls back to static limit when balance=0 (safe behavior)
- **Position Tracking:** Local fill-based tracking unaffected
- **Next Sync:** Correct state restored on next valid API response

**Root Cause:** Suspected Hyperliquid API eventual consistency or load balancer issue.

**Mitigation:** No code change required. Current fallback behavior is safe.

## Sources

- [Hyperliquid clearinghouseState API](https://docs.chainstack.com/reference/hyperliquid-info-clearinghousestate)
- [Hyperliquid Info Endpoint Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint)
