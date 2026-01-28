# Phase B 実装計画レビュー（3.3 hip3-position）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（3.3 Week 2: hip3-position）  
確認日: 2026-01-20

## 結論
**未承諾**。3.3 は現状だと型/依存関係が破綻していて、そのまま実装に入ると詰まります。次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `PendingOrder` を `hip3-position` が保持すると crate 依存が循環し得る
3.3 の `PositionTracker.pending_orders: DashMap<ClientOrderId, PendingOrder>` は、`PendingOrder` が 3.2 の OrderBuilder/Executor 側（= `hip3-executor`）に置かれる前提だと、`hip3-position -> hip3-executor` 依存が必要になります。一方 3.4 では `Executor` が `PositionTracker` を保持するため、`hip3-executor -> hip3-position` 依存も発生し、**循環依存**でビルドできません。

→ 方針をどちらかに固定してください（おすすめ順）:
- **(推奨)** `PendingOrder`（および `PendingCancel`/`ActionBatch` など共有型）を `hip3-core`（または新規の共有crate）へ移し、`hip3-executor` と `hip3-position` が両方参照する
- `hip3-position` 側は `PendingOrder` を持たず、`ClientOrderId -> (market, side, size, reduce_only, ...)` の **最小トラッキング用struct** を独自定義する

### 2) `Position` / Flatten が型不整合で、このままだとコンパイルしない
`Position` は `size: Decimal` / `entry_price: Decimal` ですが、3.2 の `OrderBuilder::build_ioc()` は `Size`/`Price` を受けます。また Flatten は `position.spec.clone()` を参照していますが、`Position` に `spec` がありません。

→ 3.3 の `Position` を次のどちらかに統一してください。
- `Position { size: Size, entry_price: Price, ... }` にして、Flatten/TimeStop/Executor で一貫して `Price/Size` を使う  
- `Position` は `Decimal` のままにするなら、Flatten で `Decimal -> Price/Size` 変換方針と `MarketSpec` の取得元（`Executor.market_specs` 参照など）を明記し、`position.spec` 参照を消す

### 3) `orderUpdates`/`userFills` の“取り込み順序・欠損・再起動”時の整合ルールが未定義
`pending_orders` を使う前提なら、少なくとも以下が計画に必要です（ここが曖昧だと READY-TRADING 判定とポジションが壊れます）。

- `pending_orders` を **いつ登録/削除**するか（例: enqueue時に登録、orderUpdatesで状態更新、filled/canceledで削除）
- 再起動時に `pending_orders` が空でも `userFills` が流れてくるケースの扱い（snapshot受領後に復元する/不足分は clearinghouseState で補う等）
- `isSnapshot` 受領前後での処理方針（snapshot前の增分を捨てるのか、バッファするのか）

---

## 再レビュー（2026-01-20）
前回の3点（循環依存回避の方針、`Position` の `Price/Size` 統一、`isSnapshot`/再起動の整合方針の追記）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `pending_orders` に何を保持するかが矛盾している（`PendingOrder` vs `TrackedOrder`）
本文では `pending_orders: DashMap<ClientOrderId, PendingOrder>` としつつ、再起動復元では「`PendingOrder` の完全な情報がないため最小トラッキングにする」と書いており、さらに `TrackedOrder` を定義しています。加えてライフサイクル表は「orderUpdates: open で exchange_oid を記録」とありますが、`PendingOrder` に `exchange_oid/status/filled_size` などの格納先がありません。

→ 計画としてどれが正かを **断定して一本化**してください（例）:
- `pending_orders: DashMap<ClientOrderId, TrackedOrder>` に統一し、enqueue時は `PendingOrder -> TrackedOrder` に落として登録する
- もしくは `PendingOrder` に `exchange_oid/status/filled_size` を追加し、再起動時に復元できる最小情報を定義して矛盾を解消する

### 2) `isSnapshot` バッファリングの実装モデル（スレッド安全/排他）が未確定
`PositionTracker` に `Vec<OrderUpdate>` / `Vec<UserFill>` を持たせて `&mut self` で処理する例が出ていますが、実際は WS 受信タスクから並行に呼ばれる想定です（DashMapも並行利用前提のはず）。このままだと実装時に `Mutex`/actor 化などの設計判断が必要になり、計画が“そのまま写せない”状態です。

→ `PositionTracker` を以下のどちらかに固定してください。
- **actor方式**: `PositionTrackerTask` を立てて mpsc で orderUpdates/userFills を順序付きで投入（内部は `Vec` を素直に使える）
- **ロック方式**: `order_buffer/fills_buffer/snapshot flags` を `Mutex` 等で保護し、外部APIは `&self` で呼べる形にする

### 3) `entry_time: Instant` と TimeStop の再起動整合が未定義
TimeStop は「ポジションの経過時間」が核心ですが、`Instant` は再起動で復元できません。再起動直後にタイマーがリセットされると、TimeStop が効かずに想定より長くポジションが残るリスクがあります。

→ `Position.entry_time` を `timestamp_ms`（fills由来）など復元可能な表現にするか、少なくとも「再起動時は entry_time を now 扱いにして TimeStop をリセットする」等の方針を断定してください（安全側は fills timestamp から復元）。

---

## 再レビュー（2026-01-20, 修正後）
前回の3点（`pending_orders` の `TrackedOrder` 統一、actor 方式の採用、`entry_timestamp_ms` への変更）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `PendingOrder` と `TrackedOrder::from_pending()` のフィールドが不整合（`order.side` が存在しない）
`TrackedOrder::from_pending()` が `side: order.side` を参照していますが、3.2 の `PendingOrder` は `is_buy: bool` を持つ設計のままです。このままだと計画の疑似コードがコンパイルしません。

→ `PendingOrder` の最終形を断定して、全セクションで統一してください。
- 例: `PendingOrder { side: OrderSide, ... }` に変更し、wire変換時に `is_buy = matches!(side, Buy)` を導出する
- もしくは `from_pending()` 側で `is_buy -> OrderSide` 変換を明記する

### 2) `register_order(tracked)` のAPIが未定義（Msg/Handle/Task のどこにも存在しない）
ライフサイクル表と pseudo code は `position_tracker_handle.register_order(tracked).await;` を前提にしていますが、`PositionTrackerMsg` に登録用のバリアントが無く、`PositionTrackerHandle` にも `register_order()` がありません。

→ `TrackedOrder` を登録するための API を計画に固定してください（例: `PositionTrackerMsg::RegisterOrder(TrackedOrder)` を追加し、Handle に `register_order(tracked)` を実装）。

### 3) 3.4 以降が旧 `PositionTracker` 前提のまま（actor handle との整合が取れていない）
3.3 で actor 方式（`PositionTrackerTask`/`PositionTrackerHandle`）にしたので、3.4 の `Executor { position_tracker: Arc<PositionTracker> }` や `has_position()` 前提の同期呼び出しはそのままだと成立しません（Handle の `get_position()` は async）。

→ 3.4 の型/呼び出しを actor 方式に合わせて更新してください。
- `Executor` が保持するのは `PositionTrackerHandle`（または `Arc<...>`）にする
- 同期APIが必要なら、Handle 内に `positions_cache: DashMap<...>` を持たせて `has_position()` を `&self` で読める等、実装方針を断定する

---

## 再レビュー（2026-01-20, 修正後）
前回の3点（`PendingOrder.side` 統一、`register_order()` API 追加、3.4 の actor handle 整合）は反映されています。  
**3.3 hip3-position は承諾**します（この内容で実装に進めます）。
