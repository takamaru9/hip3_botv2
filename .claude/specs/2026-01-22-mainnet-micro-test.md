# メインネット少額テスト設定 Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-22 |
| Last Updated | 2026-01-25 |
| Status | `[IN_PROGRESS]` - 初約定成功、追加バグ修正中 |
| Source Plan | `.claude/plans/ethereal-sauteeing-galaxy.md` |

---

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1 | TLT の asset_idx を perpDexs API から取得 | [x] DONE | TLT は削除済み、NVDA (index 23) に変更 |
| P2 | config/mainnet-test.toml 作成 | [x] DONE | xyz:NVDA, max_notional=20 |
| P3 | signer_address 導出・設定 | [x] DONE | 0xB3B8aEfcC0b4b2CBFab3439FD042e7658F638998 |
| P4 | メインネットテスト起動 | [x] DONE | Trading mode で稼働中 |
| P5 | シグナル発生確認 | [x] DONE | 手動テストで確認 |
| P6 | 注文送信確認 | [x] DONE | xyz:SILVER 注文送信成功 |
| P7 | Fill 受信確認 | [x] DONE | 0.2 @ $104.63 約定確認 (2026-01-25) |
| P8 | PositionTracker pending 二重計上バグ修正 | [x] DONE | register_order_actor_only() API 追加 |
| P9 | ExecutorConfig notional 上限追従 | [x] DONE | detector.max_notional に同期 |
| P10 | 再現テスト追加 | [x] DONE | test_register_order_actor_only_does_not_double_count_caches |
| P11 | BUG-001〜008 修正 | [x] DONE | 多数のWS/署名関連バグを修正 |
| P12 | BUG-011 xyz asset ID修正 | [x] DONE | meta(dex=xyz) API使用 |
| P13 | BUG-009/010 修正 | [ ] TODO | v 文字列型、WS形式 |
| P14 | Preflight テスト修正 | [x] DONE | perp_dex_id=1に伴うアサーション値更新 |

---

## Deviations from Plan

### 1) 市場変更: TLT → NVDA

**Original (Plan)**:
> `asset_idx = ???  # perpDexs API から取得した TLT の index`
> `coin = "xyz:TLT"`

**Actual Implementation**:
```toml
asset_idx = 23
coin = "xyz:NVDA"
```

**Reason**:
- perpDexs API を確認したところ、xyz:TLT は Hyperliquid から削除されていた
- 以前の index 21 は現在 xyz:NATGAS に変更
- Phase A で高 EV (38.80bps) だった xyz:NVDA (index 23) を選択

### 2) signer_address 確定

**Original (Plan)**:
> `signer_address = "..."  # HIP3_TRADING_KEY から導出されるアドレス（推奨）`

**Actual Implementation**:
```toml
signer_address = "0xB3B8aEfcC0b4b2CBFab3439FD042e7658F638998"
```

**Derivation**:
- Private key: `0x7cc63bcd7c544da0f0b93faba43a16272a7c25c868a95e77fc76385b64b8726a`
- eth_account.Account.from_key() で導出

---

## Key Implementation Details

### 設定ファイル

**File**: `config/mainnet-test.toml`

```toml
mode = "trading"
ws_url = "wss://api.hyperliquid.xyz/ws"
info_url = "https://api.hyperliquid.xyz/info"
xyz_pattern = "xyz"
is_mainnet = true

user_address = "0x0116A3D95994BcC7D6A84380ED6256FBb32cD25D"
signer_address = "0xB3B8aEfcC0b4b2CBFab3439FD042e7658F638998"
private_key = "use_env"

[[markets]]
asset_idx = 23
coin = "xyz:NVDA"

[detector]
max_notional = 20  # $20 limit
```

### 起動確認ログ

```
INFO hip3_bot: Starting HIP-3 Bot v0.1.0
INFO hip3_bot: Configuration loaded, config.mode: Trading
INFO hip3_bot::app: Starting application, mode: Trading
INFO hip3_bot::app: Initializing Trading mode components
INFO hip3_bot::app: Signer initialized,
  trading_address: Some(0xb3b8aefcc0b4b2cbfab3439fd042e7658f638998),
  user_address: Some("0x0116A3D95994BcC7D6A84380ED6256FBb32cD25D"),
  expected_signer_address: Some(0xb3b8aefcc0b4b2cbfab3439fd042e7658f638998),
  vault_address: None,
  is_mainnet: true
INFO hip3_bot::app: Trading mode initialized with ExecutorLoop and PositionTracker
INFO hip3_ws::connection: Trading subscriptions sent, user: 0x0116A3D95994BcC7D6A84380ED6256FBb32cD25D
```

### 市場データ受信確認

```
DEBUG hip3_feed::parser: Hyperliquid AssetCtx update,
  key: MarketKey { dex: DexId(0), asset: AssetId(23) },
  coin: xyz:NVDA,
  oracle: 185.37,
  mark: 185.38,
  funding: 0.00000625

DEBUG hip3_feed::parser: Hyperliquid BBO update,
  key: MarketKey { dex: DexId(0), asset: AssetId(23) },
  coin: xyz:NVDA,
  bid: 185.38,
  ask: 185.42
```

### Safety Fixes (2026-01-22 レビュー後)

1. **PositionTracker pending 二重計上修正**
   - TrySendError::Full 経路で pending_markets_cache が二重計上される問題を修正
   - `register_order_actor_only()` API を追加
   - 該当ファイル: `crates/hip3-position/src/tracker.rs`, `crates/hip3-executor/src/executor.rs`

2. **ExecutorConfig notional 上限追従**
   - detector.max_notional ($20) を ExecutorConfig にも適用
   - 該当ファイル: `crates/hip3-bot/src/app.rs`

3. **検証結果**
   - `cargo test -p hip3-position -p hip3-executor` → 46 passed
   - 再現テスト: `test_register_order_actor_only_does_not_double_count_caches`

---

## 実行コマンド

```bash
export HIP3_TRADING_KEY="7cc63bcd7c544da0f0b93faba43a16272a7c25c868a95e77fc76385b64b8726a"
cargo run --release -- --config config/mainnet-test.toml
```

---

## 既知の制約・注意事項

| 項目 | 内容 |
|------|------|
| HardStop | 全cancel+全flatten 未実装 → **手動 flatten 必須** |
| 緊急対応 | Hyperliquid UI で手動クローズ |
| 上限 | max_notional = $20 |
| 市場時間 | NVDA は NYSE 上場、米国市場時間外は BBO 更新が低頻度 |

---

## Blocking Issues (2026-01-25 Updated)

テスト中に以下のバグが発見され、修正が必要：

### 解決済み

| ID | 概要 | 修正日 |
|----|------|--------|
| ~~BUG-001~~ | subscriptionResponse ACKパース仕様ズレ | ✅ 2026-01-24 |
| ~~BUG-002~~ | orderUpdates 配列形式非対応 | ✅ 2026-01-24 |
| ~~BUG-003~~ | Signature r/s に 0x prefix なし | ✅ 2026-01-24 |
| ~~BUG-004~~ | 価格/サイズ精度制限未適用 | ✅ 2026-01-24 |
| ~~BUG-005~~ | Mark price欠損時Gate Fail Open | ✅ 2026-01-24 |
| ~~BUG-008~~ | SpecCache 初期化されていない | ✅ 2026-01-24 |
| ~~BUG-011~~ | xyz perp asset ID計算誤り | ✅ 2026-01-25 |

### 未解決（現在のブロッカー）

| ID | 概要 | 計画 | ステータス |
|----|------|------|------------|
| BUG-009 | Signature v フィールドが数値型 | `.claude/plans/2026-01-25-signature-v-string-type.md` | HIGH - 計画DRAFT |
| BUG-010 | WS POST JSON形式不正 | `.claude/plans/2026-01-25-ws-post-json-format-fix.md` | HIGH - 計画DRAFT |
| BUG-006 | WS shutdown pathがtask終了しない | `.claude/plans/2026-01-24-review-findings-fix.md` (F2) | MEDIUM - 計画DRAFT |
| BUG-007 | orderUpdates statusマッピング不完全 | `.claude/plans/2026-01-24-review-findings-fix.md` (F3) | MEDIUM - 計画DRAFT |

### BUG-009 影響 (HIGH)

- `v: u8` が数値（`28`）としてシリアライズ
- Hyperliquid API は文字列（`"28"`）を期待
- JSONパースエラーで失敗

### BUG-010 影響 (HIGH)

- `vaultAddress` フィールドがNone時に省略
- WebSocket POSTでは必須の可能性
- REST APIとWebSocket POSTで要件が異なる可能性

---

## Progress Highlights

### 2026-01-25: 初約定成功

```json
{
  "status": "ok",
  "response": {
    "type": "order",
    "data": {
      "statuses": [{
        "filled": {
          "totalSz": "0.2",
          "avgPx": "104.63",
          "oid": 302228651101
        }
      }]
    }
  }
}
```

- 銘柄: xyz:SILVER
- サイズ: 0.2
- 約定価格: $104.63
- 注文ID: 302228651101

---

## Next Steps

1. ~~BUG-001〜008, 011 修正~~ ✅ 完了
2. BUG-009（Signature v 文字列型）を修正 ← **最優先**
3. BUG-010（WS POST形式）を修正
4. BUG-006/007 を順次修正
5. シグナル発生→自動注文→約定のE2Eフロー確認
6. ポジション追跡が正常に動作することを確認
7. 100トレード完了を目指す
8. テスト完了後、結果を分析してレポート作成
