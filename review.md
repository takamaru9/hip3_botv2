s
# Oracle/Mark Dislocation Taker（HIP-3 / xyz限定）実装着手判断メモ

## 結論
- **実装着手：可**（ただし **Phase A：観測のみ** から開始し、Phase B（発注）へは DoD を満たしてから進む）。
- 現状の実装計画は、運用事故の主因になりやすい「Hard Risk Gate」「一意性検証」「冪等（cloid）」「WS健全性」「レート制限会計」を先に潰せる構造になっており、着手に十分な成熟度。

## 前提（一次情報での最終整合チェック済み）
以下は Hyperliquid 公式Docsの記載に基づく前提で、実装上の制約として固定する。

- **WS維持**：サーバは **60秒間クライアントに何も送られない** 場合、接続を閉じる。必要に応じて `{ "method": "ping" }` を送信し、`{ "channel": "pong" }` を受ける。
- **WS送信レート上限**（IP単位・全WS合算）：
  - 送信 **最大 2000 msg/min**
  - **inflight post 最大 100**
  - 接続 最大100、購読 最大1000（参考）
- **Nonce制約**：nonce は `(T - 2 days, T + 1 day)` の範囲。さらに **各アドレスで“最大100個のnonce（高い方）”が保持**され、新規Txはその集合の最小値より大きく、未使用である必要。
- **Subscriptions**：購読フォーマットは `{"method":"subscribe","subscription":{...}}`。`activeAssetCtx` は **perps/spot両方**の型が返り得る（`WsActiveAssetCtx` / `WsActiveSpotAssetCtx`）。

## 着手してよい範囲（推奨スコープ）
### Phase A（観測のみ）：着手OK
- WS（自前実装）
- Feed（bbo / activeAssetCtx の集約、age算定、欠損分類）
- Detector（oraclePx vs best の cross 検出、イベント化）
- Telemetry（統計、再現ログ、アラート、日次サマリ）

### Phase B（発注あり）：Phase AのDoD達成後
- “跨ぎ瞬間のみIOCでtaker” の実弾（IOC発注）
- 短時間でフラット化（反対売買・撤収）
- Hard Risk Gate を最優先にした停止・縮退

## 実装開始前の「Go条件」（これが満たせれば迷わず進める）
### 1) 仕様TODOをゼロにする
- `MarketSpec::format_price/format_size` を実装し、**golden test**（拒否・丸め・境界）を固定。
- `MarketSpec` の入力元（meta/perpDexs 等のどのフィールドを採用するか）をコードで固定し、**プロダクションの変更検知**を仕込む（差分でfail fast）。

### 2) Preflight（起動時の検証）が「落ちるべき時に必ず落ちる」
- **xyzがデプロイしているHIP-3銘柄に限定**（対象銘柄セットを起動時に確定）。
- `perpDexs/meta` で **Coin–AssetId の一意性検証**を実施し、曖昧（衝突/欠損/想定外dex混入）なら **起動拒否**。
- “WSでdex指定できない購読” を使う場合は、上記一意性検証が **唯一の安全弁**になるため、例外なく強制。

### 3) WS健全性・レート制限が「観測だけで証明できる」
- ping/pong、再接続（指数バックオフ＋jitter）、snapshot ack の扱い（`isSnapshot:true`）を実装。
- 送信レート（msg/min）と inflight post を **会計**し、上限接近で **段階的縮退**（購読削減→発注停止→全停止）。

### 4) Perps/Spot混在の例外を封じる
- `activeAssetCtx` が spot型で返るケースがある（公式Docs上も spot型が返り得る仕様）。
- 本botは perps（HIP-3）限定のため、対象coinがspot側に解決された場合は **購読対象から除外**し、ログ＋メトリクスで即時検知。

## 実装の最短ルート（価値が出る順）
1. `hip3-ws`：接続・購読・再接続・Heartbeat・RateLimit会計
2. `hip3-feed`：bbo/ctx統合、age計測、欠損・順序乱れ分類
3. `hip3-detector`：cross検出のみ（発注はしない）
4. `hip3-telemetry`：TriggerEvent永続化（Phase Aの母集団形成）
5. Phase Aの統計に基づき、Phase B（IOC発注）を段階投入

## Phase A（観測のみ）DoD（満たしたらPhase Bへ）
- 24時間以上の連続稼働で、WS再接続が自律復旧し続ける（手動介入なし）。
- msg/min / inflight post が常時観測され、上限接近時に縮退が機能する。
- 対象銘柄ごとに、以下が日次で出力される：
  - `cross_count`（oracle跨ぎ検出回数）
  - `bbo_null_rate`（BBO欠損率）
  - `ctx_age_ms`（activeAssetCtx遅延分布）
  - `best_age_ms`（bbo遅延分布）
  - “跨ぎの持続時間” の分布（1tick/複数tick）

## 参照（一次情報 / 公式Docs）
- WebSocket timeouts & heartbeats：
  - https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/timeouts-and-heartbeats
- Rate limits and user limits（WS: 2000 msg/min, inflight post 100 など）：
  - https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits
- Nonces and API wallets：
  - https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/nonces-and-api-wallets
- WebSocket subscriptions（activeAssetCtx / bbo 等）：
  - https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions
- WebSocket overview（disconnects/reconnects の注意）：
  - https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket

## 参考（実装上の落とし穴）
- `activeAssetCtx` が spot coin だと `activeSpotAssetCtx` が返る件（現象の具体例）：
  - https://github.com/hyperliquid-dex/hyperliquid-rust-sdk/issues/88