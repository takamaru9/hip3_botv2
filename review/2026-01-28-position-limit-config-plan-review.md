# Position Limit Configuration Plan Review

## Metadata

| Item | Value |
|------|-------|
| Review Date | 2026-01-28 |
| Plan File | `~/.claude/plans/piped-strolling-papert.md` |
| Reviewer | Claude Opus 4.5 |
| Status | **要修正** |

---

## Summary

同時保有ポジション制限機能（`max_concurrent_positions`）を追加する計画のレビュー。
設計方針は妥当だが、**いくつかの重要な問題点**を修正する必要がある。

---

## 評価

### 良い点 ✅

| 項目 | 評価 |
|------|------|
| **目的の明確性** | ハードコードされた制限をconfig化する意図が明確 |
| **既存コード調査** | ExecutorConfig, PositionTracker.position_count()の参照あり |
| **設定項目の設計** | `max_concurrent_positions`, `max_total_notional`, `max_notional_per_market`は適切 |
| **デフォルト値** | serde(default)でオプショナル化は良い設計 |
| **検証計画** | fmt/clippy/check + ユニットテスト + VPSテストの3段階は適切 |
| **リスク考慮** | 既存ポジション・reduce_only・HardStopへの影響を考慮 |

### 問題点 ❌

#### 1. **RejectReasonの場所が間違っている** (Critical)

**計画の記述:**
```
### Step 4: RejectReason に新しい理由を追加
`crates/hip3-executor/src/executor.rs` の `RejectReason` enum
```

**実際:**
```
RejectReason は crates/hip3-core/src/execution.rs:249 に定義されている
```

**修正:**
- 修正ファイルリストに `crates/hip3-core/src/execution.rs` を追加
- Step 4 のファイルパスを修正

---

#### 2. **Gate番号が実際のコードと不一致** (Major)

**計画のGate順序:**
```
Gate 1: HardStop
Gate 2: READY-TRADING
Gate 3: MaxPositionPerMarket
Gate 4: MaxPositionTotal
Gate 4B: MaxConcurrentPositions  ← NEW
Gate 5: has_position
Gate 6: PendingOrder
Gate 7: ActionBudget
```

**実際のコード (executor.rs):**
```
Gate 1: HardStop
Gate 2: READY-TRADING (skipped, handled by bot)
Gate 3: MaxPositionPerMarket
Gate 4: MaxPositionTotal
Gate 5: has_position
Gate 6: PendingOrder (try_mark_pending_market)
Gate 7: ActionBudget
```

**問題:**
- 計画では「Gate 4B」だが、実際は「Gate 5前」に挿入
- Gate番号がずれている（計画のGate 5,6,7は実際は Gate 5,6,7）

**修正:**
- Gate 4B → Gate 4.5 または「Gate 4後、Gate 5前」と明記
- 全Gate番号をコードと一致させる

---

#### 3. **論理的矛盾: 既存マーケットへの追加許可** (Critical)

**計画の記述:**
```rust
// Gate 4B: MaxConcurrentPositions
let current_position_count = self.position_tracker.position_count();
if current_position_count >= self.config.max_concurrent_positions {
    // 既存マーケットへの追加は許可、新規マーケットはブロック
    if !self.position_tracker.has_position(market) {
        // ... reject ...
    }
}
```

**問題:**
- Gate 5 (`has_position`) で既にポジションがあるとSkippedになる
- つまりGate 4.5に到達する時点で、そのマーケットにはポジションが**ない**
- `has_position(market) == true` のケースは Gate 4.5 で発生しない

**実際の動作:**
```
1. 既存ポジションあり → Gate 5で Skipped(AlreadyHasPosition)
2. 新規マーケット → Gate 4.5で評価される
```

**結論:**
- 「既存マーケットへの追加は許可」のロジックは**不要**
- 単純に `position_count >= max` でrejectすればよい

**修正後のStep 3:**
```rust
// Gate 4.5: MaxConcurrentPositions
let current_position_count = self.position_tracker.position_count();
if current_position_count >= self.config.max_concurrent_positions {
    debug!(
        current = current_position_count,
        max = self.config.max_concurrent_positions,
        market = %market,
        "Max concurrent positions reached"
    );
    return ExecutionResult::rejected(RejectReason::MaxConcurrentPositions);
}
```

---

#### 4. **修正ファイルリストの漏れ** (Major)

**現在の計画:**
```
| ファイル | 変更内容 |
|----------|----------|
| `crates/hip3-bot/src/config.rs` | PositionConfig 構造体追加 |
| `crates/hip3-executor/src/executor.rs` | Gate追加、Config読み込み |
| `config/default.toml` | [position] セクション追加 |
| `config/mainnet-optimal-test.toml` | [position] セクション追加 |
```

**漏れ:**
| ファイル | 変更内容 |
|----------|----------|
| `crates/hip3-core/src/execution.rs` | RejectReason::MaxConcurrentPositions 追加 |
| `crates/hip3-bot/src/app.rs` | PositionConfig → ExecutorConfig 変換 |

---

#### 5. **ExecutorConfig 修正の詳細不足** (Minor)

**計画:**
```rust
impl ExecutorConfig {
    pub fn from_position_config(pos: &PositionConfig) -> Self {
        Self {
            max_concurrent_positions: pos.max_concurrent_positions,
            // ...
        }
    }
}
```

**問題:**
- 現在の `ExecutorConfig` には `max_concurrent_positions` フィールドがない
- フィールド追加が明示されていない

**修正:**
```rust
pub struct ExecutorConfig {
    pub max_notional_per_market: Decimal,
    pub max_notional_total: Decimal,
    pub max_concurrent_positions: usize,  // NEW
}
```

---

## 一次情報の確認状況

| 項目 | 確認状況 | 備考 |
|------|----------|------|
| ExecutorConfig 現状 | ✅ 確認済 | executor.rs:221-237 |
| RejectReason 場所 | ⚠️ 誤認識 | execution.rs:249（executor.rsではない） |
| PositionTracker.position_count() | ✅ 確認済 | tracker.rs:813-815 |
| Gate順序 | ⚠️ 一部不正確 | 実コードと番号ずれ |

---

## 修正提案

### 修正版 修正ファイル

| ファイル | 変更内容 |
|----------|----------|
| `crates/hip3-bot/src/config.rs` | PositionConfig 構造体追加 |
| `crates/hip3-core/src/execution.rs` | RejectReason::MaxConcurrentPositions 追加 |
| `crates/hip3-executor/src/executor.rs` | ExecutorConfig にフィールド追加、Gate追加 |
| `crates/hip3-bot/src/app.rs` | PositionConfig → ExecutorConfig 変換 |
| `config/default.toml` | [position] セクション追加 |
| `config/mainnet-optimal-test.toml` | [position] セクション追加 |

### 修正版 Step 3 (Gate実装)

```rust
// Gate 4.5: MaxConcurrentPositions
// (Gate 4: MaxPositionTotal の後、Gate 5: has_position の前)
let current_position_count = self.position_tracker.position_count();
if current_position_count >= self.config.max_concurrent_positions {
    debug!(
        current = current_position_count,
        max = self.config.max_concurrent_positions,
        market = %market,
        "Max concurrent positions reached"
    );
    return ExecutionResult::rejected(RejectReason::MaxConcurrentPositions);
}
```

### 修正版 Gate順序

```
Gate 1:   HardStop          → Rejected(HardStop)
Gate 2:   READY-TRADING     → (app.rsで確認)
Gate 3:   MaxPositionPerMarket → Rejected(MaxPositionPerMarket)
Gate 4:   MaxPositionTotal  → Rejected(MaxPositionTotal)
Gate 4.5: MaxConcurrentPositions → Rejected(MaxConcurrentPositions)  ← NEW
Gate 5:   has_position      → Skipped(AlreadyHasPosition)
Gate 6:   PendingOrder      → Skipped(PendingOrderExists)
Gate 7:   ActionBudget      → Skipped(BudgetExhausted)
```

---

## 結論

| 評価項目 | 判定 |
|----------|------|
| 設計方針 | ✅ 承認 |
| 実装詳細 | ❌ 要修正 |
| 検証計画 | ✅ 承認 |

**アクション:**
1. 上記5点の問題を修正してから実装に移行
2. 特に RejectReason の場所と Gate ロジックの修正は必須

---

## Appendix: 実コード参照

### RejectReason (hip3-core/src/execution.rs:249-264)
```rust
pub enum RejectReason {
    NotReady,
    MaxPositionPerMarket,
    MaxPositionTotal,
    HardStop,
    QueueFull,
    InflightFull,
    MarketDataUnavailable,
}
```

### ExecutorConfig (hip3-executor/src/executor.rs:221-237)
```rust
pub struct ExecutorConfig {
    pub max_notional_per_market: Decimal,  // default: 50
    pub max_notional_total: Decimal,       // default: 100
}
```
