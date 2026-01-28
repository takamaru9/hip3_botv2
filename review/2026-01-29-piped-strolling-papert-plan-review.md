# Position Tracker Sync Fix Plan Review

## Metadata

| Item | Value |
|------|-------|
| Plan File | `~/.claude/plans/piped-strolling-papert.md` |
| Review Date | 2026-01-29 |
| Reviewer | Claude (code-reviewer) |
| Status | **APPROVED with recommendations** |

---

## Executive Summary

| Category | Score | Notes |
|----------|-------|-------|
| 問題理解 | ✅ Excellent | 根本原因を正確に特定 |
| 解決策 | ✅ Good | 多層防御アプローチは適切 |
| 実装可能性 | ✅ Good | 必要なデータはすべて利用可能 |
| エッジケース | ⚠️ Needs Work | 重複検出の設計に改善余地 |
| テスト計画 | ✅ Good | 必要なステップを網羅 |

**総合評価**: 承認（推奨事項あり）

---

## 1. 問題理解の検証

### ✅ 正確に把握

計画で指摘された問題箇所を確認：

```rust
// executor_loop.rs:656-674
OrderResponseStatus::Filled { oid, total_sz, avg_px } => {
    // ORDER状態のみ更新、POSITIONは更新しない ← 確認済み
    self.executor
        .position_tracker()
        .order_update(cloid.clone(), OrderState::Filled, order.size, Some(*oid))
        .await;
}
```

**検証結果**:
- `order_update()` は注文状態のみを更新
- `fill()` が呼ばれていないため、ポジションが更新されない
- `userFills` WebSocketが唯一のポジション更新経路 → 紛失時に乖離発生

---

## 2. 解決策の評価

### Phase 1: Post Response約定時のポジション更新（P0）

**評価**: ✅ 適切

**必要データの確認**:

| データ | ソース | 確認結果 |
|--------|--------|----------|
| `market` | `PendingOrder.market` | ✅ 利用可能 |
| `side` | `PendingOrder.side` | ✅ 利用可能 |
| `price` | `avg_px` (post response) | ✅ 利用可能 |
| `size` | `total_sz` (post response) | ✅ 利用可能 |
| `timestamp_ms` | `chrono::Utc::now()` | ✅ 生成可能 |

**コード検証**:
```rust
// hip3-core/src/execution.rs:23-38
pub struct PendingOrder {
    pub cloid: ClientOrderId,
    pub market: MarketKey,
    pub side: OrderSide,
    pub price: Price,
    pub size: Size,
    pub reduce_only: bool,
    pub created_at: u64,
}
```

すべてのフィールドが存在し、計画の実装は実現可能。

### Phase 2: Fill重複検出（P0）

**評価**: ⚠️ 要改善

**現在の提案**:
```rust
recent_fills: HashSet<(MarketKey, OrderSide, u64)>, // (market, side, timestamp_ms)
```

**問題点**:

1. **タイムスタンプの一意性が保証されない**
   - post responseのfillには外部タイムスタンプがない
   - `chrono::Utc::now()` を使用するため、同一msで複数fillがあり得る

2. **同一market/sideの連続fillで衝突**
   - 高頻度取引で同一msに複数の約定が発生した場合、誤って重複判定される可能性

**推奨**: cloidベースの重複検出

```rust
// 推奨: cloidベースの重複検出
recent_fills: HashSet<ClientOrderId>,

// on_fill() または fill() を cloid 引数付きに拡張
fn on_fill(&mut self, market: MarketKey, side: OrderSide, price: Price,
           size: Size, timestamp_ms: u64, cloid: Option<ClientOrderId>) {
    if let Some(id) = cloid {
        if self.recent_fills.contains(&id) {
            debug!("Skipping duplicate fill for cloid: {}", id);
            return;
        }
        self.recent_fills.insert(id.clone());
    }
    // ... 既存のポジション更新ロジック
}
```

**利点**:
- cloidは一意性が保証されている
- userFillsからもcloidを取得可能
- タイミング依存がない

### Phase 3: 定期Resync（P1）

**評価**: ✅ 適切

- `sync_positions_from_api()` が既に存在し、再利用可能
- 1分間隔は妥当（負荷とのバランス）
- 安全網として機能

**確認済み**: `app.rs:425` に `sync_positions_from_api` が存在

---

## 3. エッジケースの追加検討

| ケース | 計画の対応 | 追加考慮 |
|--------|------------|----------|
| 部分約定 | `total_sz`で対応 | ✅ OK |
| ポジションフリップ | `update_position_static()`対応 | ✅ OK |
| 高頻度での重複fill | タイムスタンプベース | ⚠️ cloidベース推奨 |
| IOC注文の即時約定 | post responseで対応 | ✅ OK |
| **REST API経由の注文** | **未考慮** | ⚠️ 確認必要 |

**新規エッジケース**:
- REST API経由で注文した場合、post responseのフローが異なる可能性
- 現在のコードがREST/WS両方をカバーしているか確認が必要

---

## 4. テスト計画の評価

**評価**: ✅ 十分

| テスト項目 | 方法 | 評価 |
|------------|------|------|
| コンパイル確認 | `cargo fmt && cargo clippy && cargo check` | ✅ 適切 |
| 単体テスト | 提案あり | ⚠️ 具体的なテストコードがない |
| VPSデプロイ | デプロイ・ログ確認コマンド | ✅ 適切 |
| 乖離テスト | 手動シナリオ | ✅ 適切 |

**推奨**: 単体テストの具体化

```rust
#[tokio::test]
async fn test_fill_from_post_response_updates_position() {
    let tracker = setup_position_tracker();

    // 注文を登録
    tracker.register_order(/* ... */).await;

    // post responseのfillをシミュレート
    tracker.fill(market, OrderSide::Buy, Price::new(dec!(50000)),
                 Size::new(dec!(0.1)), now_ms()).await;

    // ポジションが更新されたことを確認
    let pos = tracker.get_position(market).await;
    assert!(pos.is_some());
    assert_eq!(pos.unwrap().size, Size::new(dec!(0.1)));
}

#[tokio::test]
async fn test_duplicate_fill_is_ignored() {
    // 重複検出のテスト
}
```

---

## 5. 推奨事項

### 必須（実装前に対応）

1. **重複検出をcloidベースに変更**
   - `fill()` メソッドに `cloid: Option<ClientOrderId>` 引数を追加
   - `recent_fills: HashSet<ClientOrderId>` に変更
   - userFillsからもcloidを渡すように修正

### 推奨（実装後でも可）

2. **単体テストの追加**
   - post response fillでポジション更新されるテスト
   - 重複fillが無視されるテスト

3. **ログレベルの調整**
   - 重複検出時は`debug`で十分（既に提案通り）
   - 正常な`fill`呼び出しは`info`（既に提案通り）

4. **メトリクスの追加**（optional）
   - `position_tracker_fills_total` カウンター
   - `position_tracker_duplicate_fills_total` カウンター

---

## 6. 結論

| 判定 | 理由 |
|------|------|
| **APPROVED** | 根本原因の特定と解決策は正確。重複検出の改善を推奨するが、blocking issueではない |

### 実装優先順位（推奨）

1. Phase 1: post response fillでのposition更新 → **即実装**
2. Phase 2: fill重複検出（cloidベースに修正） → **Phase 1と同時**
3. Phase 3: 定期resync → **Phase 1-2の効果確認後**

### 最終チェックリスト

- [ ] 重複検出をcloidベースに変更
- [ ] `fill()` メソッドのシグネチャを確認（cloid引数追加が必要か）
- [ ] userFillsハンドラでもcloidを渡すように修正
- [ ] 単体テスト追加
- [ ] デプロイ後のログ確認
