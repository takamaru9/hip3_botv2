# Phase B 実装計画レビュー（3.4 統合・READY-TRADING）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（3.4 Week 2-3: 統合・READY-TRADING）  
確認日: 2026-01-20

## 結論
**未承諾**。3.4 は方向性は良いですが、境界（crate/型）と起動順序・Gateが未確定で、そのまま実装すると設計判断が必要になって詰まります。次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) 循環依存回避の方針が 3.4 と矛盾（`PositionQuery`/trait object 前提が残っている）
冒頭の「循環依存回避」では `Executor` が `Arc<dyn PositionQuery>` を保持する前提になっていますが、3.4 では `position_tracker: PositionTrackerHandle`（actor handle）を直接保持する設計に変わっています。さらに `PositionQuery` trait 自体も本文内に定義がありません。

→ どちらを正とするかを断定し、計画全体（2.x の方針説明/図、3.4 Executor構造体、crate依存）を統一してください。
- `hip3-executor` が `hip3-position` に依存して `PositionTrackerHandle` を使うなら、図/文章から `PositionQuery` 前提を削除して依存関係を更新
- 依存を避けるなら、`PositionQuery` を **定義**し、3.4 の `PositionTrackerHandle` 依存をその trait object に置き換える

### 2) READY-TRADING のゲートが実行フローに接続されていない
`TradingReadyChecker` は定義されていますが、`Executor::on_signal()` / `ExecutorLoop::tick()` 側で READY-TRADING を満たすまで「新規発注を止める」接続がありません。actor 方式だと `isSnapshot` 適用完了も非同期なので、**準備完了前に取引を開始**し得ます。

→ 計画に以下を追加して「いつから発注して良いか」を固定してください。
- `md_ready`/`order_snapshot`/`fills_snapshot`/`position_synced` を **誰が** いつセットするか
- READY-TRADING 未達の間は `on_signal` を reject/queue するのか、`ExecutorLoop` 自体を開始しないのか（どちらかに断定）
- `PositionTrackerTask` から「snapshot適用完了」を外部へ通知する仕組み（watch/oneshot/状態問い合わせ など）

### 3) Risk Gate「PendingOrder（同一市場に未約定注文あり→新規禁止）」が 3.4 に反映されていない + 登録が race になる
4.1 で `PendingOrder` Gate を定義していますが、3.4 の `on_signal()` は `has_position()` しか見ておらず、同一市場の未約定注文を禁止できません。また `register_order(tracked)` は `tokio::spawn` で fire-and-forget のため、**enqueue より後に登録されるレース**が起き得ます（将来 pending gate を入れても抜け穴になる）。

→ 計画に「同一市場の未約定注文を O(1) で判定する方法」と「登録順序」を固定してください（例）:
- `PositionTrackerHandle` に `has_pending_order(market)` 用の同期キャッシュ（`pending_markets_cache`）を追加し、`enqueue` 前に **同期的に** 更新する
- あるいは `Executor` 側で `pending_markets: DashMap<MarketKey, bool>` を持ち、enqueue/register を同一スレッドで原子的に更新する（actor への登録は補助）

---

## 再レビュー（2026-01-20）
前回の3点（循環依存方針の統一、READY-TRADING の実行フロー接続、PendingOrder Gate の追加）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `register_order()` の順序が不正で、enqueue失敗時に `pending_orders` がリークし得る
`on_signal()` / `submit_reduce_only()` が `tokio::spawn` で `register_order(tracked)` を投げた後に enqueue しており、enqueue が `QueueFull/InflightFull` で失敗しても **actor側に TrackedOrder が残る**可能性があります（pending_markets_cache は rollback しても、`pending_orders` は残る）。

→ どちらかに断定して整合を取ってください。
- `mpsc::Sender::try_send()` を使って **enqueue 成功後**に同期的に `RegisterOrder` を送る（spawn をやめる）
- もしくは enqueue 失敗時に `UnregisterOrder(cloid)` 等のメッセージで actor 側も rollback する

### 2) PendingOrder Gate の「解除」が未設計（filled/canceled/rejected/送信失敗で解除されない）
`pending_markets_cache` は mark/unmark（enqueue失敗時）までは入っていますが、**注文が完了したとき**（filled/canceled/rejected）に減らす流れが本文/疑似コードにありません。また `ExecutorLoop::handle_send_failure()` は new_order を落とす方針なので、送信失敗/タイムアウト/Rejected の場合に pending が解除されないと、その市場が永続的にブロックされ得ます。

→ 計画に以下を固定してください。
- `PositionTrackerTask` が orderUpdates の terminal 状態（filled/canceled/rejected）で `decrement_pending_market(market)` を必ず呼ぶ（Task 側が `pending_markets_cache` を保持する/またはメッセージで解除する設計にする）
- new_order を「再キューしない」方針なら、`handle_send_failure()` 側でも **pending解除 + pending_orders解除** を行う（同一 cloid の二重発注を避けるなら、その前提と手順も明記）

### 3) READY-TRADING の待機APIが実装しづらい（`wait_ready(&mut self)` と `Arc<TradingReadyChecker>` の整合）
`TradingReadyChecker` を `Arc` で共有する設計なのに、待機APIが `wait_ready(&mut self)` になっています。複数タスクが同時にフラグを更新する前提だと、`&mut self` を取れる呼び出し点が作りづらく、実装時に迷います。

→ `ExecutorLoop`（または起動オーケストレータ）側は `ready_checker.subscribe()` で `watch::Receiver<bool>` を取得し、`mut rx` を待つ形に寄せてください（`wait_ready` は削除するか、`Receiver` を引数に取る形に変更）。

---

## 再レビュー（2026-01-20, 修正後）
前回の3点（`try_register_order` で enqueue 後に登録、PendingOrder解除フローの追加、`wait_ready` 削除→`subscribe()` 待機）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `try_register_order()` 失敗時に、追跡が欠落して PendingOrder Gate が永久化し得る
`try_register_order()` が `TrySendError` で失敗すると、actor 側に `TrackedOrder` が登録されません。この状態で orderUpdates が来ても `pending_orders.get_mut(&cloid)` にヒットせず、terminal で `decrement_pending_market()` も走らないため、`pending_markets_cache` が **解除されず市場が永久ブロック**し得ます。

→ **登録は必ず届ける**方針にしてください（例）:
- `try_register_order()` が失敗したら、フォールバックで `tokio::spawn(async move { handle.register_order(tracked).await; })`（enqueue 後なのでリークしない）
- もしくは `RegisterOrder` 経路だけは unbounded channel 等で「満杯で落ちない」前提にする（容量/安全性の判断も明記）

### 2) PendingOrder Gate が厳密に守れない（check→mark が非原子的で、同一市場に二重enqueueし得る）
`on_signal()` は `has_pending_order()`（read）→ `mark_pending_market()`（write）になっており、並行に `on_signal()` が走ると両方が gate をすり抜けて **同一市場に複数の new_order が enqueue** され得ます。

→ 次のどちらかに断定してください。
- `try_mark_pending_market(market) -> bool`（存在しなければ insert、存在すれば false）で **原子的に** gate+mark を行い、`has_pending_order()` の事前チェックをやめる  
- もしくは「on_signal は単一スレッドで直列実行」の前提を明記し、その前提が崩れた場合の保護（Mutex/actor化等）も書く

※ 併せて、3.3 の `pending_orders` ライフサイクル節に残っている `register_order() -> enqueue` の例も、最終仕様（mark→enqueue→register）に更新してください（本文の整合性）。

### 3) `PostResult::Rejected`（post応答エラー）で pending 解除が走らずブロックし得る
`ExecutorLoop::on_ws_message()` は `PostResult::Rejected` で「再キューしない」ログのみになっており、`pending_markets_cache`/actor の `pending_orders` を解除しません。postレベルで拒否された場合、orderUpdates が来ない可能性があるため、PendingOrder Gate が解除されず市場がブロックし得ます。

→ `Rejected` を **terminal** として扱い、少なくとも以下の cleanup を入れてください。
- new_order: `unmark_pending_market(market)` + `RemoveOrder(cloid)`（actor側も解除）
- reduce_only: 上と同様の解除 + アラート（Flatten必達のため）

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（`try_register_order` のフォールバック、`try_mark_pending_market` の原子化、`PostResult::Rejected` の cleanup、ライフサイクル例の更新）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `ExecutionResult::Failed` が未定義 + 「上流通知」の経路が設計されていない
3.4 の「送信失敗時の再キュー方針」や `handle_send_failure()` のコメントで `ExecutionResult::Failed` が前提になっていますが、`ExecutionResult` enum に該当バリアントがありません。また、new_order の送信失敗は enqueue 後に起きるため、`on_signal()` の戻り値として「上流へ返す」のは不可能です（既に `Queued` を返している）。

→ 計画をどちらかに断定して整合を取ってください。
- **A. 非同期イベント通知にする**: `ExecutionEvent/ExecutionFailure` などの channel を定義し、`ExecutorLoop` が send failure / timeout / disconnect / rejected を publish（戦略側はアラート・再シグナル等で処理）
- **B. 「上流通知」をやめる**: 失敗時はログ/メトリクス/アラートのみ、と明記し、`ExecutionResult::Failed` の記述を削除

### 2) `PostRequestManager::check_timeouts()` の疑似コードが Rust 的に成立しない（`oneshot::Sender` を move できない）
`DashMap::retain()` のクロージャ内で `req.tx.send(...)` していますが、`oneshot::Sender::send(self, T)` は Sender を消費するため、`&mut PendingRequest` から move できず実装で詰まります。

→ どちらかに修正してください。
- `tx: Option<oneshot::Sender<PostResult>>` にして `if let Some(tx) = req.tx.take() { let _ = tx.send(...); }` の形にする
- もしくは retain で send せず、タイムアウト対象の `post_id` を収集 → `remove()` で所有権を取ってから send する

### 3) `mark_as_sent()` / `on_batch_sent()` と `on_response()` の並行実行前提が未確定で、inflight がドリフトし得る
現状は `sent: bool` を `mark_as_sent()` でセットし、`on_response()` が `sent` を見て inflight decrement の要否を決める設計です。しかし `on_ws_message()` が別タスクから並行に呼ばれる場合、**レスポンスが `mark_as_sent()` より先に処理される**などで inflight がズレるレースが起き得ます。

→ 計画に「並行性の前提」を固定してください（どちらかに断定）。
- **A. 直列実行を保証**: `ExecutorLoop` 1タスクが `select!` 等で tick と ws メッセージ処理を直列化し、`PostRequestManager` は並行アクセスされない前提にする
- **B. 並行でも正しい状態機械**: `sent/completed` を原子的に扱う（例: enum state + CAS）など、`on_response` と `on_send_success` の順序が入れ替わっても inflight が一致するようにする

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（「上流通知」記述の削除、`PendingRequest.tx: Option<oneshot::Sender>` 化、`ExecutorLoop` の直列実行前提の明記）は反映されています。  
**3.4 統合・READY-TRADING は承諾**します（この内容で実装に進めます）。
