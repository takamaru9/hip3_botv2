# Tick Interval 最適化計画

## Metadata

| Item | Value |
|------|-------|
| Created | 2026-01-31 |
| Task | エッジエロージョン削減のためtick interval短縮 |
| Status | `[PLANNING]` |

## 問題分析

### エッジエロージョンの原因

| フェーズ | 所要時間 | 備考 |
|---------|---------|------|
| WSメッセージ受信 → 検出完了 | < 1ms | 最適化済み |
| 検出 → on_signal() 完了 | < 1ms | 最適化済み |
| on_signal() → バッチキュー投入 | < 0.1ms | 最適化済み |
| **バッチキュー → tick()実行** | **0-100ms** | **最大ボトルネック** |
| tick() → 署名完了 | 1-5ms | CPU bound |
| 署名 → WS送信完了 | < 0.1ms | 最適化済み |

### 現状

- `BatchConfig::default()` で `interval_ms: 100` がハードコード
- TOML設定で変更不可能
- 平均待機時間: 50ms、最悪ケース: 100ms

### Rate Limit余裕

| 制限 | 値 | 現在使用率 |
|------|-----|-----------|
| Messages/minute | 2000 | 600 (30%) |
| Inflight messages | 100 | OK |

20ms interval = 3000 ticks/minute だが、実際に送信されるのはオーダーがある時のみなので問題なし。

---

## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| Rate Limits | Hyperliquid Docs | https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits | 2026-01-31 |

## 実装計画

### P0: 設定可能化 + デフォルト変更

#### P0-1: ExecutorConfig 追加

**ファイル:** `crates/hip3-bot/src/config.rs`

```rust
/// Executor configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    /// Batch processing interval in milliseconds.
    /// Lower values reduce latency but increase CPU usage.
    /// Default: 20ms (was 100ms)
    #[serde(default = "default_batch_interval_ms")]
    pub batch_interval_ms: u64,
}

fn default_batch_interval_ms() -> u64 {
    20 // 100ms → 20ms
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            batch_interval_ms: default_batch_interval_ms(),
        }
    }
}
```

#### P0-2: AppConfig に executor フィールド追加

**ファイル:** `crates/hip3-bot/src/config.rs`

```rust
pub struct AppConfig {
    // ... 既存フィールド ...

    /// Executor configuration.
    #[serde(default)]
    pub executor: ExecutorConfig,
}
```

#### P0-3: BatchConfig 構築時に設定値を使用

**ファイル:** `crates/hip3-bot/src/app.rs`

```rust
// Before
let batch_scheduler = Arc::new(BatchScheduler::new(
    BatchConfig::default(),
    // ...
));

// After
let batch_config = BatchConfig {
    interval_ms: self.config.executor.batch_interval_ms,
    ..BatchConfig::default()
};
let batch_scheduler = Arc::new(BatchScheduler::new(
    batch_config,
    // ...
));
```

#### P0-4: TOML 設定追加

**ファイル:** `config/mainnet-trading-parallel.toml`

```toml
[executor]
batch_interval_ms = 20  # 100ms → 20ms for lower latency
```

---

## 期待効果

| 指標 | Before | After |
|------|--------|-------|
| 平均待機時間 | 50ms | 10ms |
| 最悪ケース待機 | 100ms | 20ms |
| エッジエロージョン | ~22.6 bps | 推定 ~10-15 bps |

---

## 修正ファイル一覧

| ファイル | 変更内容 |
|----------|----------|
| `crates/hip3-bot/src/config.rs` | `ExecutorConfig` 追加、`AppConfig` に `executor` フィールド追加 |
| `crates/hip3-bot/src/app.rs` | `BatchConfig` 構築時に設定値を使用 |
| `config/mainnet-trading-parallel.toml` | `[executor]` セクション追加 |
| `config/default.toml` | `[executor]` セクション追加 |

---

## 検証計画

```bash
# 1. ビルド確認
cargo fmt && cargo clippy -- -D warnings && cargo check

# 2. テスト
cargo test -p hip3-bot -p hip3-executor

# 3. ローカル確認
HIP3_CONFIG=config/mainnet-trading-parallel.toml cargo run --release

# 4. ログで確認
# "Batch scheduler initialized with interval: 20ms" のようなログ追加も検討
```

---

## リスク評価

| リスク | 影響 | 対策 |
|--------|------|------|
| CPU使用率増加 | 低 | 20msは十分余裕あり |
| Rate Limit | なし | 2000 msg/min に対し余裕あり |
| 互換性 | なし | デフォルト値変更のみ、既存設定は引き続き動作 |
