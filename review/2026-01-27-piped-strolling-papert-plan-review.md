# Plan Review: piped-strolling-papert.md (Real-Time Dashboard)

## Metadata

| Item | Value |
|------|-------|
| Reviewed Date | 2026-01-27 |
| Re-Reviewed | 2026-01-27 |
| Plan File | `~/.claude/plans/piped-strolling-papert.md` |
| Reviewer | Claude Opus 4.5 |
| Status | **Approved** ✅ |

---

## Re-Review (2026-01-27)

全ての指摘事項が対応されました。

| 指摘項目 | ステータス | 対応箇所 |
|----------|------------|----------|
| 一次情報確認セクション | ✅ Fixed | L13-28 |
| HTTPS/TLS必須要件 | ✅ Fixed | L376-462 |
| WebSocket接続制限実装 | ✅ Fixed | L401-442 (ConnectionLimiter) |
| 依存関係ワークスペース追加 | ✅ Fixed | L82-114 |
| `hip3_feed::MarketState` 明示 | ✅ Fixed | L177-186 |
| 新規crate選択理由 | ✅ Fixed | L116-124 |
| enum型への変更 | ✅ Fixed | L149-174 (DashboardMessage) |
| Integration ポイント修正 | ✅ Fixed | L323-350 |

### 追加の改善点

- Broadcast容量設計: L218-222 (32 messages buffer)
- Lagging receiver対処: L244-264
- 静的ファイル組み込み方式: L273-287 (`include_str!`)
- nginx HTTPS設定例: L444-462

**結論**: 計画は承認。実装開始可能。

---

## Initial Review (2026-01-27)

## Executive Summary

リアルタイムダッシュボード実装計画のレビュー。アーキテクチャは妥当だが、**一次情報の確認欠如**と**セキュリティ設計の不備**により、現状では承認不可。

| Category | Rating | Notes |
|----------|--------|-------|
| 技術的実現可能性 | ⚠️ Medium | 型の整合性OK、依存関係追加方法に不備 |
| セキュリティ | ❌ Insufficient | Basic auth + パブリック公開はリスク |
| 一次情報の確認 | ❌ Missing | 外部ライブラリ仕様の確認なし（CLAUDE.md違反） |
| 実装詳細度 | ⚠️ Partial | WebSocket接続制限の具体実装が不明 |
| 既存コードとの整合性 | ✅ Good | 調査結果と整合 |

---

## 1. コードベース整合性確認

### 1.1 参照型の存在確認

| 型 | 存在 | 場所 | 共有方法 | 計画整合性 |
|----|------|------|----------|------------|
| `MarketState` (feed) | ✅ | `hip3-feed/src/market_state.rs:120` | `Arc<MarketState>` | ✅ 整合 |
| `MarketStateCache` (executor) | ✅ | `hip3-executor/src/executor.rs:256` | `Arc<MarketStateCache>` | ✅ 整合 |
| `PositionTrackerHandle` | ✅ | `hip3-position/src/tracker.rs:425` | Clone（内部Arc） | ✅ 整合 |
| `HardStopLatch` | ✅ | `hip3-risk/src/hard_stop.rs:70` | `Arc<HardStopLatch>` | ✅ 整合 |
| `SignalRecord` | ✅ | `hip3-persistence/src/writer.rs:18` | Clone（値型） | ✅ 整合 |
| `Application` | ✅ | `hip3-bot/src/app.rs:79` | N/A | ✅ 整合 |

### 1.2 依存関係の現状

| 依存関係 | ワークスペース定義 | 使用状況 |
|----------|-------------------|----------|
| axum | ❌ なし | ❌ 未使用（新規追加必要） |
| tower-http | ❌ なし | ❌ 未使用（新規追加必要） |
| tokio | ✅ `{ version = "1", features = ["full"] }` | 互換性あり |
| serde | ✅ `{ version = "1", features = ["derive"] }` | 互換性あり |
| serde_json | ✅ `{ version = "1", features = ["preserve_order"] }` | 互換性あり |

### 1.3 MarketState 名前衝突

**発見**: `MarketState` が2箇所に存在

```
hip3-feed::MarketState      - BBO + AssetCtx 統合管理
hip3-executor::MarketState  - mark price 単体キャッシュ
```

**計画の参照**:
> `crates/hip3-feed/src/market_state.rs` | Source of BBO, Oracle data

**問題**: 計画で `hip3_feed::MarketState` を使用することが明示されていない。import時の混乱を避けるため明記が必要。

---

## 2. Critical Issues

### 2.1 一次情報の確認欠如 (CLAUDE.md違反)

**問題**: 計画に「参照した一次情報」セクションがない。

CLAUDE.md の規定:
> **🚨 ABSOLUTE PROHIBITION: Planning or implementing based on memory/training data is FORBIDDEN.**

確認が必要な一次情報:

| 項目 | 確認すべきソース | 確認内容 |
|------|------------------|----------|
| axum 0.7 WebSocket | [axum docs](https://docs.rs/axum/latest/axum/) | `ws` feature の使い方、upgrade handler |
| tower-http CORS | [tower-http docs](https://docs.rs/tower-http/latest/tower_http/) | CORS middleware 設定方法 |
| tower-http ServeDir | 同上 | 静的ファイル配信の設定 |
| tokio::sync::broadcast | [tokio docs](https://docs.rs/tokio/latest/tokio/sync/broadcast/) | capacity, lagging receiver の挙動 |

**要修正**: 計画に以下のセクションを追加

```markdown
## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| axum WebSocket | docs.rs | https://docs.rs/axum/0.7/axum/extract/ws/ | YYYY-MM-DD |
| ... | ... | ... | ... |

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| broadcast lagging | ドキュメント不明瞭 | unit test で確認 |
```

### 2.2 セキュリティ設計の不備

**計画の記載**:
> - Access Method: VPSにパブリック公開（ポート8080）
> - Optional basic auth via config

**リスク分析**:

| リスク | 深刻度 | 影響 |
|--------|--------|------|
| Basic auth 平文送信 | High | HTTP上ではパスワード傍受可能 |
| ブルートフォース攻撃 | Medium | 認証突破の可能性 |
| 情報漏洩（ポジション/P&L） | High | 取引戦略・資産状況の流出 |
| セッションハイジャック | Medium | 認証済みセッションの乗っ取り |

**計画の緩和策**:
> For production, use nginx reverse proxy with HTTPS.

**問題**: これは「Recommendation」として記載されているが、**必須要件**とすべき。

**要修正**:

```markdown
## Security Requirements (MANDATORY)

### 本番環境必須事項
- [ ] HTTPS/TLS 必須（nginx reverse proxy または直接TLS）
- [ ] 認証失敗時の rate limiting（5回失敗で1分ブロック等）
- [ ] 可能であれば IP 制限（VPN経由のみ等）

### 開発環境許容事項
- HTTP（localhost のみ）
- Basic auth なし
```

### 2.3 WebSocket接続制限の実装詳細不足

**計画の記載**:
> Max 10 concurrent WebSocket connections

**問題**: 具体的な実装方法が記載されていない。

**必要な実装パターン**:

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct ConnectionLimiter {
    current: AtomicUsize,
    max: usize,
}

impl ConnectionLimiter {
    pub fn new(max: usize) -> Self {
        Self {
            current: AtomicUsize::new(0),
            max,
        }
    }

    pub fn try_acquire(&self) -> Option<ConnectionGuard> {
        loop {
            let current = self.current.load(Ordering::Acquire);
            if current >= self.max {
                return None; // 接続拒否
            }
            if self.current
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(ConnectionGuard { limiter: self });
            }
        }
    }
}

pub struct ConnectionGuard<'a> {
    limiter: &'a ConnectionLimiter,
}

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.limiter.current.fetch_sub(1, Ordering::Release);
    }
}
```

**要修正**: 上記パターンまたは同等の実装方法を計画に追加。

---

## 3. Medium Issues

### 3.1 依存関係追加方法の不備

**計画の記載** (Phase 1):
```toml
[dependencies]
axum = { version = "0.7", features = ["ws"] }
tower-http = { version = "0.5", features = ["fs", "cors"] }
```

**問題**: ワークスペース依存関係への追加が明記されていない。

**修正案**:

```toml
# Step 1: ルート Cargo.toml に追加
[workspace.dependencies]
axum = { version = "0.7", features = ["ws"] }
tower-http = { version = "0.5", features = ["fs", "cors"] }
futures-util = "0.3"

# Step 2: crates/hip3-dashboard/Cargo.toml
[dependencies]
axum = { workspace = true }
tower-http = { workspace = true }
futures-util = { workspace = true }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }

# Internal dependencies
hip3-core = { path = "../hip3-core" }
hip3-feed = { path = "../hip3-feed" }
hip3-position = { path = "../hip3-position" }
hip3-risk = { path = "../hip3-risk" }
```

### 3.2 新規crate vs 既存crate拡張の判断根拠

**疑問**: なぜ `hip3-dashboard` を新規作成するのか？

既存 `hip3-telemetry` にHTTPサーバーを追加する選択肢もある。

| 選択肢 | メリット | デメリット |
|--------|---------|------------|
| 新規 `hip3-dashboard` | 責務分離が明確、独立してテスト可能 | crate数増加、依存関係増 |
| `hip3-telemetry` 拡張 | 既存構造活用、Prometheus metrics統合容易 | 責務混在の可能性 |

**判断**: 新規crateは妥当（ダッシュボードはtelemetryより広い責務を持つ）

**要修正**: 選択理由を計画に記載

```markdown
### Why New Crate

`hip3-telemetry` はメトリクス収集に特化。ダッシュボードは以下を含むため別crate:
- WebSocket リアルタイム更新
- 静的ファイル配信
- 複数データソース統合
- 認証機能
```

### 3.3 Integration ポイントの行番号依存

**計画の記載**:
> Integration in app.rs (~line 512, after Trading mode init)

**問題**: 行番号は変動するため、目印となるコード参照にすべき。

**修正案**:

```markdown
**Integration point in app.rs:**
```rust
// After: let hard_stop_latch = Arc::new(HardStopLatch::new());
// Before: let executor_loop = ExecutorLoop::new(...);
// Look for comment: "// Phase B: Trading mode initialization"
```

---

## 4. Minor Issues

### 4.1 API型設計の改善

**計画の記載**:
```rust
pub struct DashboardUpdate {
    pub type_: String, // "update" | "signal" | "risk_alert"
```

**問題**: `type_` は `String` ではなく enum + serde tag にすべき

**改善案**:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DashboardMessage {
    Snapshot(DashboardSnapshot),
    Update {
        timestamp: i64,
        markets: Option<HashMap<String, MarketDataSnapshot>>,
        positions: Option<Vec<PositionSnapshot>>,
    },
    Signal(SignalSnapshot),
    RiskAlert {
        timestamp: i64,
        alert_type: RiskAlertType,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskAlertType {
    HardStop,
    GateTriggered,
    SpreadExceeded,
}
```

**メリット**:
- コンパイル時の型安全性
- 網羅的パターンマッチ
- serde が自動で `"type": "update"` 等を生成

### 4.2 Broadcaster の容量設定

**計画の記載**:
```rust
tx: broadcast::Sender<String>, // JSON messages
```

**問題**: `broadcast::channel` の容量が未指定

**追記すべき内容**:

```rust
// Capacity considerations:
// - 100ms interval = 10 updates/sec
// - Max 10 clients
// - Buffer for slow clients: 32 messages (3.2 sec worth)
let (tx, _) = broadcast::channel::<String>(32);

// Handle lagging receivers
match rx.recv().await {
    Ok(msg) => { /* process */ }
    Err(broadcast::error::RecvError::Lagged(n)) => {
        warn!("Client lagged by {} messages, catching up", n);
    }
    Err(broadcast::error::RecvError::Closed) => break,
}
```

### 4.3 静的ファイル配信の詳細

**計画の記載**:
> `GET /` → Static HTML/JS

**問題**: 組み込み vs ファイルシステムの選択が不明

**選択肢**:

| 方式 | メリット | デメリット |
|------|---------|------------|
| `include_str!` 組み込み | 単一バイナリ、デプロイ簡単 | 変更時再コンパイル必要 |
| `ServeDir` ファイル配信 | 実行時変更可能 | ファイルパス管理必要 |

**推奨**: 組み込み方式（デプロイ簡便性優先）

```rust
async fn serve_index() -> impl IntoResponse {
    Html(include_str!("../static/index.html"))
}
```

---

## 5. Good Points

| 項目 | 評価 | コメント |
|------|------|----------|
| アーキテクチャ図 | ✅ 優秀 | ASCII図で明確、コンポーネント関係が理解しやすい |
| フェーズ分け | ✅ 適切 | 7フェーズで段階的実装、依存関係を考慮 |
| Critical Files セクション | ✅ 有用 | 変更対象ファイルが明確 |
| Verification Plan | ✅ 具体的 | 手動テスト項目が実用的 |
| 読み取り専用設計 | ✅ 正しい判断 | 制御エンドポイントなし = 攻撃面縮小 |
| UI レイアウト設計 | ✅ 明確 | 4セクション構成が視覚的に示されている |
| Docker 検証手順 | ✅ 実用的 | ポート公開とcurl確認手順あり |

---

## 6. Action Items

### Must Fix (承認前に必須)

| # | 項目 | 優先度 | 対応内容 |
|---|------|--------|----------|
| 1 | 一次情報確認セクション追加 | P0 | axum, tower-http, broadcast のドキュメント確認・記載 |
| 2 | HTTPS/TLS を必須要件に昇格 | P0 | Security セクション書き換え |
| 3 | WebSocket接続制限の実装方法 | P0 | ConnectionLimiter パターンを記載 |
| 4 | 依存関係のワークスペース追加方法 | P1 | Phase 1 のコード修正 |

### Should Fix (承認後でも可)

| # | 項目 | 優先度 | 対応内容 |
|---|------|--------|----------|
| 5 | `hip3_feed::MarketState` 明示 | P2 | 名前衝突の注意書き追加 |
| 6 | 新規crate選択の理由記載 | P2 | "Why New Crate" セクション追加 |
| 7 | `type_` を enum に変更 | P2 | API型設計の改善 |
| 8 | Integration ポイントをコード参照に | P2 | 行番号依存を解消 |

### Nice to Have (将来改善)

| # | 項目 | 優先度 | 対応内容 |
|---|------|--------|----------|
| 9 | 認証失敗時のrate limiting | P3 | 5回失敗で1分ブロック等 |
| 10 | IP制限オプション | P3 | 許可IPリスト機能 |
| 11 | Broadcast容量設計の明記 | P3 | lagging receiver対応 |
| 12 | 静的ファイル配信方式の決定 | P3 | 組み込み vs ファイルシステム |

---

## 7. Conclusion

### 承認ステータス: **Needs Revision**

計画のアーキテクチャと全体設計は妥当だが、以下の理由により現状では承認不可:

1. **CLAUDE.md違反**: 一次情報の確認・記載がない
2. **セキュリティ不備**: Basic auth + HTTP でパブリック公開は危険
3. **実装詳細不足**: 接続制限の具体的実装方法が不明

### 次のステップ

1. Must Fix 項目 (#1-4) を対応
2. 修正版計画を再レビュー依頼
3. 承認後、Spec ファイル作成 → 実装開始

---

## Appendix: コードベース調査詳細

### A. Application struct 現在のフィールド

```rust
// crates/hip3-bot/src/app.rs:79-113
pub struct Application {
    config: AppConfig,
    market_state: Arc<MarketState>,           // ← Dashboard で使用
    spec_cache: Arc<SpecCache>,
    risk_gate: RiskGate,
    detector: DislocationDetector,
    writer: ParquetWriter,
    followup_writer: Arc<tokio::sync::Mutex<FollowupWriter>>,
    cross_tracker: CrossDurationTracker,
    daily_stats: Option<DailyStatsReporter>,
    last_stats_output: Instant,
    xyz_dex_id: Option<DexId>,
    gate_block_state: HashMap<(MarketKey, String), bool>,
    market_threshold_map: HashMap<u32, Decimal>,

    // Phase B: Trading mode components
    executor_loop: Option<Arc<ExecutorLoop>>,
    position_tracker: Option<PositionTrackerHandle>,  // ← Dashboard で使用
    position_tracker_handle: Option<tokio::task::JoinHandle<()>>,
    connection_manager: Option<Arc<ConnectionManager>>,
    risk_event_tx: Option<mpsc::Sender<ExecutionEvent>>,
}
```

### B. ワークスペースメンバー一覧

| # | Crate | 用途 |
|---|-------|------|
| 1 | hip3-core | 共通型定義 |
| 2 | hip3-ws | WebSocket 接続管理 |
| 3 | hip3-feed | 市場データフィード |
| 4 | hip3-registry | 銘柄レジストリ |
| 5 | hip3-risk | リスク管理 |
| 6 | hip3-detector | シグナル検出 |
| 7 | hip3-executor | 注文執行 |
| 8 | hip3-position | ポジション管理 |
| 9 | hip3-telemetry | メトリクス・ログ |
| 10 | hip3-persistence | データ永続化 |
| 11 | hip3-bot | メインアプリケーション |
| **12** | **hip3-dashboard** | **新規追加予定** |

### C. HardStopLatch 構造

```rust
// crates/hip3-risk/src/hard_stop.rs:70
pub struct HardStopLatch {
    triggered: AtomicBool,
    triggered_at: AtomicU64,
    reason: RwLock<Option<HardStopReason>>,
}

// Dashboard で必要な情報
impl HardStopLatch {
    pub fn is_triggered(&self) -> bool;
    pub fn triggered_at(&self) -> Option<u64>;
    pub fn reason(&self) -> Option<HardStopReason>;
}
```

### D. SignalRecord 構造

```rust
// crates/hip3-persistence/src/writer.rs:18
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalRecord {
    pub timestamp_ms: i64,
    pub market_key: String,
    pub side: String,
    pub raw_edge_bps: f64,
    pub net_edge_bps: f64,
    pub oracle_px: f64,
    pub best_px: f64,
    pub best_size: f64,
    pub suggested_size: f64,
    pub signal_id: String,
}
```

---

*Review generated by Claude Opus 4.5*
