# Phase A コードレビュー修正計画

**対象**: Oracle/Mark Dislocation Taker Phase A実装
**レビュー**: `/Users/taka/crypto_trading_bot/hip3_botv2/review/2026-01-18-oracle-dislocation-taker-code-review.md`
**作成日**: 2026-01-19

## 設計決定事項

| 項目 | 決定 |
|------|------|
| taker_fee_bps | HIP-3適用後のeffective bps（設定値をそのまま使用） |
| Parquet方式 | run_id付き別ファイル（`signals_YYYY-MM-DD_run-<uuid>.parquet`） |
| P1-1 tick_size | API調査して実装（perpDexs/metaレスポンスを確認）|

---

## 修正優先順序

| 順序 | ID | 問題 | ファイル | 理由 |
|------|-----|------|---------|------|
| 1 | P0-1 | ParquetWriter追記問題 | hip3-persistence/writer.rs | データ保存の基盤 |
| 2 | P0-4 | taker_fee_bps未使用 | hip3-detector/detector.rs, fee.rs | 検出ロジックの正確性 |
| 3 | P0-2 | RiskGate EWMA汚染 | hip3-risk/gates.rs | 検出ロジック依存 |
| 4 | P0-3 | WS heartbeat過剰ping | hip3-ws/connection.rs | 接続安定性 |
| 5 | P1-2 | activeAssetCtxパターン | hip3-ws/subscription.rs | READY状態管理 |
| 6 | P1-1 | MarketSpec精度 | hip3-registry/spec_cache.rs | Phase B準備 |
| 7 | P1-3 | ParamChange wiring | app.rs (スタブのみ) | Phase B準備 |
| 8 | P2-1 | oracle_ageメトリクス順 | hip3-bot/app.rs | 軽微 |
| 9 | P2-3 | backoff off-by-one | hip3-ws/connection.rs | 軽微 |

---

## P0修正詳細

### P0-1: ParquetWriter run_id付きファイル方式

**ファイル**: `crates/hip3-persistence/src/writer.rs`

**問題**: flush毎に新しいArrowWriterを作成し、同一ファイルに書くとParquetファイルが破損

**方針**: run_id付き別ファイル（`signals_YYYY-MM-DD_run-<uuid>.parquet`）
- プロセス継続中は単一writerを保持
- 日付変更時にclose→新run_id付きファイルへrotate
- 再起動時は常に新run_idで新ファイル作成（追記の問題を回避）
- 分析側はglobで複数ファイルを読む

**修正内容**:
1. `run_id: String` (UUID) を起動時に生成
2. `ActiveWriter`構造体を追加（writer, date, run_id, schemaを保持）
3. `flush()`で日付変更時のみwriterをclose→新ファイルへrotate
4. `Drop`実装でバッファflush→writerclose
5. **日付取得をトレイト注入可能に**（テスト用）

```rust
/// 日付取得トレイト（テスト時にモック可能）
pub trait DateProvider: Send + Sync {
    fn today(&self) -> String;
}

pub struct RealDateProvider;
impl DateProvider for RealDateProvider {
    fn today(&self) -> String {
        Utc::now().format("%Y-%m-%d").to_string()
    }
}

struct ActiveWriter {
    writer: ArrowWriter<File>,
    date: String,
    run_id: String,
    schema: Arc<Schema>,
}

impl ParquetWriter {
    pub fn new(base_dir: &str, max_buffer_size: usize) -> Self {
        Self::with_date_provider(base_dir, max_buffer_size, Arc::new(RealDateProvider))
    }

    pub fn with_date_provider(
        base_dir: &str,
        max_buffer_size: usize,
        date_provider: Arc<dyn DateProvider>,
    ) -> Self {
        let run_id = Uuid::new_v4().to_string()[..8].to_string();
        // ...
    }

    pub fn flush(&mut self) -> PersistenceResult<()> {
        let today = self.date_provider.today();
        // 日付変更時にrotate（同一run_id内で日付変更）
        if self.active_writer.as_ref().map(|w| &w.date) != Some(&today) {
            self.close_active_writer()?;
        }
        // ファイル名: signals_YYYY-MM-DD_run-<uuid>.parquet
    }
}
```

**テスト追加**:
- `test_multiple_flushes_same_run`: 複数flush→1ファイル、読み取り可能確認（モック日付使用）
- `test_date_rotation`: 日付変更時のrotate確認（`MockDateProvider`で日付を切り替え）
- `test_run_id_unique`: 異なるwriter instanceは異なるrun_idを持つ

---

### P0-4: DetectorConfig taker_fee_bps反映

**ファイル**:
- `crates/hip3-detector/src/fee.rs`
- `crates/hip3-detector/src/detector.rs`

**問題**: `config.taker_fee_bps`が無視され、`UserFees::default()`を使用

**修正内容**:

`fee.rs`に追加:
```rust
impl UserFees {
    /// effective bps（HIP-3 2x適用済み）からUserFeesを作成
    pub fn from_effective_taker_bps(effective_taker_bps: Decimal) -> Self {
        let base_taker_bps = effective_taker_bps / HIP3_FEE_MULTIPLIER;
        Self { taker_bps: base_taker_bps, ... }
    }
}
```

`detector.rs`修正:
```rust
pub fn new(config: DetectorConfig) -> Self {
    let user_fees = UserFees::from_effective_taker_bps(config.taker_fee_bps);
    let fee_calculator = FeeCalculator::new(user_fees, ...);
}
```

**テスト追加**:
- `test_config_taker_fee_bps_used`: config値がfee計算に反映される確認
- `test_user_fees_from_effective`: effective→base→effective round trip

---

### P0-2: RiskGate Early Return

**ファイル**: `crates/hip3-risk/src/gates.rs`

**問題**: ブロック条件でも全ゲートを実行し、EWMA等が無効データで汚染

**修正内容**:
```rust
pub fn check_all(&mut self, ...) -> RiskResult<Vec<GateResult>> {
    // Gate 1: Oracle Freshness
    let gate1 = self.check_oracle_fresh(oracle_age_ms);
    if gate1.is_block() { return Err(GateBlocked { gate: "oracle_fresh", ... }); }

    // Gate 7: BBO Update
    let gate7 = self.check_bbo_update(bbo_age_ms);
    if gate7.is_block() { return Err(...); }

    // ... 前提条件gateを先に評価

    // Gate 3: Spread Shock (EWMA更新) - 前提条件pass後のみ実行
    let gate3 = self.check_spread_shock(snapshot);
    if gate3.is_block() { return Err(...); }

    // ... 残りのgate
}
```

**Gate評価順序**:
1. oracle_fresh (前提条件)
2. bbo_update (前提条件)
3. ctx_update (前提条件)
4. time_regression (前提条件)
5. mark_mid_divergence (BBO有効性)
6. spread_shock (EWMA更新 - ここでのみ副作用)
7. oi_cap, param_change, halt

**テスト追加**:
- `test_ewma_not_updated_when_bbo_null`: null BBOでEWMA変化なし確認
- `test_early_return_gate_order`: oracle staleでspread_shockが実行されない確認

---

### P0-3: WS Heartbeat should_send_heartbeat使用

**ファイル**: `crates/hip3-ws/src/connection.rs`

**問題**: `should_send_heartbeat()`が未使用で常にping送信

**修正内容**:
```rust
_ = self.heartbeat.wait_for_check() => {
    if self.heartbeat.is_timed_out() {
        return Err(WsError::HeartbeatTimeout);
    }
    // 条件を追加
    if self.heartbeat.should_send_heartbeat() {
        let ping = WsRequest::ping();
        write.send(Message::Text(serde_json::to_string(&ping)?)).await?;
        self.heartbeat.record_ping();
    }
}
```

**テスト追加**（時間経過に依存しない形）:
- `test_should_send_heartbeat_respects_waiting_for_pong`: ping送信後は`waiting_for_pong=true`でfalseを返す
- `test_pong_clears_waiting_flag`: pong受信後は`waiting_for_pong=false`になる

**注**: `should_send_heartbeat()`の「一定時間経過後にtrue」のテストは時刻注入が必要なためPhase Bで検討

---

## P1修正詳細

### P1-2: activeAssetCtxパターン修正（両方拾う形）

**ファイル**: `crates/hip3-ws/src/subscription.rs`

**問題**: `"assetCtx"`パターンが`"activeAssetCtx"`にマッチしない

**修正方針**: 大小文字差を吸収して両方拾う形に
```rust
impl RequiredChannel {
    pub fn matches(&self, channel: &str) -> bool {
        let lower = channel.to_ascii_lowercase();
        lower.contains(self.channel_pattern())
    }

    pub fn channel_pattern(&self) -> &'static str {
        match self {
            Self::Bbo => "bbo",
            Self::AssetCtx => "assetctx",  // 小文字で比較（activeAssetCtx, assetCtx両方にマッチ）
            Self::OrderUpdates => "orderupdates",
        }
    }
}
```

**テスト追加**:
- `test_active_asset_ctx_matching`: "activeAssetCtx:BTC" にマッチ
- `test_legacy_asset_ctx_matching`: "assetCtx:perp:0" にもマッチ

---

### P1-1: MarketSpec tick_size設定（API調査→実装）

**ファイル**: `crates/hip3-registry/src/spec_cache.rs`

**前提**: 実装前にHyperliquid perpDexs/meta APIレスポンスを確認し、tick_sizeフィールドの有無と形式を特定する

**調査項目**:
1. `info.hyperliquid.xyz/info` の `perpDexs` エンドポイントのレスポンス構造
2. tick_size に該当するフィールド名（`tickSize`, `priceTickSize` 等）
3. フィールドが存在しない場合の導出ルール

**修正内容**（調査結果に応じて調整）:
1. `RawPerpSpec`に該当フィールドを追加（`#[serde(rename = "...")]`）
2. `parse_spec()`でtick_sizeをパース
3. フィールドが無い場合のfallback: `sz_decimals`から導出 or デフォルト(0.01)
4. `decimals_from_tick_size()`でmax_price_decimals導出

**テスト追加**:
- `test_parse_spec_with_tick_size`: APIから取得したtick_sizeが正しくパースされる
- `test_parse_spec_fallback`: フィールド無しの場合のfallback動作

---

### P1-3: ParamChange wiring (Phase Aスタブ)

**ファイル**: `crates/hip3-bot/src/app.rs`

Phase Aではコメントによるスタブのみ:
```rust
// P1-3: Phase B TODO - Add periodic spec refresh task
// let spec_refresh_interval = tokio::time::interval(Duration::from_secs(300));
```

---

## P2修正詳細

### P2-1: oracle_ageメトリクス更新順

**ファイル**: `crates/hip3-bot/src/app.rs`

**修正**: `update_ctx()`の後に`get_oracle_age_ms()`を呼び出す

---

### P2-3: exponential backoff修正

**ファイル**: `crates/hip3-ws/src/connection.rs`

**修正**:
```rust
let exponent = attempt.saturating_sub(1).min(10);  // 2^(attempt-1)
let delay = base.saturating_mul(1u64 << exponent);
```

---

## 検証計画

### 1. 単体テスト
```bash
cargo test -p hip3-persistence
cargo test -p hip3-detector
cargo test -p hip3-risk
cargo test -p hip3-ws
cargo test -p hip3-registry
cargo test --workspace
```

### 2. 統合検証
```bash
cargo run --release -- --config config/default.toml
```

**確認項目**:
- [ ] Parquetファイルが正しく作成・追記される
- [ ] 複数flush後もファイルが読み取り可能
- [ ] READY-MD状態に到達する
- [ ] Heartbeatエラーが発生しない
- [ ] シグナル検出が適切なedge閾値で動作する

### 3. Parquet検証
```bash
# parquet-toolsで読み取り確認（run_id付きファイル）
parquet-tools head ./data/signals/signals_YYYY-MM-DD_run-*.parquet

# 複数ファイルの統合読み取り（分析時）
# Python: pyarrow.parquet.read_table("./data/signals/")
```

---

## リスクと対策

| リスク | 影響度 | 対策 |
|--------|--------|------|
| ParquetWriter変更によるデータ損失 | 高 | 既存データバックアップ、テスト環境で検証 |
| Fee計算変更によるシグナル数変化 | 中 | 変更前後でシグナル数比較 |
| Early returnによる検出漏れ | 低 | 元々Blockケースなので影響なし |
| Heartbeat変更による接続不安定 | 中 | ログ監視 |

---

## Critical Files

| ファイル | 修正内容 |
|---------|---------|
| `crates/hip3-persistence/src/writer.rs` | P0-1: Parquet Writer全面改修 |
| `crates/hip3-risk/src/gates.rs` | P0-2: RiskGate early return |
| `crates/hip3-ws/src/connection.rs` | P0-3, P2-3: Heartbeat/Backoff |
| `crates/hip3-detector/src/detector.rs` | P0-4: taker_fee_bps |
| `crates/hip3-detector/src/fee.rs` | P0-4: from_effective_taker_bps |
| `crates/hip3-ws/src/subscription.rs` | P1-2: activeAssetCtx |
| `crates/hip3-registry/src/spec_cache.rs` | P1-1: tick_size |
| `crates/hip3-bot/src/app.rs` | P1-3スタブ, P2-1 |
