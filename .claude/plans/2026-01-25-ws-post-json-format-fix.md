# WebSocket POST JSON Format Fix Plan

## Metadata

| Item | Value |
|------|-------|
| Version | v1.5 DRAFT |
| Created | 2026-01-25 |
| Bug Report | `bug/2026-01-25-json-parse-error-after-v-fix.md` |
| Related Bug | `bug/2026-01-25-signature-v-string-type.md` |
| Priority | HIGH (注文送信がブロックされている) |
| Estimated Effort | 小〜中 |

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| WebSocket POST Format | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/post-requests | 2026-01-25 |
| Exchange Endpoint | Hyperliquid GitBook | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint | 2026-01-25 |
| Python SDK signing.py | GitHub | https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/hyperliquid/utils/signing.py | 2026-01-25 |

## 問題分析

### 経緯

1. **前回のバグ** (`2026-01-25 12:38`): `v: 28` (数値) で JSON パースエラー
2. **今回のバグ** (`2026-01-25 13:24`): `v: "27"` (文字列) で JSON パースエラー

両方で同じ「Error parsing JSON into valid websocket request」が発生しているため、`v` の型だけが問題ではない。

### 根本原因の分析

| 要素 | 現在の実装 | ドキュメント例 | 問題か？ |
|------|------------|----------------|----------|
| `vaultAddress` | `None` → **省略** | `"vaultAddress": "0x12...3"` | **可能性高** |
| `signature.v` | `"27"` (String) | `"v": ...` (型不明) | **可能性あり** |

**仮説**: WebSocket POST では `vaultAddress` フィールドが必須であり、省略するとパースエラーになる。

### 既知の失敗記録

| バグ報告 | 日時 | vaultAddress | signature.v | 結果 |
|----------|------|--------------|-------------|------|
| `bug/2026-01-25-signature-v-string-type.md` | 12:38 JST | **省略** | `28` (数値) | JSON パースエラー |
| `bug/2026-01-25-json-parse-error-after-v-fix.md` | 13:24 JST | **省略** | `"27"` (文字列) | JSON パースエラー |

**重要**: 両バグ報告で共通しているのは「`vaultAddress` フィールドの省略」である。`signature.v` の型は異なるが同じエラーが発生しているため、`vaultAddress` の省略が根本原因である可能性が高い。

**一次情報との整合性**:
- Exchange Endpoint ドキュメントでは `vaultAddress` は「vault/subaccount で取引する場合に設定」とされオプション扱い
- しかし WebSocket POST ドキュメントの例では常に含まれている
- **結論**: REST API とWebSocket POST で要件が異なる可能性があり、実測で確認が必要

### 一次情報の詳細

**1. WebSocket POST ドキュメント例**:
```json
{
  "method": "post",
  "id": 256,
  "request": {
    "type": "action",
    "payload": {
      "action": {...},
      "nonce": 1713825891591,
      "signature": {"r": "...", "s": "...", "v": "..."},
      "vaultAddress": "0x12...3"
    }
  }
}
```

**2. REST Exchange Endpoint ドキュメント**:
- `vaultAddress` は "If trading on behalf of a vault or subaccount" の場合に必要
- 通常取引では不要（オプション）と記載

**3. Python SDK (`signing.py`)**:
- `signature["v"]` は数値 (`28`) として送信
- REST API では `vaultAddress` は vault 使用時のみ含まれる

### 矛盾点

- **REST API**: `vaultAddress` はオプション（省略可）
- **WebSocket POST**: ドキュメント例では常に含まれている

WebSocket POST と REST API で挙動が異なる可能性がある。

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| vaultAddress の必須性 | ドキュメントで明示されていない | Mainnet 少額テストで検証 |
| v の型（WebSocket POST） | ドキュメントで明示されていない | Mainnet 少額テストで検証 |

**注意**: Testnet では HIP-3 銘柄のテストが困難なため、Mainnet 少額テストで検証する。

## 修正戦略

### 段階的検証アプローチ（必須）

**原因を切り分けるため、以下の順序で実装・検証を行う。一度に複数の変更を入れない。**

**検証環境**: Mainnet 少額テスト（Testnet では HIP-3 銘柄のテストが困難なため）

| Phase | 内容 | 検証 |
|-------|------|------|
| **Phase A** | `vaultAddress` を `null` で明示的に含める | Mainnet 少額テスト |
| **Phase A 成功後** | **Phase B** `signature.v` を数値に戻す | Mainnet 少額テスト |

### 理由

- Phase A: ドキュメント例との最大の差異を解消
- Phase B: Python SDK と同じ形式に合わせる
- 段階的に検証することで、問題の原因を特定可能

### Mainnet 少額テスト安全ガード（必須）

**以下の安全策を全て適用した上で検証を行う。**

**注意**: Mainnet の最小注文額は $10 のため、$11 を設定。

| 安全策 | 設定 | 目的 |
|--------|------|------|
| **最小サイズ** | `max_notional = 11` ($11、最小注文額 $10 の直上) | 損失最小化 |
| **損失上限** | 単一テストで最大 $11 | 許容損失の明示 |
| **監視体制** | ログを常時監視 | 異常即検知 |
| **即時停止** | Ctrl+C で即時停止 | 緊急対応 |
| **テスト時間** | 最大 5 分 | 露出時間制限 |

**緊急停止手順**:
```bash
# 1. Ctrl+C でボット停止
# 2. ポジション確認
curl -X POST https://api.hyperliquid.xyz/info \
  -H "Content-Type: application/json" \
  -d '{"type": "clearinghouseState", "user": "0x0116A3D95994BcC7D6A84380ED6256FBb32cD25D"}'
# 3. 必要に応じて手動でポジションクローズ（Hyperliquid UI）
```

## Implementation Steps

### Phase A: vaultAddress の修正

#### Step A-1: vaultAddress を null で出力する

**File**: `crates/hip3-ws/src/message.rs`

**現状 (L53-55)**:
```rust
/// Optional vault address for vault trading.
#[serde(rename = "vaultAddress", skip_serializing_if = "Option::is_none")]
pub vault_address: Option<String>,
```

**修正後**:
```rust
/// Vault address for vault trading. Null for personal trading.
#[serde(rename = "vaultAddress")]
pub vault_address: Option<String>,
```

**効果**: `None` の場合、JSON に `"vaultAddress": null` として出力される（省略ではなく）

#### Step A-2: vaultAddress のキー存在確認テスト追加

**File**: `crates/hip3-ws/src/message.rs` (テストセクション)

**追加するアサート**:
```rust
#[test]
fn test_post_payload_vault_address_null_is_explicit() {
    let payload = PostPayload {
        action: serde_json::json!({"type": "test"}),
        nonce: 12345,
        signature: SignaturePayload {
            r: "0xabc".to_string(),
            s: "0xdef".to_string(),
            v: "27".to_string(),  // Phase A 時点では String のまま
        },
        vault_address: None,
    };

    let json = serde_json::to_value(&payload).unwrap();

    // キーが存在することを確認（is_null だけでなく contains_key で検証）
    assert!(
        json.as_object().unwrap().contains_key("vaultAddress"),
        "vaultAddress key must be present even when None"
    );
    assert!(json["vaultAddress"].is_null());
}
```

#### Step A-3: Mainnet 少額テスト（Phase A）

**前提条件**:
- [ ] 安全ガード設定を確認（max_notional = 11）
- [ ] 緊急停止手順を確認
- [ ] ログ監視体制を確保

**検証内容**:
1. `vaultAddress: null` 形式で注文送信
2. JSON パースエラーが解消されるか確認
3. 成功 → Phase B へ進む
4. 失敗 → Alternative アプローチへ（後述の判断基準参照）

**検証コマンド**:
```bash
# 1. max_notional を $11 に設定（config/mainnet-test.toml を編集、最小注文額 $10 の直上）
# [detector]
# max_notional = 11

# 2. Mainnet 少額テストで起動
cargo run --release -- --config config/mainnet-test.toml

# 3. 最大 5 分間監視、シグナル発生を待つ
# 4. Ctrl+C で停止
```

**成功判定**:
- JSON パースエラーが発生しない
- 注文が受け付けられる（ACK または約定）

**失敗判定**:
- "Error parsing JSON into valid websocket request" が継続する場合

---

### Phase B: signature.v の修正（Phase A 成功後のみ実行）

#### Step B-1: signature.v を u8 に戻す

**前回の修正を revert し、数値形式に戻す。**

#### Step B-1a. SignaturePayload (hip3-ws)

**File**: `crates/hip3-ws/src/message.rs`

**現状**:
```rust
pub struct SignaturePayload {
    pub r: String,
    pub s: String,
    pub v: String,  // ← 前回の修正で String に変更
}
```

**修正後**:
```rust
pub struct SignaturePayload {
    pub r: String,
    pub s: String,
    pub v: u8,  // ← 数値に戻す
}
```

#### Step B-1b. ActionSignature (hip3-executor)

**File**: `crates/hip3-executor/src/ws_sender.rs`

**現状**:
```rust
pub struct ActionSignature {
    pub r: String,
    pub s: String,
    pub v: String,  // ← 前回の修正で String に変更
}
```

**修正後**:
```rust
pub struct ActionSignature {
    pub r: String,
    pub s: String,
    pub v: u8,  // ← 数値に戻す
}
```

#### Step B-1c. from_bytes メソッド

**File**: `crates/hip3-executor/src/ws_sender.rs`

**現状**:
```rust
Self { r, s, v: v.to_string() }
```

**修正後**:
```rust
Self { r, s, v }
```

#### Step B-1d. with_signature_parts メソッド

**File**: `crates/hip3-executor/src/ws_sender.rs`

**現状**:
```rust
pub fn with_signature_parts(self, r: String, s: String, v: String) -> SignedAction
```

**修正後**:
```rust
pub fn with_signature_parts(self, r: String, s: String, v: u8) -> SignedAction
```

#### Step B-1e. executor_loop.rs

**File**: `crates/hip3-executor/src/executor_loop.rs`

**現状**:
```rust
v: (27 + signature.v() as u8).to_string(),
```

**修正後**:
```rust
v: 27 + signature.v() as u8,
```

#### Step B-1f. real_ws_sender.rs

**File**: `crates/hip3-executor/src/real_ws_sender.rs`

**現状**:
```rust
v: action.signature.v.clone(),
```

**修正後**:
```rust
v: action.signature.v,
```

#### Step B-2: テストの修正

前回 String に変更したテストを u8 に戻す。

**更新対象テストファイル**:

| ファイル | テスト名 | 行 | 変更内容 |
|----------|----------|-----|----------|
| `crates/hip3-ws/src/message.rs` | `test_post_request_serialization` | L517 | `v: "27".to_string()` → `v: 27` |
| `crates/hip3-ws/src/message.rs` | `test_post_request_with_vault_address` | L543 | `v: "28".to_string()` → `v: 28` |
| `crates/hip3-ws/src/message.rs` | `test_post_payload_vault_address_null_is_explicit` | (Step A-2 追加) | `v: "27".to_string()` → `v: 27` |
| `crates/hip3-executor/src/ws_sender.rs` | `test_signature_from_bytes` | L268 | `assert_eq!(sig.v, "28")` → `assert_eq!(sig.v, 28)` |
| `crates/hip3-executor/src/ws_sender.rs` | `test_signature_from_bytes_normalizes_v` | L283 | `assert_eq!(sig.v, "28")` → `assert_eq!(sig.v, 28)` |
| `crates/hip3-executor/src/ws_sender.rs` | `test_signature_from_bytes_normalizes_v_zero` | L296 | `assert_eq!(sig.v, "27")` → `assert_eq!(sig.v, 27)` |
| `crates/hip3-executor/src/real_ws_sender.rs` | `test_send_success` | L129 | `v: "27".to_string()` → `v: 27` |

#### Step B-3: Mainnet 少額テスト（Phase B）

**前提条件**:
- [ ] Phase A が成功していること
- [ ] 安全ガード設定を確認（max_notional = 11）
- [ ] 緊急停止手順を確認

**検証内容**:
1. `signature.v` が数値形式で送信されることを確認
2. 注文が正常に送信されるか確認
3. ACK メッセージを受信することを確認

**検証コマンド**:
```bash
# Mainnet 少額テストで起動
cargo run --release -- --config config/mainnet-test.toml

# 最大 5 分間監視、シグナル発生を待つ
# Ctrl+C で停止
```

**成功判定**:
- 注文送信成功
- ACK メッセージ受信
- 約定（オプション、シグナル条件による）

## Expected JSON Output

```json
{
  "method": "post",
  "id": 7,
  "request": {
    "type": "action",
    "payload": {
      "action": {
        "type": "order",
        "orders": [...],
        "grouping": "na"
      },
      "nonce": 1769315241462,
      "signature": {
        "r": "0x888a465b0cd6ecd17a3105f49f2894ef00259c6bfe9d8ff1e3168d4ffdf08306",
        "s": "0x5640a8b50450df11fd1297b5529e179838611b838347f3ae3fb5089bc092032a",
        "v": 27
      },
      "vaultAddress": null
    }
  }
}
```

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| vaultAddress=null が拒否される | 高 | Mainnet 少額テストで検証、失敗時は Alternative へ |
| v 数値形式が拒否される | 高 | Phase A 成功後に検証、段階的アプローチ |
| REST API との挙動差異 | 中 | WebSocket POST 専用の形式として対応 |
| **Mainnet での実損** | 中 | 安全ガード（$11上限、即時停止）で最小化 |

## Alternative Approaches（Phase A 失敗時のフォールバック）

**判断基準**: Phase A (Step A-3) で `vaultAddress: null` が拒否された場合、以下の順序で Alternative を試す。

### Alternative A: vaultAddress を空文字列にする

**適用条件**: `vaultAddress: null` が "invalid address" 等のエラーで拒否された場合

**修正箇所**: `crates/hip3-executor/src/real_ws_sender.rs`

```rust
// PostPayload 構築時
vault_address: self.vault_address.clone().or_else(|| Some(String::new())),
```

JSON: `"vaultAddress": ""`

**検証**: Mainnet 少額テストで再検証（安全ガード適用）

### Alternative B: vaultAddress をユーザーアドレスにする

**適用条件**: Alternative A も拒否された場合

**修正箇所**: `crates/hip3-executor/src/real_ws_sender.rs`

vault なしの場合、signer_address を設定する。

```rust
vault_address: self.vault_address.clone().or_else(|| Some(self.signer_address.clone())),
```

**注意**: `RealWsSender` に `signer_address` フィールドを追加する必要あり

### 切り替え判断フロー

```
Phase A (vaultAddress: null)
    ↓
[成功] → Phase B へ
[失敗: "invalid address" 等]
    ↓
Alternative A (vaultAddress: "")
    ↓
[成功] → Phase B へ
[失敗]
    ↓
Alternative B (vaultAddress: signer_address)
    ↓
[成功] → Phase B へ
[失敗] → 一次情報の再調査が必要
```

## Verification Checklist

### Phase A: Build & Test

- [ ] Step A-1 適用: `skip_serializing_if` 削除
- [ ] Step A-2 適用: キー存在確認テスト追加
- [ ] `cargo fmt`
- [ ] `cargo clippy -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `test_post_payload_vault_address_null_is_explicit` テスト成功

### Phase A: Mainnet 少額テスト

- [ ] 安全ガード設定確認: `max_notional = 11`
- [ ] 緊急停止手順確認
- [ ] Step A-3: Mainnet 少額テストで注文送信
- [ ] JSON パースエラーが解消されるか確認
- [ ] **成功** → Phase B へ
- [ ] **失敗** → Alternative A/B を試す

### Phase B: Build & Test（Phase A 成功後）

- [ ] Step B-1a〜B-1f 適用: signature.v を u8 に変更
- [ ] Step B-2 適用: テスト修正
- [ ] `cargo fmt`
- [ ] `cargo clippy -- -D warnings`
- [ ] `cargo test --workspace`

### Phase B: Mainnet 少額テスト

- [ ] 安全ガード設定確認: `max_notional = 11`
- [ ] Step B-3: Mainnet 少額テストで注文送信
- [ ] signature.v が数値で送信されることを確認
- [ ] 注文送信成功確認
- [ ] ACK メッセージ受信確認

### 本番運用（Phase B 成功後）

- [ ] `max_notional` を通常設定に戻す（$20 等）
- [ ] 通常運用開始

## Non-Negotiable Requirements

1. **段階的検証必須**: Phase A → 検証 → Phase B → 検証 の順序を厳守
2. **キー存在確認テスト**: `vaultAddress` が JSON に含まれることを `contains_key` で検証
3. **ロールバック可能**: 問題があればすぐに戻せる状態を維持
4. **判断基準の遵守**: Phase A 失敗時は Alternative フローに従う
5. **Mainnet 安全ガード必須**: 以下を全て適用
   - `max_notional = 11` ($11、最小注文額 $10 の直上)
   - 最大テスト時間 5 分
   - ログ常時監視
   - 緊急停止手順の事前確認

## Review History

| Review | Date | Findings | Resolution |
|--------|------|----------|------------|
| #1 | 2026-01-25 | 段階的検証と実装手順の矛盾、AB検証設計不足、テスト項目不足、テスト対象列挙不足 | Phase A/B 分離、Step A-2 追加、テスト対象列挙、Alternative 判断基準明文化 |
| #2 | 2026-01-25 | vaultAddress必須仮説と一次情報の整合性不足、テスト名の不一致 | 既知の失敗記録追加、一次情報との整合性明記、実在するテスト名に修正 |
| #3 | 2026-01-25 | Testnet前提の手順がMainnet実施と矛盾、安全ガード不足 | 検証環境をMainnet少額テストに変更、安全ガード（$5上限・即時停止等）追加 |
| #4 | 2026-01-25 | test_post_request_with_vault_addressのv値が"27"と記載されているが実際は"28" | テスト一覧の値を実際のコードに合わせて修正 |
| #5 | 2026-01-25 | Mainnet最小注文額$10のため$5では注文不可 | 安全ガードを$5→$11に変更（最小注文額の直上） |

## Changelog

| Version | Date | Changes |
|---------|------|---------|
| v1.0 | 2026-01-25 | Initial plan |
| v1.1 | 2026-01-25 | レビュー対応: Phase A/B 分離、キー存在確認テスト追加、テスト対象列挙、Alternative 判断フロー追加 |
| v1.2 | 2026-01-25 | レビュー対応: 既知の失敗記録追加、一次情報との整合性明記、テスト名を実在する名前に修正 |
| v1.3 | 2026-01-25 | レビュー対応: 検証環境をMainnet少額テストに変更、安全ガード追加、緊急停止手順追加 |
| v1.4 | 2026-01-25 | レビュー対応: test_post_request_with_vault_addressのv値を実際のコード("28")に合わせて修正 |
| v1.5 | 2026-01-25 | レビュー対応: Mainnet最小注文額$10のため安全ガードを$5→$11に変更 |
