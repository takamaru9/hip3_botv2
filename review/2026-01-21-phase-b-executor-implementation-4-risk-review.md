# Phase B 実装計画レビュー（4. リスク管理）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（4. リスク管理）  
確認日: 2026-01-21

## 結論
**未承諾**。4章は現状だと抽象度が高く、3.4/3.6 で決めた仕様（PendingOrder/Flatten/即時停止条件など）と整合が取れていないため、実装・運用フェーズで判断が発生して事故り得ます。次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `MaxPosition` の定義が曖昧（per-market / total / notional算出 / pending含有 / 実装位置）
4.1 の `MaxPosition | ポジション > MAX_NOTIONAL | 新規禁止` だと、以下が未確定です。
- `MAX_NOTIONAL` が **市場別**なのか **全体合計**なのか（1.2 では `MAX_NOTIONAL_PER_MARKET` / `MAX_NOTIONAL_TOTAL` がある）
- notional の算出（`markPx`/`mid`/`oraclePx` のどれで評価するか、`Size` の符号、USD換算の丸め）
- **pending order（未約定）** を上限に含めるか（含めないと短時間で超過し得る）
- gate の実装位置（`Executor::on_signal()` の同期gateで弾くのか、actor側で止めるのか、reduce_only は常に通すのか）

→ 4.1 に「定義」と「実装位置」を固定してください（例: per-market/total 両方を gate 化、評価価格は mark、pending含む、reduce_only は除外、等）。

### 2) 4.2「緊急停止手順」が 3.6 の具体化と不整合 + システム側の停止状態（HardStop）が未定義
3.6 では即時停止条件（Rejected多発/slippage異常など）と手順（Ctrl+C/kill/UIでflatten/cancel）が具体化されていますが、4.2 のトリガー/シーケンスは古いままです。さらに「停止シーケンス」をコードに落とす際に必要な **停止状態の扱い** が未定義です。

→ 4.2 を以下の形で確定してください。
- 4.2 のトリガー一覧を 3.6 の即時停止条件と統一（もしくは「Mainnet専用/任意」などスコープを明記）
- システム内の `HardStop`（latch）を定義し、発火後は **新規発注を完全停止**（reduce_only/キャンセルのみ許可）する
- 停止シーケンスの責務分解（誰が cancel を enqueue するか、誰が flatten を発火するか、WS切断中の扱い、完了条件=position=0）

### 3) 4.3 ロールバック基準が 3.6 Go/No-Go と二重管理になっていて、判定指標が曖昧
4.3 の「10トレードでedge負」「50トレードでedge負」だと、(a) edge の定義（expected/actual/fees込み）、(b) 評価窓（直近N? 1日?）、(c) どのデータから算出するか、が曖昧です。一方で 3.6 には 10/50/100 の Go/No-Go 判定が既に具体化されています。

→ 4.3 は 3.6 の Go/No-Go に寄せて統合してください（どちらかに断定）。
- **A. 4.3 を削除して 3.6 に一本化**（4章からは参照のみ）
- **B. 4.3 を「全期間共通の判定ルール」として再定義**し、3.6 の表と閾値/算出方法を一致させる（`actual_edge_bps`/`slippage_bps`/`pnl_cumulative` 等）

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（MaxPosition 定義の詳細化、HardStop latch/停止シーケンスの具体化、4.3 の 3.6 統合）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) 4章で追加した gate が 3.4 と不整合（Gate順序/フロー図/返却理由が古い）
4.1 で `MaxPosition*` と `HardStop` を追加しましたが、3.4 の「Gate チェック順序」「Executor フロー図（Gate 0〜3）」「ExecutionResult/RejectReason enum」が旧仕様のままです。このままだと実装時にどれを正とするか迷います。

→ 3.4 側を更新して整合させてください（最低限）:
- Gate 順序に `HardStop` と `MaxPosition` を追加（推奨: HardStop → READY-TRADING → MaxPosition → has_position → PendingOrder → ActionBudget）
- フロー図（Gate 0〜）も同じ順序に更新
- `RejectReason` / `SkipReason` に **MaxPositionPerMarket/MaxPositionTotal/HardStop** を追加し、どちらで返すかを断定

### 2) `MaxPositionTotal` の notional 計算が成立しない（単一 `mark_px` で全市場合計になっている）
`Σ notional(all)` を判定するのに `get_total_notional(mark_px)` の形だと、単一価格で全市場の notional を評価することになり不正確/不可能です。また `mark_px` の取得元が `Executor` 内で未定義です（`Executor` 構造体に market_data が無い）。

→ 4.1 の定義/疑似コードを以下のどちらかに寄せて確定してください。
- **A. MarketState 参照**: `MarketState`（または snapshot）から各 market の `mark_px` を取り、`Σ abs(size_i)×mark_px_i` を計算（pending も同様）
- **B. order price 参照**: pending/notional は「注文の limit price」で評価し、total は「各 pending/position が持つ price」で合算（mark は不要）

いずれの場合も「pending に reduce_only を含めるか（max position 判定では除外するのか）」も明記してください。

### 3) HardStop の「自動停止トリガー」を誰がどう判定するかが未定義（`RiskMonitor` が実体不明）
トリガー（累積損失/連続損失/Rejected多発/slippage異常）を列挙していますが、どのイベント（fills/post結果/flat完了）から誰がカウントして `hard_stop_latch.trigger()` を呼ぶかが未確定です。ここが曖昧だと実装が分岐します。

→ 최소限、以下を 4.2 に固定してください。
- `RiskMonitor` を 1タスク（actor）として定義し、入力（例: `ExecutionEvent` ストリーム）とカウンタ更新（pnl/slippage/reject数）を明記
- 発火時の呼び出し経路（`hard_stop_latch.trigger(reason)` + `executor.on_hard_stop(reason).await`）と、WS切断時に誰が発火させるか（ConnectionManager/ExecutorLoop など）を断定

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（3.4 側の gate 整合、MaxPosition の total notional 計算の修正、RiskMonitor/ExecutionEvent の定義）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) HardStop が「送信段階」に効かず、既に enqueue 済みの new_order が流れてしまい得る
HardStop gate は `Executor::on_signal()`（enqueue 前）にありますが、HardStop は非同期に発火するため、発火時点で **既に new_order キューに入っている注文**が残っていると、`ExecutorLoop::tick()` が後でそれを送信し得ます。HardStop 中に new_order が送られるのは事故です。

→ HardStop 発火時の挙動を確定してください（最低限どちらか）。
- **A. new_order キューを purge**: `BatchScheduler::drop_new_orders()` 等を用意し、`executor.on_hard_stop()` で new_order を全破棄 + `pending_markets_cache`/`pending_orders` も cleanup
- **B. tick で new_order を無効化**: HardStop 中は `batch_scheduler.tick()` が **cancel/reduce_only のみ**返すようにする（new_order は返さない/返しても drop して cleanup）

### 2) 4章で参照するフィールド/依存が `Executor` 構造体に反映されておらず、計画の疑似コードが成立しない
`Executor::on_signal()` は `hard_stop_latch` を参照し、MaxPosition は `market_state_cache` を参照し、HardStop 処理は `flattener`/`alert_service` を参照していますが、3.4 の `Executor` 構造体定義にそれらがありません。また `check_max_position()` は `self.config.max_notional_*` を参照しますが `config` が未定義です。

→ 3.4 の `Executor` 構造体/初期化の計画を更新して、最低限以下を追加・整合させてください。
- `hard_stop_latch: Arc<HardStopLatch>`
- `market_state_cache: Arc<MarketStateCache>`（または同等の markPx 参照元）
- `flattener` と `alert_service` の保持場所（Executor が持つ/別オーケストレータが持つ、の断定）
- `max_notional_per_market/max_notional_total` の供給元（定数/設定ファイル/Config struct）を断定し、`self.config` 参照を成立させる

### 3) MaxPosition/HardStop で必要になる PositionTrackerHandle の同期 API が未定義
4.1/4.2 の疑似コードで `get_notional()` / `get_pending_notional_excluding_reduce_only()` / `get_all_positions()` / `get_markets_with_pending_orders()` / `get_all_pending_cloids()` を使っていますが、3.3 の `PositionTrackerHandle` に API がありません（現状は has_position/pending gate/登録系のみ）。

→ どちらかに断定して計画に落としてください。
- **A. Handle の同期キャッシュを拡張**: positions/pending_orders の read-only スナップショットを `DashMap` 等で保持し、上記 API を同期で提供（HardStop/MaxPosition は同期で計算）
- **B. actor に問い合わせる**: `PositionTrackerMsg::GetAllPositions` などの問い合わせ API を追加し、HardStop は async で state を取得してから enqueue（同期 gate は「キャッシュにある範囲」だけで判定）

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（HardStop の new_order purge + tick 側の skip、Executor 構造体への依存追加、PositionTrackerHandle の同期 API 追加）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) HardStop purge の cleanup が `pending_orders_snapshot` に依存しており、レースで `pending_markets_cache` がリークし得る
現状の `Executor::on_hard_stop()` は `drop_new_orders()` で **cloid だけ**を取得し、`get_market_for_cloid()`（= `pending_orders_snapshot`）経由で market を引いて `unmark_pending_market()` しています。  
しかし `try_register_order()` の fallback `tokio::spawn { register_order(...) }` は **HardStop と非同期**で、HardStop 発火時点でまだ `pending_orders_snapshot` に反映されていない注文が存在し得ます。

その場合:
- `get_market_for_cloid()` が `None` になり、`pending_markets_cache` が解除されず残留
- さらに fallback の `register_order()` が **HardStop 後に**届くと、`remove_order()` 済みでも再登録されて state が汚れる（以降の cancel/cleanup 対象から漏れる可能性）

→ 最低限どちらかに寄せてください（推奨は A + B）。
- **A. `drop_new_orders()` の返り値を market 付きにする**: `Vec<PendingOrder>` もしくは `Vec<(ClientOrderId, MarketKey)>` を返し、`on_hard_stop()` は snapshot を参照せず確実に `unmark_pending_market(market)` できるようにする
- **B. fallback 登録を HardStop 対応にする**: spawn 内で `hard_stop_latch.is_triggered()` を確認し、HardStop 中は `register_order()` を呼ばない（= そもそも pending_orders_snapshot に入れない）
- **C. PositionTracker 側で弾く**: `PositionTrackerTask` が HardStop 中は `RegisterOrder` を無視する（または “dropped cloId set” を持って無効化）

### 2) HardStop が「tick→送信」の間に発火すると、キュー外に出た new_order が送信され得る（送信直前ガードが必要）
`drop_new_orders()` と `tick()` の HardStop skip は **キュー内**の new_order には効きますが、`ExecutorLoop::tick()` が `batch_scheduler.tick()` で `ActionBatch::Orders` を取得した後に HardStop が発火した場合、その batch 内の new_order は **既にキュー外**であり、現状の `tick()` 実装だとそのまま署名→post され得ます。

→ `ExecutorLoop::tick()` に **送信直前の HardStop ガード**を追加してください（仕様として固定）。
- `action_batch` が `Orders` のとき、署名/送信前に `hard_stop_latch.is_triggered()` をチェック
- HardStop 中なら `orders` を **reduce_only のみにフィルタ**し、drop した new_order は `unmark_pending_market()` + `remove_order()` で cleanup（drop_new_orders と同等の扱い）
- フィルタ後に `orders` が空なら「何も送らない」で return（nonce/post_id を消費しない）

### 3) `BatchScheduler` への HardStop 注入が計画上成立していない（`Option` + `&mut self` + セット箇所なし）
`BatchScheduler` が `hard_stop_latch: Option<Arc<HardStopLatch>>` を持ち、`set_hard_stop_latch(&mut self, ...)` で注入する設計ですが、計画上:
- `batch_scheduler` は `Arc<BatchScheduler>` として保持されており `&mut self` でセットできない
- そもそも `set_hard_stop_latch()` の呼び出し箇所が無く、`None` のままなら `tick()` の HardStop skip が常に無効

→ どちらかで確定してください（推奨は A）。
- **A. latch を必須化**: `BatchScheduler::new(..., hard_stop_latch: Arc<HardStopLatch>)` とし、`Option`/setter を削除（安全装置なので未設定を許容しない）
- **B. interior mutability に変更**: `hard_stop_latch: Mutex<Option<Arc<HardStopLatch>>>` + `set_hard_stop_latch(&self, ...)` にして、どの初期化手順で必ずセットされるかを 3.4/4.2 に明記

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（HardStop purge のレース対策、送信直前 HardStop ガード、BatchScheduler への HardStop 注入の成立）は反映されています。

- `BatchScheduler::drop_new_orders()` が `Vec<(ClientOrderId, MarketKey)>` を返し、`on_hard_stop()` が snapshot 非依存で cleanup
- `try_register_order()` fallback spawn が HardStop 中の register をスキップ
- `ExecutorLoop::tick()` が post-dequeue の HardStop をガードし、new_order を drop + cleanup
- `BatchScheduler.hard_stop_latch` が必須化され、未注入で動く設計になっていない

**結論: 4章は承諾**（実装に進んで良いレベル）。
