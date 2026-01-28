# Phase B 実装計画レビュー（3.2 OrderBuilder 実装）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（3.2 Week 1-2: 署名・発注 / OrderBuilder実装）  
確認日: 2026-01-20

## 結論
**未承諾**。このままだと `hip3-core` の型/丸め仕様と食い違い、また Signer（OrderWire）までの型の流れが不整合で実装が迷子になります。次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `MarketSpec::format_price/format_size` のAPI・型に合わせる（丸め方向も含む）
計画の `OrderBuilder::build_ioc()` は `Decimal` を受けて `self.spec.format_price(price)` を呼んでいますが、`hip3-core` 側は `format_price(price: Price, is_buy: bool)` / `format_size(size: Size)` です（buy/sell で丸め方向が変わる）。

- `price/size` の型は `Price/Size`（または `Decimal`→`Price/Size` への変換を明記）に統一
- `format_price(..., is_buy)` を必ず渡す（`is_buy` は side から導出）

### 2) `Order` / `PendingOrder` / `OrderWire` / `OrderTypeWire` の責務を一本化する
OrderBuilder が返す `Order { limit_px: String, sz: String, order_type: OrderType::Ioc }` は、3.2 Signer側で定義した `OrderWire { p: String, s: String, t: OrderTypeWire::ioc() }` と齟齬があります（`OrderType::Ioc` という型も本文内で未定義）。

- 方針を断定: 「OrderBuilderは `PendingOrder`（内部表現）を作る」 or 「OrderBuilderが `OrderWire`（送信用）を直接作る」
- どちらにせよ、IOC は `OrderTypeWire::ioc()`（`{"limit":{"tif":"Ioc"}}`）で表現する（Time-in-force として扱う）
- `ActionBatch::Orders(Vec<PendingOrder>)` から `Action.orders: Vec<OrderWire>` への変換経路（どこで `MarketSpec` を参照して文字列化するか）を計画に固定

### 3) `cloid` 生成と「再送時に同一cloidを使う」規約を計画で断定する（idempotency）
`cloid生成（correlation_id由来）` の一文だけだと、WS送信失敗/タイムアウト/切断で再送したときに **別cloidになって重複注文**になり得ます。

- `hip3-core::ClientOrderId` を使う前提で、生成タイミングと保持場所を明記（例: `PendingOrder` が `cloid` を保持し、再キュー/再送でも同一値を使う）
- `post_id` は相関IDであって注文IDではないので、`cloid` を `post_id` から生成しない（再送のたびに変わるため）
- `PositionTracker.pending_orders` のキーも `String` ではなく `ClientOrderId`（または同等の型）に寄せる方針を明記

---

## 再レビュー（2026-01-20）
前回の3点（`MarketSpec` API整合、型の責務分離、cloidのidempotency規約）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `Signer::build_and_sign` が二重定義＆ `PendingOrder::to_wire(spec)` と不整合
3.2 Signer節の `build_and_sign(batch, nonce)` は `o.to_wire()` を呼んでいますが、OrderBuilder節の `PendingOrder::to_wire(&self, spec: &MarketSpec)` と食い違っています。さらに OrderBuilder節で `build_and_sign(batch, nonce, market_specs)` を追加しており、計画本文として **「どれが唯一の正なのか」**が不明です。

→ 計画内で `build_and_sign` の最終API/実装を **1つに統一**し、3.4 の呼び出し（フロー図/ExecutorLoop疑似コード/タスク）もそのAPIに合わせて更新してください。

### 2) `OrderTypeWire` のコンストラクタ名が不一致（`ioc/gtc` vs `limit_ioc/limit_gtc`）
Signer節では `OrderTypeWire::ioc()` / `gtc()` を定義・利用している一方、OrderBuilder節の `TimeInForce::to_wire()` は `OrderTypeWire::limit_ioc()` / `limit_gtc()` を呼んでいます。

→ 命名をどちらかに統一し、テスト例（`OrderTypeWire::ioc()`）も含めて全箇所を揃えてください。

### 3) OrderBuilder API変更の波及が未反映（`Side` 未定義、Flattenの型が古い）
OrderBuilder節では `build_ioc()` が `Side` を受けますが、本文内で `Side` が定義されていません（既存の `hip3_core::OrderSide` などへ寄せるのが無難です）。また 3.3 Flatten は `Order` を返し、`cloid: &str` を渡していますが、OrderBuilder節の前提は `PendingOrder` + `ClientOrderId` です。

→ `Side` を既存型へ統一（または定義を追加）し、3.3 Flatten も `PendingOrder` を返して `ClientOrderId` を渡す形に更新してください（`submit_reduce_only(PendingOrder)` と整合させる）。

---

## 再レビュー（2026-01-20, 修正後）
前回の3点（`build_and_sign` API 統一、`OrderTypeWire` 命名統一、`Side`→`OrderSide` 統一と Flatten の型更新）は反映されています。  
**3.2 OrderBuilder は承諾**します（この内容で実装に進めます）。
