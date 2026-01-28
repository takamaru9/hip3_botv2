# Signature `v` Field String Type Fix Plan

## Metadata

| Item | Value |
|------|-------|
| Version | v1.1 DRAFT |
| Created | 2026-01-25 |
| Bug Report | `bug/2026-01-25-signature-v-string-type.md` |
| Priority | HIGH (注文送信がブロックされている) |
| Estimated Effort | 小 (型変更のみ、ロジック変更なし) |

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| WebSocket POST Signature Format | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/post-requests | 2026-01-25 |
| Exchange Endpoint (REST) | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint | 2026-01-25 |
| Python SDK signing.py | GitHub | https://github.com/hyperliquid-dex/hyperliquid-python-sdk | 2026-01-25 |

**ドキュメント確認結果**:
- WebSocket POST docs: signature フィールドの例では `"r": "...", "s": "...", "v": "..."` と文字列形式で表記
- Exchange Endpoint docs: signature の内部構造（r, s, v の型）は明示されていない
- Python SDK (REST): `v` は数値のまま送信 (`{"r": to_hex(...), "s": to_hex(...), "v": signed["v"]}`)

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| v フィールドの型 | ドキュメントで型が明示されていない | Testnet で数値/文字列の AB 検証 |

**注意**: バグレポートでは `v: 28` (数値) で JSON パースエラーが発生しているため、文字列への変更は妥当と判断。ただし、Testnet で最終確認を行う。

## Issue Summary

WebSocket POST リクエストの署名フィールド `v` が数値（`28`）として送信されているが、Hyperliquid API は文字列（`"28"`）を期待している。これにより JSON パースエラーが発生し、注文送信が失敗する。

### Current vs Expected

| Field | Current | Expected |
|-------|---------|----------|
| `r` | `"0x..."` (String) ✅ | `"0x..."` (String) |
| `s` | `"0x..."` (String) ✅ | `"0x..."` (String) |
| `v` | `28` (Number) ❌ | `"28"` (String) |

## Root Cause Analysis

2 つの構造体で `v` フィールドが `u8` として定義されている:

1. **`SignaturePayload`** (`hip3-ws/src/message.rs:66`) - WebSocket メッセージ用
2. **`ActionSignature`** (`hip3-executor/src/ws_sender.rs:65`) - Executor 内部用

serde の JSON シリアライズ時に `u8` は数値としてシリアライズされるため、API が期待する文字列形式にならない。

## Impact Analysis

### 影響を受けるファイル

| File | Lines | Change Type |
|------|-------|-------------|
| `crates/hip3-ws/src/message.rs` | 60-67 | 構造体フィールド型変更 |
| `crates/hip3-executor/src/ws_sender.rs` | 59-66, 68-82, 192 | 構造体フィールド型変更 + `from_bytes` + `with_signature_parts` |
| `crates/hip3-executor/src/executor_loop.rs` | 408 | 値生成時の `.to_string()` 追加 |
| `crates/hip3-executor/src/real_ws_sender.rs` | 52 | `.clone()` に変更（既に String） |

### テストファイル

| File | Lines | Change Type |
|------|-------|-------------|
| `crates/hip3-ws/src/message.rs` | 521, 547 | `v: 27` → `v: "27".to_string()` |
| `crates/hip3-executor/src/ws_sender.rs` | 223, 244, 274, 289, 302 | `v: 27/28` → `v: "27/28".to_string()` + assert 修正 |
| `crates/hip3-executor/src/real_ws_sender.rs` | 119 | `v: 27` → `v: "27".to_string()` |

## Implementation Steps

### Step 1: Update `SignaturePayload` (hip3-ws)

**File**: `crates/hip3-ws/src/message.rs`

**Before (L60-67)**:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignaturePayload {
    pub r: String,
    pub s: String,
    pub v: u8,
}
```

**After**:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignaturePayload {
    pub r: String,
    pub s: String,
    pub v: String,
}
```

### Step 2: Update `ActionSignature` (hip3-executor)

**File**: `crates/hip3-executor/src/ws_sender.rs`

#### 2a. 構造体定義 (L59-66)

**Before**:
```rust
#[derive(Debug, Clone, Serialize)]
pub struct ActionSignature {
    pub r: String,
    pub s: String,
    pub v: u8,
}
```

**After**:
```rust
#[derive(Debug, Clone, Serialize)]
pub struct ActionSignature {
    pub r: String,
    pub s: String,
    pub v: String,
}
```

#### 2b. `from_bytes` メソッド (L68-82)

**Before**:
```rust
impl ActionSignature {
    /// Create from raw signature bytes (64 bytes) + v byte
    pub fn from_bytes(bytes: &[u8; 65]) -> Self {
        let r = format!("0x{}", hex::encode(&bytes[0..32]));
        let s = format!("0x{}", hex::encode(&bytes[32..64]));
        let v_raw = bytes[64];
        // Normalize v: if it's 0 or 1 (y_parity), convert to 27 or 28
        let v = if v_raw < 27 { v_raw + 27 } else { v_raw };

        Self { r, s, v }
    }
}
```

**After**:
```rust
impl ActionSignature {
    /// Create from raw signature bytes (64 bytes) + v byte
    pub fn from_bytes(bytes: &[u8; 65]) -> Self {
        let r = format!("0x{}", hex::encode(&bytes[0..32]));
        let s = format!("0x{}", hex::encode(&bytes[32..64]));
        let v_raw = bytes[64];
        // Normalize v: if it's 0 or 1 (y_parity), convert to 27 or 28
        let v = if v_raw < 27 { v_raw + 27 } else { v_raw };

        Self { r, s, v: v.to_string() }
    }
}
```

#### 2c. `with_signature_parts` メソッド (L192)

**File**: `crates/hip3-executor/src/ws_sender.rs`

**Before**:
```rust
/// Build with signature components.
pub fn with_signature_parts(self, r: String, s: String, v: u8) -> SignedAction {
    SignedAction {
        action: self.action,
        nonce: self.nonce,
        signature: ActionSignature { r, s, v },
        post_id: self.post_id,
    }
}
```

**After**:
```rust
/// Build with signature components.
pub fn with_signature_parts(self, r: String, s: String, v: String) -> SignedAction {
    SignedAction {
        action: self.action,
        nonce: self.nonce,
        signature: ActionSignature { r, s, v },
        post_id: self.post_id,
    }
}
```

**注意**: このメソッドの呼び出し元も `v` を `String` で渡すよう修正が必要。

### Step 3: Update Signature Generation (executor_loop.rs)

**File**: `crates/hip3-executor/src/executor_loop.rs`

**Before (L405-411)**:
```rust
let signed_action = SignedAction {
    action,
    nonce,
    signature: ActionSignature {
        r: format!("0x{}", hex::encode(signature.r().to_be_bytes::<32>())),
        s: format!("0x{}", hex::encode(signature.s().to_be_bytes::<32>())),
        v: 27 + signature.v() as u8,
    },
    post_id,
};
```

**After**:
```rust
let signed_action = SignedAction {
    action,
    nonce,
    signature: ActionSignature {
        r: format!("0x{}", hex::encode(signature.r().to_be_bytes::<32>())),
        s: format!("0x{}", hex::encode(signature.s().to_be_bytes::<32>())),
        v: (27 + signature.v() as u8).to_string(),
    },
    post_id,
};
```

### Step 4: Update Conversion (real_ws_sender.rs)

**File**: `crates/hip3-executor/src/real_ws_sender.rs`

**Before (L49-53)**:
```rust
let payload = PostPayload {
    action: action_value,
    nonce: action.nonce,
    signature: SignaturePayload {
        r: action.signature.r.clone(),
        s: action.signature.s.clone(),
        v: action.signature.v,
    },
    vault_address: self.vault_address.clone(),
};
```

**After**:
```rust
let payload = PostPayload {
    action: action_value,
    nonce: action.nonce,
    signature: SignaturePayload {
        r: action.signature.r.clone(),
        s: action.signature.s.clone(),
        v: action.signature.v.clone(),
    },
    vault_address: self.vault_address.clone(),
};
```

### Step 5: Update Tests

#### 5a. hip3-ws/src/message.rs テスト

**L521 付近**:
```rust
// Before
signature: SignaturePayload {
    r: "0x...".to_string(),
    s: "0x...".to_string(),
    v: 27,
},

// After
signature: SignaturePayload {
    r: "0x...".to_string(),
    s: "0x...".to_string(),
    v: "27".to_string(),
},
```

**L547 付近**: 同様に `v: 28` → `v: "28".to_string()`

#### 5b. hip3-executor/src/ws_sender.rs テスト

**L223, L244 付近**:
```rust
// Before
ActionSignature {
    r: "0x...".to_string(),
    s: "0x...".to_string(),
    v: 27,
}

// After
ActionSignature {
    r: "0x...".to_string(),
    s: "0x...".to_string(),
    v: "27".to_string(),
}
```

**L274, L289, L302 付近** (`from_bytes` テスト):
```rust
// Before
assert_eq!(sig.v, 28);

// After
assert_eq!(sig.v, "28");
```

#### 5c. hip3-executor/src/real_ws_sender.rs テスト

**L119 付近**: `v: 27` → `v: "27".to_string()`

## Verification Checklist

### Build & Test

- [ ] `cargo fmt` - フォーマット確認
- [ ] `cargo clippy -- -D warnings` - 静的解析
- [ ] `cargo build --release` - ビルド成功
- [ ] `cargo test --workspace` - 全テスト成功

### JSON Output Verification

- [ ] `SignaturePayload` のシリアライズで `"v": "27"` または `"v": "28"` が出力される
- [ ] `ActionSignature` のシリアライズで `"v": "27"` または `"v": "28"` が出力される

### Testnet AB 検証（実装前に推奨）

ドキュメントで `v` の型が明示されていないため、Testnet で事前検証を推奨：

1. **現状確認**: `v: 28` (数値) で送信 → JSON パースエラー確認（バグレポートと一致）
2. **修正確認**: `v: "28"` (文字列) で送信 → 成功確認

**検証方法**:
```bash
# 1. Testnet 用設定で起動
cargo run --release -- --config config/testnet.toml

# 2. シグナル発生を待つか、手動でテスト注文を発行
# 3. ログで JSON ペイロードと API レスポンスを確認
```

**AB 検証が困難な場合**: バグレポートの実測結果（数値でエラー）を信頼し、文字列で実装。

### Runtime Verification

- [ ] Testnet テスト実行
- [ ] JSON パースエラーが発生しない
- [ ] 注文送信が成功する
- [ ] ACK メッセージを受信
- [ ] Mainnet テスト実行（Testnet 成功後）

## Non-Negotiable Requirements

1. **型変更のみ**: ロジック変更は行わない（v の正規化ロジック 27/28 は維持）
2. **後方互換性**: この変更により既存のシリアライズ形式が正しくなるが、デシリアライズには影響しない
3. **テスト更新必須**: 型変更に伴うテスト更新を忘れない

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| 型変更によるコンパイルエラー | 低 | コンパイル時に検出される |
| テスト漏れ | 低 | `cargo test` で検出される |
| デシリアライズ影響 | なし | Hyperliquid からの応答に signature フィールドは含まれない |
| v 型の仕様変更 | 中 | ドキュメント未明示のため、Testnet で事前検証 |

## Review History

| Review | Date | Findings | Resolution |
|--------|------|----------|------------|
| #1 | 2026-01-25 | `with_signature_parts` 漏れ、Testnet 検証不足 | Step 2c 追加、AB 検証追加 |

## Changelog

| Version | Date | Changes |
|---------|------|---------|
| v1.0 | 2026-01-25 | Initial plan |
| v1.1 | 2026-01-25 | `with_signature_parts` 修正追加、Testnet AB 検証追加、一次情報セクション拡充 |
