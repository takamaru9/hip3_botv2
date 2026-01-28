# Auto Exit Integration Plan レビュー

対象: `.claude/plans/2026-01-25-auto-exit-integration.md`

## Findings

- [HIGH] PriceProviderAdapter の設計が現行 API と依存関係に合っていません。計画では `hip3-position` に `MarketStateCache` を使う Adapter を置き、`get_bbo()`/`get_mid_price()`/`get_best_bid()`/`get_best_ask()` を使う前提ですが、`MarketStateCache` は `hip3-executor` にあり `get_mark_px()` しかありません。また `PriceProvider` trait は `get_price()` のみです。現状のままだと API 不一致 + 依存関係の逆転（hip3-position → hip3-executor）でコンパイルできません。`.claude/plans/2026-01-25-auto-exit-integration.md:109` `.claude/plans/2026-01-25-auto-exit-integration.md:123` `.claude/plans/2026-01-25-auto-exit-integration.md:125`
- [HIGH] RiskMonitor の型選定が混在しており、イベント送信経路が成立しません。計画は `event_tx/rx` で非同期パイプを作る一方、`risk_monitor.on_event(...)` の同期 API を使う想定です。また `ExecutionEvent::OrderRejected/OrderFilled` と `max_loss_usd`/`max_consecutive_failures` は `hip3-risk` 側の定義ですが、アプリは `hip3-executor::HardStopLatch` を使っており `hip3-executor::RiskMonitor` は `event_rx` 受信+異なる `ExecutionEvent`/config です。どちらに寄せるか決めて型・イベント・起動方法を統一しないと実装できません。`.claude/plans/2026-01-25-auto-exit-integration.md:92` `.claude/plans/2026-01-25-auto-exit-integration.md:241` `.claude/plans/2026-01-25-auto-exit-integration.md:288`
- [HIGH] `handle_order_update()` の実装案が現行コードと整合しません。計画では `OrderStatus::Rejected` を使いますが、`orderUpdates` の status は文字列で `OrderStatus` enum は存在しません。`&mut self` も現状は `&self` です。これではコンパイルできないため、実際の `OrderUpdatePayload.status` 文字列（`"rejected"` / `*Rejected` / `scheduledCancel` など）に合わせた分岐に更新する必要があります。`.claude/plans/2026-01-25-auto-exit-integration.md:243` `.claude/plans/2026-01-25-auto-exit-integration.md:249`
- [HIGH] HardStop flatten 実装案が現行 API に合っていません。`FlattenOrderBuilder::build_reduce_only` は存在せず、`create_flatten_order(position, price, slippage_bps, now_ms)` が正しいシグネチャです。`market_state_cache.get_mid_price()` も存在せず、`get_mark_px()` などの実在 API を使う必要があります。さらに `ExecutorLoop` に `position_tracker`/`batch_scheduler` フィールドは無いので `self.executor.position_tracker()`/`self.executor.batch_scheduler()` 経由でアクセスする設計に直さないと実装できません。`.claude/plans/2026-01-25-auto-exit-integration.md:319` `.claude/plans/2026-01-25-auto-exit-integration.md:335` `.claude/plans/2026-01-25-auto-exit-integration.md:342`
- [MEDIUM] `config/default.toml` に `time_stop`/`risk_monitor` を追加する一方で、`AppConfig`/`TimeStopConfig` に対応フィールドを追加する工程が抜けています。現状の Serde は未知フィールドを無視するため、運用者は設定が反映されたと思っても実際には使われません。設定構造体・読み込み・`Application::run()` の wiring まで計画に明記すべきです。`.claude/plans/2026-01-25-auto-exit-integration.md:432` `.claude/plans/2026-01-25-auto-exit-integration.md:436`
- [MEDIUM] HardStop flatten がワンショットで、価格未取得や約定失敗時のリトライがありません。監視ループは 1 回 flatten して break するため、BBO/mark が欠けた市場や失敗した reduce-only が残ったままになります。最低限、Flattener の失敗管理（Phase 5）か再試行条件を Phase 4 に組み込む必要があります。`.claude/plans/2026-01-25-auto-exit-integration.md:335` `.claude/plans/2026-01-25-auto-exit-integration.md:365`
- [LOW] BatchScheduler の reduce-only キューは既に実装済みのため、Phase 2 の「reduce-only キュー追加」は重複作業になり得ます。計画は「flatten_rx から enqueue_reduce_only へ流す」部分にフォーカスした方が安全です。`.claude/plans/2026-01-25-auto-exit-integration.md:198`

## Questions / Assumptions

- RiskMonitor は `hip3-executor` に寄せる前提ですか、それとも `hip3-risk` の `HardStopLatch`/`RiskMonitor` に統一しますか？どちらに寄せるかでイベント型・起動方法が大きく変わります。
- TimeStop/HardStop の価格ソースは `MarketStateCache::mark_px` を使う方針ですか、それとも `hip3-feed::MarketState` の BBO を使いますか？（依存方向と価格品質に影響）

## Change Summary

- 方向性は妥当ですが、主要コンポーネントの API/依存関係の不一致が多く、このままでは実装不能です。RiskMonitor と PriceProvider の“どの型を使うか”を確定させ、既存 API に合わせた wiring に修正する必要があります。
