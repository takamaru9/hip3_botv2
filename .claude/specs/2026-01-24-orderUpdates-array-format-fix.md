# orderUpdates 配列形式対応 Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-24 |
| Last Updated | 2026-01-24 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-24-orderUpdates-array-format-fix.md` |
| Related Bug | BUG-002 |

---

## Implementation Status Summary

| Phase | Item | Status | Progress |
|-------|------|--------|----------|
| P0 | 配列形式対応 + フィールド互換 | [x] DONE | 100% |
| P1 | テスト追加 | [x] DONE | 100% |
| P2 | 既存as_order_update() deprecate | [x] DONE | 100% |

**コード実装完了率: 100%**

---

## Task Breakdown

### P0 [High]: 配列形式対応 + フィールド互換

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P0-1 | `OrderInfo` にフィールド互換対応 (`limitPx` alias) | [x] DONE | message.rs |
| P0-2 | `OrderUpdatesResult` struct追加 | [x] DONE | message.rs:206 |
| P0-3 | `as_order_updates()` メソッド追加 | [x] DONE | message.rs:359 |
| P0-4 | lib.rs re-export | [x] DONE | lib.rs:23 |
| P0-5 | app.rs 呼び出し側修正 | [x] DONE | エラー可視性維持 |

### P1 [Medium]: テスト追加

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P1-1 | 配列形式パーステスト（公式スキーマ） | [x] DONE | limitPx/timestamp版 |
| P1-2 | 配列形式パーステスト（後方互換） | [x] DONE | px版 |
| P1-3 | 空配列テスト | [x] DONE | |
| P1-4 | 単一オブジェクトテスト（後方互換） | [x] DONE | |
| P1-5 | 一部パース失敗テスト | [x] DONE | failed_count検証 |
| P1-6 | 単一オブジェクトパース失敗テスト | [x] DONE | |

### P2 [Low]: 既存as_order_update() deprecate

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P2-1 | `#[deprecated]` attribute追加 | [x] DONE | message.rs:341 |
| P2-2 | 内部実装を`as_order_updates()`委譲に変更 | [x] DONE | message.rs:344 |

---

## Deviations from Plan

（計画通りに実装、逸脱なし）

---

## Key Implementation Details

### 実装されたコード

| ファイル | 行 | 内容 |
|----------|-----|------|
| `message.rs` | 206-213 | `OrderUpdatesResult` struct |
| `message.rs` | 359-418 | `as_order_updates()` - 配列/単一オブジェクト両対応 |
| `message.rs` | 337-344 | `as_order_update()` deprecated + 委譲実装 |
| `lib.rs` | 23 | `OrderUpdatesResult` re-export |

### OrderUpdatesResult 構造

```rust
pub struct OrderUpdatesResult {
    /// Successfully parsed order updates.
    pub updates: Vec<OrderUpdatePayload>,
    /// Number of elements that failed to parse.
    pub failed_count: usize,
}
```

### 対応フォーマット

1. **配列形式（公式）**: `{"channel": "orderUpdates", "data": [...]}`
2. **単一オブジェクト形式（後方互換）**: `{"channel": "orderUpdates", "data": {...}}`
3. **空配列**: 正常扱い（failed_count=0）

---

## Verification Checklist

- [x] 全ユニットテスト pass
- [x] 統合テスト pass
- [ ] Testnet接続テスト（BUG-003/004修正後に実施）
  - [ ] 配列形式でorderUpdatesが届くことを確認
  - [ ] 全要素が正常にパースされることを確認

---

## Implementation Completed

**実装完了日**: 2026-01-24
