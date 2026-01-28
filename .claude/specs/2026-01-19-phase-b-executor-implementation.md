# Phase B Executor Implementation Spec

## Metadata

| Item | Value |
|------|-------|
| Plan Date | 2026-01-19 |
| Last Updated | 2026-01-22 |
| Status | `[COMPLETED]` |
| Source Plan | `.claude/plans/2026-01-19-phase-b-executor-implementation.md` |

---

## Implementation Status Summary

| Phase | Section | Status | Progress |
|-------|---------|--------|----------|
| 3.1 | NonceManager + BatchScheduler | [x] DONE | 100% |
| 3.2 | Signer (KeyManager) | [x] DONE | 100% |
| 3.3 | hip3-position (PositionTracker) | [x] DONE | 100% |
| 3.4 | Integration (Executor, ExecutorLoop, READY-TRADING) | [x] DONE | 100% |
| 3.4.1 | WsSender統合 (hip3-ws拡張) | [x] DONE | 100% |
| 3.5 | Testnet検証 | [ ] TODO | 0% |
| 3.6 | Mainnet超小口テスト | [ ] TODO | 0% |
| 4 | リスク管理 (HardStop, MaxPosition) | [x] DONE | 100% |

**コード実装完了率: 100%**
**運用検証: Testnet/Mainnet テスト待ち**

---

## Task Breakdown

### Phase 1: 共有型追加 (hip3-core)

| ID | Item | Status | Notes |
|----|------|--------|-------|
| C-1 | PendingOrder 構造体 | [x] DONE | `crates/hip3-core/src/execution.rs` |
| C-2 | PendingCancel 構造体 | [x] DONE | `crates/hip3-core/src/execution.rs` |
| C-3 | TrackedOrder 構造体 | [x] DONE | `crates/hip3-core/src/execution.rs` |
| C-4 | ActionBatch enum | [x] DONE | Orders/Cancels 二種、SDK準拠 |
| C-5 | OrderWire, CancelWire | [x] DONE | `crates/hip3-executor/src/signer.rs` |
| C-6 | ExecutionResult/RejectReason enum | [x] DONE | QueuedDegraded variant 追加済み |

### Phase 2: hip3-executor 基盤

#### 3.1 NonceManager

| ID | Item | Status | Notes |
|----|------|--------|-------|
| N-1 | Clock trait | [x] DONE | Send + Sync bound |
| N-2 | SystemClock | [x] DONE | 本番用実装 |
| N-3 | NonceManager 構造体 | [x] DONE | AtomicU64 counter |
| N-4 | approx_server_time_ms() | [x] DONE | offset 反映 |
| N-5 | next() - CAS ループ | [x] DONE | max(last+1, approx_server) |
| N-6 | sync_with_server() | [x] DONE | drift 検知 (2s warn, 5s err) |
| N-7 | ユニットテスト 7項目 | [x] DONE | 全テストパス |

#### 3.1 BatchScheduler

| ID | Item | Status | Notes |
|----|------|--------|-------|
| B-1 | InflightTracker | [x] DONE | AtomicU32, CAS ループ |
| B-2 | BatchConfig | [x] DONE | 設定構造体 |
| B-3 | BatchScheduler 構造体 | [x] DONE | 3キュー構造 |
| B-4 | enqueue_new_order() | [x] DONE | inflight チェック含む |
| B-5 | enqueue_reduce_only() | [x] DONE | 優先キュー |
| B-6 | enqueue_cancel() | [x] DONE | 最優先 |
| B-7 | tick() | [x] DONE | ActionBatch 返却 |
| B-8 | on_batch_sent/complete | [x] DONE | inflight 会計 |
| B-9 | drop_new_orders() | [x] DONE | HardStop 用 |
| B-10 | ユニットテスト 12項目 | [x] DONE | 15テストパス |

#### 3.2 Signer

| ID | Item | Status | Notes |
|----|------|--------|-------|
| S-1 | KeySource enum | [x] DONE | EnvVar/File/Memory |
| S-2 | KeyManager | [x] DONE | PrivateKeySigner 保持 |
| S-3 | from_bytes() (test用) | [x] DONE | バイト直接ロード |
| S-4 | Action 構造体 | [x] DONE | msgpack 対応 |
| S-5 | OrderWire, CancelWire | [x] DONE | serde rename 対応 |
| S-6 | OrderTypeWire | [x] DONE | ioc()/gtc()/alo() |
| S-7 | SigningInput | [x] DONE | action_hash 計算 |
| S-8 | Signer.sign_action() | [x] DONE | EIP-712 署名 |
| S-9 | ユニットテスト | [x] DONE | SDK 互換性検証済 |

### Phase 3: hip3-position

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P-1 | Position 構造体 | [x] DONE | entry_timestamp_ms, is_long(), is_short() |
| P-2 | PositionTrackerMsg enum | [x] DONE | actor メッセージ |
| P-3 | PositionTrackerTask | [x] DONE | mpsc + 状態管理ループ |
| P-4 | PositionTrackerHandle | [x] DONE | API インターフェース |
| P-5 | positions_cache (DashMap) | [x] DONE | 高頻度チェック用 |
| P-6 | pending_markets_cache | [x] DONE | PendingOrder Gate 用 |
| P-7 | try_mark_pending_market | [x] DONE | 原子的 mark（DashMap entry API） |
| P-8 | orderUpdates 処理 | [x] DONE | 状態遷移 |
| P-9 | userFills 処理 | [x] DONE | Position 更新 |
| P-10 | isSnapshot 処理 | [x] DONE | バッファリング |
| P-11 | TimeStop 構造体 | [x] DONE | check() メソッド |
| P-12 | Flattener | [x] DONE | FlattenOrderBuilder として実装 |

### Phase 4: 統合 (3.4)

| ID | Item | Status | Notes |
|----|------|--------|-------|
| I-1 | TradingReadyChecker | [x] DONE | 4フラグ + watch channel |
| I-2 | PostIdGenerator | [x] DONE | 連番生成 |
| I-3 | PostRequestManager | [x] DONE | inflight 追跡 (sent flag) |
| I-4 | Executor 構造体 | [x] DONE | 依存フィールド統合 |
| I-5 | on_signal() | [x] DONE | Gate 順序適用（7段階） |
| I-6 | submit_reduce_only() | [x] DONE | 優先キュー経由 |
| I-7 | ExecutorLoop | [x] DONE | tick + HardStop guard |
| I-8 | on_hard_stop() | [x] DONE | cleanup 処理 |

### Phase 4.1: WsSender統合 (3.4.1)

| ID | Item | Status | Notes |
|----|------|--------|-------|
| W-1 | PostRequest/PostResponse (message.rs) | [x] DONE | serde rename対応、SDK互換JSON |
| W-2 | WsWriteHandle (hip3-ws) | [x] DONE | fire-and-forget送信、rate limit + READY-TRADING check |
| W-3 | WsOutbound enum | [x] DONE | Text/Post二種、channel-based送信 |
| W-4 | ConnectionManager outbound channel | [x] DONE | tokio::select! でincoming/outgoing両立 |
| W-5 | inflight tracking (transport層) | [x] DONE | record_post_send/response、reset_inflight on disconnect |
| W-6 | RealWsSender (hip3-executor) | [x] DONE | WsSender trait実装、SignedAction→PostRequest変換 |
| W-7 | OrderUpdatePayload/FillPayload | [x] DONE | message.rsに追加、WsMessage helper methods |
| W-8 | orderUpdates/userFills購読 | [x] DONE | SubscriptionManager helper methods |
| W-9 | PongMessage deny_unknown_fields | [x] DONE | untagged enum正常動作のため |
| W-10 | Unit tests (hip3-ws) | [x] DONE | 39テスト、serde検証含む |
| W-11 | Unit tests (hip3-executor) | [x] DONE | 86テスト、RealWsSender含む |

### Phase 5: リスク管理 (4)

| ID | Item | Status | Notes |
|----|------|--------|-------|
| R-1 | HardStopLatch | [x] DONE | AtomicBool latch + reason保存 |
| R-2 | ExecutionEvent enum | [x] DONE | RiskMonitor 入力 |
| R-3 | RiskMonitor actor | [x] DONE | トリガー判定 |
| R-4 | MaxPosition Gate (per-market) | [x] DONE | $50 上限 |
| R-5 | MaxPosition Gate (total) | [x] DONE | $100 上限 |

---

## Deviations from Plan

| ID | Original | Actual | Reason |
|----|----------|--------|--------|
| D-1 | SkipReason::HasPosition | SkipReason::AlreadyHasPosition | 明確性向上のため命名変更 |
| D-2 | SkipReason::PendingOrder | SkipReason::PendingOrderExists | 明確性向上のため命名変更 |
| D-3 | Flattener struct | FlattenOrderBuilder struct | 責務を明確化（注文生成に特化） |
| D-4 | HardStopLatch in batch.rs (仮) | HardStopLatch in hip3-risk | 適切なcrateに配置 |

---

## Key Implementation Details

### Gate チェック順序（厳守）

```
1. HardStop        → Rejected(HardStop)
2. READY-TRADING   → Rejected(NotReady)
3. MaxPositionPerMarket → Rejected(MaxPositionPerMarket)
4. MaxPositionTotal → Rejected(MaxPositionTotal)
5. has_position    → Skipped(AlreadyHasPosition)
6. PendingOrder    → Skipped(PendingOrderExists)
7. ActionBudget    → Skipped(BudgetExhausted)
8. (all passed)    → try_mark_pending_market + enqueue
```

### スレッドセーフ設計

- NonceManager: CAS loop (compare_exchange)
- TradingReadyChecker: AtomicBool + watch channel
- InflightTracker: AtomicU32 + CAS loop
- PositionTrackerHandle: DashMap + atomic caches
- HardStopLatch: AtomicBool + Mutex for reason

### テスト結果

| Crate | テスト数 | 結果 |
|-------|---------|------|
| hip3-core | 10+ | ALL PASS |
| hip3-executor | 86 | ALL PASS |
| hip3-ws | 39 | ALL PASS |
| hip3-position | 20+ | ALL PASS |
| hip3-risk | 31 | ALL PASS |

---

## Review Checkpoints

| Checkpoint | Items | Reviewer | Status |
|------------|-------|----------|--------|
| Phase 1 完了 | C-1〜C-6 | 自己レビュー | [x] DONE |
| Phase 2 完了 | N-*, B-*, S-* | 自己レビュー | [x] DONE |
| Phase 3 完了 | P-* | 自己レビュー | [x] DONE |
| Phase 4 完了 | I-* | 自己レビュー | [x] DONE |
| Phase 5 完了 | R-* | 自己レビュー | [x] DONE |
| 総合レビュー | 全体 | 最終確認 | [x] DONE |

---

## 総合評価

### Production Deployment Readiness: **GO** (Testnet/Mainnet検証後)

#### 強み

1. **設計との完全な整合性**: 計画書の全構造体、API、ゲートチェック順序が正確に実装
2. **堅牢なリスク管理**: HardStopLatch + RiskMonitor による多層的な閾値監視
3. **スレッドセーフ設計**: 全コンポーネントが atomic 操作またはactor patternで実装
4. **テスト可能性**: Clock trait、actor pattern による高い分離性

#### 推奨アクション

1. **Testnet 検証の完了**: Plan Section 4.6 の検証項目 #1-#10 を全て実行
2. **Mainnet Micro Test**: $1 notional での動作確認
3. **監視体制の確認**: HardStop 発動時のアラート通知が機能すること

---

## 実装ファイル一覧

| Crate | File | 主要コンポーネント |
|-------|------|-------------------|
| hip3-core | src/execution.rs | PendingOrder, TrackedOrder, ActionBatch, ExecutionResult |
| hip3-executor | src/nonce.rs | Clock, NonceManager |
| hip3-executor | src/batch.rs | InflightTracker, BatchScheduler |
| hip3-executor | src/signer.rs | KeyManager, Signer, Action, OrderWire |
| hip3-executor | src/executor.rs | Executor, ExecutorLoop, PostIdGenerator |
| hip3-executor | src/ready.rs | TradingReadyChecker |
| hip3-executor | src/real_ws_sender.rs | RealWsSender (WsSender trait実装) |
| hip3-position | src/tracker.rs | Position, PositionTrackerTask, PositionTrackerHandle |
| hip3-position | src/time_stop.rs | TimeStop, FlattenOrderBuilder, TimeStopMonitor |
| hip3-risk | src/hard_stop.rs | HardStopLatch, RiskMonitor, ExecutionEvent |
| hip3-risk | src/gates.rs | MaxPositionPerMarketGate, MaxPositionTotalGate |
| hip3-ws | src/message.rs | PostRequest, PostResponse, OrderUpdatePayload, FillPayload |
| hip3-ws | src/ws_write_handle.rs | WsWriteHandle, WsOutbound, PostError |
| hip3-ws | src/connection.rs | ConnectionManager (outbound channel追加) |
| hip3-ws | src/subscription.rs | orderUpdates/userFills購読helpers |
