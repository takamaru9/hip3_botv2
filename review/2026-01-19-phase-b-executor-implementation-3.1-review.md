# Phase B 実装計画レビュー（3.1 hip3-executor基盤）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（3.1 Week 1: hip3-executor基盤）  
確認日: 2026-01-19

## 結論
**未承諾**。以下の3点を計画に反映してから、再レビューしたいです。

## 修正点（最大3点）

### 1) NonceManager: `next()` の仕様を「取引所制約」起点で固定する
現状案は `counter.fetch_add(1)` で単調増加は担保できますが、`server_time_offset_ms` が実質使われず、再起動/時刻ズレ/大量発注時に「取引所のnonce制約」を満たせない可能性があります。  
→ 計画に以下を明記し、その前提に沿ったアルゴリズムへ修正してください。

- **前提（取引所制約）**: nonce の許容窓（`T-2days .. T+1day`）や「高いnonce上位100個集合」など、守るべき制約を本文に記載
- **生成規則**: `nonce = max(last_nonce + 1, approx_server_time_ms())`（「単調増加」と「時刻近傍」の両立）
- **sync時のfast-forward**: `sync_with_server()` で `counter` も `server_time_ms` に追従させる（オフセット保存だけで終わらない）
- **オフセット定義の明確化**: `server - local` など、`approx_server_time_ms()` を一意に計算できる符号/定義に統一

### 2) NonceManager: ユニットテスト可能な設計（Clock注入）に直す
`SystemTime::now()` 直呼びはテストが不安定になりやすいので、計画段階で「Clock注入」を前提にしてください。

- `now_ms()` を差し替え可能にする（trait/関数ポインタ/クロージャ等）
- 最低限のテスト項目を計画に追加する（例: 単調増加、並行呼び出し、時刻逆行、syncでfast-forward、ドリフト 2s warn / 5s error）

### 3) BatchScheduler: “バッチ” の単位とバックプレッシャを設計として確定する
100ms周期で「何を1回の署名/送信にまとめるか」が未確定だと、nonce・署名・inflight上限（100 post）の会計が設計できません。  
→ 計画に以下を追記して、BatchSchedulerの責務と境界を固定してください。

- **バッチ単位**: 1 tick で「1つのL1 actionに複数orders/cancelsをまとめる」のか、「1注文=1 action」なのか（= nonce/署名の粒度）
- **inflight上限との整合**: inflightが上限に近い/超過時の挙動（enqueue拒否・待機・縮退）と、`max_batch_size` の意味
- **キュー溢れ時の方針**: 優先順位（cancel優先など）、ドロップ/拒否/リトライのどれを採用するか、ユニットテスト観点

---

### 追加の修正点（今回の指摘: 3点）

1) **BatchScheduler: inflight と “バッチ単位” の整合を取り直す**  
「1 L1 action = 1 inflight 消費」の前提に対して、`tick()` が `remaining_slots` や `remaining_slots > 1` を使って分岐しており、設計が矛盾しています。  
→ 1tickで送るpost数（1つだけ / 複数も可）を確定し、`tick()` の条件と `Batch` の返り値（`Option<Batch>` or `Vec<Batch>`）を合わせてください。

2) **BatchScheduler: キュー溢れ/縮退時の挙動が危険（cancel無制限・order拒否の扱い）**  
現状の例だと cancel は無制限に積めてしまい、メモリ/遅延リスクが出ます。また high-watermark 到達時に order を「積まずに返す」設計なので、上流がどう扱うかを決めないとシグナルが黙って落ちます。  
→ cancelにも容量/上限/縮退方針を入れる、high-watermark時は「キューに積むが発注は遅らせる」等の方針を明文化（戻り値も `QueueFull` と `InflightFull` などに分離推奨）。

3) **計画全体のAPI整合: NonceManager型とBatchScheduler APIが後続と食い違う**  
`NonceManager<C: Clock>` にした一方で、3.4の `Executor` は `Arc<NonceManager>` のままです。また Executor の `batch_scheduler.enqueue(order).await` と、3.1の `enqueue_order()`（sync/戻り値あり）が一致していません。  
→ 計画本文の型/関数名/同期方式を揃え、Executorが「どこでtickを回し、どこでnonceを払い出し、どこで署名してpostするか」を1本の流れで確定してください。

---

## 再レビュー（2026-01-19）
前回の追加3点（バッチ単位の確定、cancelキュー上限、Executor API整合）は反映されており、方向性は良いです。  
ただし 3.1 全体としてはまだ **未承諾** で、次の3点を計画に追記/修正してください。

### 追加の修正点（今回の指摘: 3点）

1) **post応答タイムアウト/切断時の inflight 回収（デッドロック防止）**  
`on_response()` 前提だと、応答が来ない/切断/再接続で inflight が戻らず `tick()` が止まる可能性があります。  
→ `PostRequestManager`（`id -> oneshot`）＋タイムアウト（例: 3-5s）を設計に入れ、成功/失敗/timeout/切断の全経路で `on_batch_complete()` が必ず呼ばれることを保証してください（切断時は inflight をリセット or 全回収）。

2) **送信失敗時にバッチ内容が“消える”問題（再キュー or 明示失敗）**  
`tick()` はキューから drain して返すため、`ws_sender.post()` が失敗すると注文/キャンセルが黙って失われます（特に reduce-only/flatten は危険）。  
→ 送信失敗時の扱いを計画で固定してください（例: (a) バッチを先頭に再キューしてリトライ、(b) reduce-only だけ再キュー、(c) 失敗を上流へ返してアラート＋HardStop）。

3) **reduce-only（フラット化）を新規注文より常に優先し、縮退中も通す**  
設計表では `cancel > reduce-only > new order` ですが、現状のキュー構造だと reduce-only が新規注文の後ろに溜まり得ます。  
→ reduce-only 用の別キュー/優先度付きキューを設ける、もしくは `PendingOrder` を優先度で並べ替えるなど、**TimeStop/Flatten が常に先に流れる**設計にしてください（高水位縮退中も reduce-only は送る方針を明記）。

---

## 再レビュー（2026-01-20）
前回の3点は反映されています（`PostRequestManager`/タイムアウト、送信失敗時の再キュー方針、3キュー構造で reduce-only 優先）。  
ただし 3.1 はまだ **未承諾** で、次の3点を直してください。

1) **inflight=100 で cancel を送ろうとして上限を破る**  
計画の `BatchScheduler::tick()` が `inflight >= 100` でも cancel を収集し `inflight_tracker.increment()` しています。inflight上限は post全体に効くため、上限到達時は cancel でも送れません。  
→ `inflight >= 100` のときは **送信しない（None）** か、少なくとも **incrementしない** 形に修正し、「cancelは優先するが上限は超えない」を設計として固定してください。

2) **WS送信エラー時に inflight が戻らない（リーク）**  
`tick()` は Batch作成時に inflight を increment しますが、`ws_sender.post()` エラー経路で `on_batch_complete()` が呼ばれていません。  
→ 送信エラー経路でも必ず `on_batch_complete()` する（もしくは increment を “実際に送れた後” に移す）など、**inflightが必ず回収される**ようにしてください。

3) **応答相関キーが nonce 前提で危うい（WS仕様は post id 相関）**  
`PostRequestManager` が `nonce -> request` で管理されていますが、WSのpostは基本 `id` で相関されます（`nonce` が常に応答に入る保証が計画内にない）。  
→ `post_id -> request` を基本にし、必要なら `nonce` は付随情報として保持する形に変更してください（少なくとも「なぜnonceで相関できるか」の根拠を計画に明記）。

---

## 再レビュー（2026-01-20, 追補）
上の3点（`inflight>=100`時は`None`、`tick()`でincrementしない、`post_id`相関）は反映されました。  
ただし 3.1 全体としてはまだ **未承諾** で、次の3点を直してください。

1) **post_manager登録とinflight増減の“状態遷移”がズレており、切断/タイムアウトで破綻し得る**  
`post_manager.register()` が送信前に行われ、`on_batch_sent()`（inflight increment）が送信成功後になっています。送信待ち中に切断/異常が起きると、「pendingに入っているがinflightに入っていない」状態が作れます。  
→ `register + inflight increment` を「送信が確定した瞬間」に揃える（もしくは PendingRequest に `sent: bool` を持たせ、timeout/disconnectの回収対象を sent のみに限定）など、状態遷移を計画として確定してください。

2) **`InflightTracker` が未定義で、既存WSレート制限との二重管理リスクがある**  
計画では `InflightTracker` を使っていますが、構造体定義がありません。またリポジトリには `crates/hip3-ws/src/rate_limiter.rs` が既に inflight を会計しています。  
→ (a) `RateLimiter` を唯一の inflight ソースにする、または (b) `InflightTracker` を明示定義し、WS側と executor側で二重会計しない設計にしてください。

3) **切断時のinflight回収が二重に存在しており、どちらが正なのか不明**  
`BatchScheduler::on_disconnect()` は `reset()` を想定していますが、`ExecutorLoop::on_disconnect()` は pending数分 `on_batch_complete()` を呼ぶ方針になっています。  
→ どちらかに統一し、重複しない（＝過剰decrement/過剰resetにならない）ように、計画本文・テスト項目・タスクを揃えてください。

---

## 再レビュー（2026-01-20, 追補2）
前回の3点（`sent`状態遷移の明確化、`InflightTracker`定義、切断時回収の一元化）は反映されています。  
ただし 3.1 はまだ **未承諾** で、次の3点を直してください。

1) **InflightTracker の underflow（`fetch_sub`）が致命的**  
`decrement()` が `fetch_sub(1)` だと、二重decrement等で 0→`u32::MAX` にラップし得ます。  
→ `saturating_sub` 相当（`fetch_update`/CASループ）にして、0未満にならないことを保証してください（`limit` も活用して上限超過も防止推奨）。

2) **PostRequestManager の `drain()` の分解が不正（実装時にコンパイル落ち）**  
`for (_, (_, req)) in self.pending.drain()` は `DashMap::drain()` の返り値と合いません。  
→ `for (_post_id, req) in ...` 等に修正し、「このまま写して実装できる」粒度にしてください。

3) **RateLimiter との inflight 会計を実装タスクとして明示する**  
計画では `InflightTracker` を唯一の inflight ソースにしていますが、現状の `crates/hip3-ws/src/rate_limiter.rs` は inflight を会計しています。  
→ 実装で「RateLimiter の inflight 会計を外す/統合する/参照に置き換える」のどれを採るかを確定し、P0タスクとして追記してください（ドリフトするとデバッグ不能になります）。
