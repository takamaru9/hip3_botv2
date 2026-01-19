

# 8.9.1 実運用コア（Feed + Detection + Execution）— WS自前実装メモ

## 目的
- 低遅延で **Feed → Detection → Execution** を回す。
- SDK非依存で、運用品質（再接続・状態同期・相関ID・レート制御・keep-alive）まで自前で担保する。

## 1. 接続（Connection）
- Mainnet: `wss://api.hyperliquid.xyz/ws`
- Testnet: `wss://api.hyperliquid-testnet.xyz/ws`
- サーバ側切断は「通常系」として扱う（再接続・整合回復を前提にする）。

## 2. Keep-alive（Heartbeat）
- サーバは **60秒間クライアントへメッセージを送っていない接続を閉じる**。
- 60秒以内に購読データが流れない可能性がある場合、クライアントから heartbeat を送る。
  - 送信: `{ "method": "ping" }`
  - 応答: `{ "channel": "pong" }`
- 実装目安: outboundが45秒以上ない場合に `ping`。`pong` 連続未達なら再接続。

## 3. 購読（Subscriptions）
### 3.1 共通フォーマット
```json
{
  "method": "subscribe",
  "subscription": { ... }
}
```
- 成功応答: `channel="subscriptionResponse"`
- データ本体は `channel` が購読タイプ名（例: `"l2Book"`, `"bbo"`, `"activeAssetCtx"`, `"orderUpdates"`）で流れる。

### 3.2 HIP-3 bot向けの最低限購読（例）
- `bbo(coin)`: best bid/ask（低トラフィック）
- `l2Book(coin)`: 板（必要なら）
- `activeAssetCtx(coin)`: oraclePx / markPx 等
- ユーザ系: `orderUpdates(user)`, `userEvents(user)`, `userFills(user)`

### 3.3 READY条件（重要）
- 戦略稼働（発注許可）は **必要購読がすべてREADY** になってから。
- `userFills` は初回に `isSnapshot:true` が来る（これを受信したらREADY）。
- `bbo/l2Book/activeAssetCtx` は初回データ受信でREADY。

## 4. Post requests（WS経由の info/action）
### 4.1 リクエスト（相関ID必須）
```json
{
  "method": "post",
  "id": 123,
  "request": {
    "type": "info" | "action",
    "payload": { ... }
  }
}
```

### 4.2 レスポンス（idで相関）
```json
{
  "channel": "post",
  "data": {
    "id": 123,
    "response": { "type": "info" | "action" | "error", "payload": { ... } }
  }
}
```

### 4.3 使いどころ
- 起動直後/再接続直後に `post: info` を使って補助スナップショットを取る（購読到着待ち短縮/欠損補完）。

## 5. レート制限（実装必須のガード）
- IP単位で（全接続合算）
  - WS接続数、購読数、user系ユニークユーザ数
  - 送信メッセージ数（2000/分）
  - inflight post（100）
- 実装方針:
  - outboundは直送禁止（送信キュー + トークンバケットで2000/分を遵守）
  - `post` は semaphore 等で inflight<=100 を担保

## 6. 自前WSクライアント構成（推奨コンポーネント）
- **ConnectionManager**: connect/reconnect（指数バックオフ + jitter）、再接続後に必ず購読復元
- **SubscriptionManager**: desired購読集合の差分適用、READY条件管理
- **HeartbeatManager**: 45秒無outboundでping、pong未達で再接続
- **Router/Dispatcher**: inboundを `channel` でルーティング
  - `subscriptionResponse` → SubscriptionManager
  - `post` → PostRequestManager（id相関）
  - `pong` → HeartbeatManager
  - それ以外（`bbo`, `l2Book`, `activeAssetCtx`, `orderUpdates` 等）→ Feed handler
- **PostRequestManager**: `id -> oneshot` で完了通知、タイムアウト/エラー回収
- **MarketState**: Feed→Detection 用の共有状態（single-writer推奨）

## 7. 再接続時の整合回復（Trading botとしての必須シーケンス）
1) 発注停止（RiskGate = HARD_STOP）
2) WS再接続
3) desired購読を全て再送
4) READY条件を満たすまで待機（snapshot/初回受信）
5) 必要なら `post: info` で補助スナップショット
6) 発注再開（RiskGate解除）

## 8. メッセージ型（最小スキーマ）
### Outbound
- `Subscribe { subscription }`
- `Unsubscribe { subscription }`
- `Ping`
- `Post { id, request { type, payload } }`

### Inbound
- `SubscriptionResponse`
- `ChannelData { channel, data }`
- `PostResponse { id, response }`
- `Pong`

## 9. 最小購読テンプレ（HIP-3 “逆MM” 用）

- Marketごと: `bbo(coin)`（または `l2Book(coin)`）, `activeAssetCtx(coin)`
- User: `orderUpdates(user)`, `userFills(user)`



## 10. 参照（一次情報URL）
- Hyperliquid Docs（WebSocket / Subscriptions / Post requests / Heartbeat / Rate limits）: https://hyperliquid.gitbook.io/hyperliquid-docs/
- WebSocket（エンドポイント）: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket
- Subscriptions（購読一覧・スキーマ）: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions
- Post requests（WS経由 info/action, id相関）: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/post-requests
- Heartbeat（ping/pong, 60秒ルール）: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/heartbeat
- Rate limits（WS制限）: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits

## 11. 自前WS実装の意義（メリット）
自前実装の価値は「WSフレーミングをゼロから書く」ことではなく、**運用上のSLO（安定性・整合性・安全性・拡張性）を戦略とインフラに最適化**できる点にある。

### 11.1 信頼性・整合性を“仕様”として担保
- **再接続シーケンスを固定化**できる（例：RiskGate HARD_STOP → resubscribe → READY確認 → snapshot補完 → 再開）。
- 「購読は戻ったが userFills snapshot 未受信」「板は来たが assetCtx が古い」などの **部分復旧状態を明確に管理**し、誤発注リスクを下げられる。

### 11.2 レート制限・inflight を全体で一元制御
- IP単位で効く制限（送信2000/分、inflight post 100、購読数等）を前提に、**送信キュー/トークンバケット/Semaphoreを単一の制御面に集約**できる。
- reconnect storm / mass resubscribe 時のスパイク耐性が上がる。

### 11.3 遅延とジッタの制御
- 受信→状態更新→検知→発注のクリティカルパスを **single-writer / lock最小**に設計でき、遅延とジッタを抑えやすい。
- ping間隔、バックプレッシャー、キュー深さ、JSONパース等を戦略要件に合わせて調整できる。

### 11.4 可観測性（Observability）の作り込み
- `channel別遅延`, `READYまでの時間`, `pong RTT`, `reconnect原因別回数`, `post timeout率`, `欠損補完回数` などを **トレード運用に直結する指標として計測**できる。
- 障害解析で「いつ・どの購読が・どの状態で戻ったか」を追える。

### 11.5 仕様変更・戦略変更への追従
- 新購読タイプ追加、payload変更、複数dex取り扱い、coin解決ロジック変更などを、**内部ドメインモデル中心に拡張**できる。
- SDKのアップデート待ちや互換性影響を受けにくい。

### 11.6 セキュリティ（鍵管理）の自由度
- action（署名付き）を含む場合、鍵管理（remote signer/権限分離等）を自前設計でき、攻撃面を縮小しやすい。

### 11.7 コスト（注意点）
- 重いのはWS接続そのものではなく、**再接続・整合・レート・観測を正しく作る**コスト。
- ただし「プロダクション運用」「自動発注」「戦略都合の状態機械が必要」な場合、このコストを払う価値が出やすい。

## 11. 自前でWSを実装する意義（メリット）

自前実装のメリットは「接続そのもの」ではなく、**運用上のSLO（安定性・整合性・安全性・拡張性）を、戦略とインフラに最適化できる**点にある。

### 11.1 信頼性と整合性を“仕様”として担保
- **再接続シーケンスを戦略要件に合わせて固定化**できる（例：RiskGate HARD_STOP → resubscribe → READY確認 → snapshot補完 → 再開）。
- 部分復旧状態（板は来たが userFills snapshot が未到着、assetCtx が古い等）を明示管理し、誤発注を防げる。

### 11.2 レート制限・inflightを全接続合算で一元管理
- IP単位・全接続合算で効く制限（送信2000/分、inflight post 100 等）に対して、**トークンバケット + semaphore** を統一制御面に集約できる。
- reconnect storm / mass resubscribe のスパイクで制限に当たりにくい。

### 11.3 遅延とジッタの制御
- 受信→状態更新→検知→発注を single-writer / lock最小で設計し、クリティカルパスを短縮できる。
- ping間隔、キュー深さ、バックプレッシャー、JSONパース方式などを戦略要件に合わせて調整できる。

### 11.4 可観測性（Observability）をトレード視点で作り込める
- `channel別遅延`, `READYまでの時間`, `pong RTT`, `reconnect原因別回数`, `post timeout率`, `欠損補完回数` など、運用上重要な指標をそのままメトリクス化できる。
- 事故対応時に「いつ・どの購読が・どの状態で・どの順に戻ったか」を追跡しやすい。

### 11.5 仕様変更・戦略変更への追従が速い
- 新しい購読タイプ追加やpayload変更に対して、内部ドメインモデル（例：MarketKey）中心で拡張できる。
- SDKのアップデート待ちや互換性問題に引きずられにくい。

### 11.6 セキュリティとキー取り回しの自由度
- action（署名付き）を含む場合、鍵管理（権限分離、remote signer、署名専用プロセス等）を自前設計でき、攻撃面を縮小しやすい。

### 11.7 コスト（判断の軸）
- もっとも重いのは WebSocketフレーミングではなく、運用品質（再接続・整合・レート・観測）を正しく作るコスト。
- ただしプロダクション運用・自動発注・状態機械が重要な戦略では、投資価値が高い。