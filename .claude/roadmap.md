# hip3_botv2 ロードマップ

**作成日**: 2026-01-19
**最終更新**: 2026-01-25
**プロジェクト**: Oracle/Mark Dislocation Taker Bot (HIP-3 / xyz限定)

---

## 参照整合情報

| 項目 | 値 |
|------|-----|
| **対象コミット** | `5ff3b13` |
| **検証実行日** | 2026-01-19 |
| **検証環境** | macOS (Darwin 25.0.0) / Rust 1.x |
| **日次出力先** | `data/mainnet/signals/*.jsonl` |
| **ログ出力先** | `logs/hip3-bot.log` / stdout (JSON) |
| **Prometheus** | `:9090/metrics` |

---

## 1. プロジェクト概要

### 戦略

**Oracle/Mark Dislocation Taker**: HIP-3市場で `oraclePx` と `best bid/ask` の乖離を収益化する戦略。

- **狙い**: best bid/ask が oraclePx を跨ぐ瞬間に IOC で踏む
- **勝ち筋**: ロジックより **Hard Risk Gate（停止品質）** と **市場選定**
- **執行**: IOC taker、短時間でフラット回帰（time stop + reduce-only）
- **対象**: xyz DEX (UNIT) のみ

### アーキテクチャ（Crate構成）

```
hip3_botv2/
├── Cargo.toml (workspace)
└── crates/
    ├── hip3-core       # ドメイン型（MarketKey, Price, Size, MarketSpec）
    ├── hip3-ws         # 自前WebSocketクライアント（再接続、Heartbeat、RateLimit）
    ├── hip3-feed       # マーケットデータ集約（BBO、Oracle、鮮度追跡）
    ├── hip3-registry   # Market Discovery、Spec同期、Preflight検証
    ├── hip3-risk       # Hard Risk Gates（8ゲート）
    ├── hip3-detector   # Dislocation検知（oracle vs best crossing）
    ├── hip3-executor   # IOC執行（NonceManager, Signer, Executor, BatchScheduler, WsSender）
    ├── hip3-position   # ポジション管理（PositionTracker, TimeStop, FlattenOrderBuilder）
    ├── hip3-telemetry  # Prometheus、構造化ログ、日次統計
    ├── hip3-persistence # JSON Lines保存（signals, followups）
    └── hip3-bot        # メインアプリケーション統合
```

---

## 2. Phase区分と状態

| Phase | 目的 | 期間 | 状態 | 進捗 |
|-------|------|------|------|------|
| **Phase A** | 観測・EV市場特定 | Week 1-12 | **完了**（分析済み） | 100% |
| **Phase A+** | フォローアップ検証 | - | **実行中** | 50% |
| **Phase B** | 超小口IOC実弾 | Week 13-16 | **Mainnet少額テスト実行中** | 98% |
| **Phase C** | 停止品質改善 | Week 17-20 | - | - |
| **Phase D** | 市場自動入替 | Week 21-24 | - | - |
| **Phase E** | 収益最大化（将来） | TBD | - | - |

**総期間**: 約6ヶ月（24週間）

---

## 3. Phase A 完了条件（DoD）

Phase B へ進む前に以下を **全て** 満たすこと：

| 項目 | 状態 | 判定基準 | 確認方法 |
|------|------|---------|----------|
| **24h連続稼働** | 🟡 部分達成 | WS再接続が自律復旧し続ける（手動介入なし） | 15h稼働、WS自律復旧1回確認。24hには未達だがWS安定性は確認済み |
| **レート制限観測** | ✅ 完了 | msg/min / inflight post が常時観測され、上限接近時に縮退が機能 | Prometheus: `hip3_ws_msgs_sent_total`, `hip3_post_inflight` |
| **日次出力指標** | ✅ 完了 | 対象銘柄ごとに日次で以下が出力される | ファイル: `data/mainnet/signals/*.jsonl` 存在確認 |

### 日次出力必須指標

| 指標 | 説明 | 状態 |
|------|------|------|
| `cross_count` | oracle跨ぎ検出回数 | ✅ |
| `bbo_null_rate` | BBO欠損率 | ✅ |
| `ctx_age_ms` (P50/P95/P99) | activeAssetCtx遅延分布 | ✅ |
| `bbo_recv_interval` (P50/P95/P99) | bbo受信間隔分布 | ✅ |
| `cross_duration_ticks` | 跨ぎの持続時間分布 | ✅ |

### Phase B 移行条件

- [x] Phase A DoD を全て満たす（24h稼働は部分達成、WS安定性は確認済み）
- [x] 2〜3市場で EV 正の兆候 → **6市場で高EV確認**（HOOD, MSTR, NVDA, COIN, CRCL, SNDK）
- [x] Risk Gate の停止品質が安定 → HeartbeatTimeout 1回、自律復旧
- [x] ctx/bbo の受信間隔分布が把握済み
- [x] bbo_null_rate が許容範囲

**Phase B 準備開始可能**: 詳細は `.claude/specs/2026-01-19-phase-a-analysis.md` 参照

---

## 3.5 Phase A+ フォローアップ検証

### 目的

シグナル発生後の収束状況を記録し、シグナルの有効性を検証する。

### 機能

| 機能 | 状態 | 説明 |
|------|------|------|
| **Followup Snapshot** | ✅ 完了 | T+1s, T+3s, T+5s でマーケット状態をキャプチャ |
| **FollowupRecord** | ✅ 完了 | 16フィールドの検証データ構造 |
| **FollowupWriter** | ✅ 完了 | JSON Lines形式で永続化 |
| **VPS稼働** | ✅ 完了 | 25/32銘柄でデータ収集中 |

### 出力ファイル

```
data/mainnet/signals/
├── signals_YYYY-MM-DD.jsonl       # シグナル（T+0）
└── followups_YYYY-MM-DD.jsonl     # フォローアップ（T+1s/T+3s/T+5s）
```

### 検証指標

| 指標 | 計算 | 意味 |
|------|------|------|
| エッジ残存率 | `edge_t5 / edge_t0` | 5秒後にエッジがどれだけ残っているか |
| Oracle収束 | `oracle_moved_bps` の符号 | + = Oracleがmarket方向へ移動 |
| Market収束 | `market_moved_bps` の符号 | + = Marketがoracle方向へ移動 |
| シグナル有効性 | `edge_change_bps < 0` | エッジ縮小 = 正しいシグナル |

### Phase B 移行追加条件

- [ ] 24時間以上のフォローアップデータ収集完了
- [ ] シグナル有効性（エッジ縮小率）の統計分析完了
- [ ] エッジ残存率が期待通りであることを確認

詳細: `.claude/specs/2026-01-20-followup-snapshot-feature.md`

---

## 4. Risk Gate 実装状況

全 8 ゲート実装完了（Phase A）

| Gate | 条件 | 非保有時アクション | 保有時アクション | 状態 |
|------|------|-------------------|-----------------|------|
| **OracleFresh** | `ctx_age_ms > MAX_CTX_AGE_MS` | 新規禁止 | 新規禁止 | ✅ |
| **MarkMidDivergence** | `abs(mark - mid)/mid > Y_bps` | 新規禁止 | サイズ1/5 | ✅ |
| **SpreadShock** | `spread > k × EWMA(spread)` | サイズ1/5 | サイズ1/5 | ✅ |
| **OiCap** | OI cap到達 | 新規禁止 | 新規禁止 | ✅ |
| **ParamChange** | tick/lot/fee変更検知 | 全キャンセル + 停止 | 縮小→停止 | ✅ |
| **Halt** | 取引停止検知 | 全キャンセル + 停止 | 縮小→停止 | ✅ |
| **NoBboUpdate** | bbo更新途絶 | 新規禁止 | 縮小→停止 | ✅ |
| **TimeRegression** | 受信timeが巻き戻り | 全キャンセル + 停止 | 縮小→停止 | ✅ |

---

## 5. P0 タスク実装状況

### Phase A 関連（完了）

| ID | タスク | 状態 |
|----|--------|------|
| P0-4 | READY-MD/READY-TRADING分離 | ✅ |
| P0-7 | 初回BBO未達タイムアウトポリシー | ✅ |
| P0-8 | レート制限"会計"メトリクス | ✅ |
| P0-12 | monotonic鮮度（age-based） | ✅ |
| P0-14 | BboNull判定 | ✅ |
| P0-15 | xyz DEX同定（Preflight） | ✅ |
| P0-16 | TimeRegression検知 | ✅ |
| P0-23 | format_price/format_size | ✅ |
| P0-24 | HIP-3手数料2x + userFees | ✅ |
| P0-26 | perpDexs API取得 | ✅ |
| P0-27 | Coin-AssetId一意性検証 | ✅ |
| P0-28 | format_price/sizeテストベクタ | ✅ |
| P0-30 | Perps/Spot混在封じ | ✅ |
| P0-31 | Phase A DoD指標 | ✅ |

### Phase B 関連

| ID | タスク | 状態 | 詳細 |
|----|--------|------|------|
| P0-11 | セキュリティ/鍵管理 | ✅ | KeyManager + Signer (EIP-712 PhantomAgent) 実装完了 |
| P0-19a | NonceManager (0起点禁止) | ✅ | Clock trait + server_offset同期 + CASループ実装 |
| P0-19b | Batching (100ms周期) | ✅ | 3-tier queue (cancel > reduce_only > new_order)、inflight上限100 |
| P0-25 | NonceManager serverTime同期 | ✅ | 2s warn / 5s error ドリフト検知 |
| P0-29 | ActionBudget制御アルゴリズム | ✅ | BatchScheduler + InflightTracker + 縮退モード |
| - | PositionTracker | ✅ | Actor pattern + DashMap caches、pending_markets_cache |
| - | TimeStop / FlattenOrderBuilder | ✅ | ポジション保持時間制限 + reduce_only生成 |
| - | HardStopLatch / RiskMonitor | ✅ | 永続停止 + 累積損失監視 |
| - | MaxPosition Gates | ✅ | Per-market ($50) + Total ($100)、Decimal比較 |
| - | Executor / ExecutorLoop | ✅ | 7-Gate check、WsSender統合、署名フロー |
| - | TradingReadyChecker | ✅ | 4 AtomicBool flags + watch channel |
| - | WsSender統合 (hip3-ws拡張) | ✅ | WsWriteHandle、RealWsSender、PostRequest/Response、orderUpdates購読 |
| - | Pending二重計上修正 | ✅ | register_order_actor_only() API追加、TrySendError::Full経路修正 |
| - | ExecutorConfig notional上限追従 | ✅ | detector.max_notional ($20) をExecutorConfigに同期 |

---

## 6. Phase B タスク詳細

### hip3-executor 実装 ✅

```
hip3-executor/
├── nonce.rs          # NonceManager（Clock trait, CASループ, server_offset同期）
├── batch.rs          # BatchScheduler（3-tier queue）, InflightTracker
├── signer.rs         # KeyManager, Signer, Action/OrderWire/CancelWire, EIP-712 PhantomAgent
├── executor.rs       # Executor.on_signal()（7 Gate checks）, ExecutorLoop.tick()
├── ready.rs          # TradingReadyChecker（4 AtomicBool flags + watch channel）
├── risk.rs           # HardStopLatch, RiskMonitor, MaxPositionGates
├── ws_sender.rs      # WsSender trait, SendResult, SignedAction, MockWsSender
└── real_ws_sender.rs # RealWsSender（WsSender実装、hip3-ws統合）

hip3-ws/ (拡張)
├── message.rs        # PostRequest/Response、OrderUpdatePayload、FillPayload追加
├── ws_write_handle.rs # WsWriteHandle（fire-and-forget送信、READY-TRADING check）
├── connection.rs     # outbound channel追加、inflight tracking
└── subscription.rs   # orderUpdates/userFills購読helpers
```

### NonceManager 設計 ✅

- **初期化**: `now_unix_ms` へ fast-forward（0起点禁止）
- **生成規則**: `max(last+1, approx_server_time)` - 単調増加と時刻近傍を両立
- **serverTime同期**: 2s warn / 5s error ドリフト検知、counter fast-forward
- **Clock trait**: テスト可能な時刻取得（SystemClock / MockClock）

### BatchScheduler 優先順位 ✅

| 優先度 | 送信種別 | 理由 |
|--------|----------|------|
| **1 (最優先)** | Cancel | 意図しない約定回避 |
| **2** | ReduceOnly | テール損失回避（必達再キュー） |
| **3 (最抑制)** | NewOrder | 機会損失より事故回避 |

### Executor Gate Check Order ✅

1. HardStop → Rejected(HardStop)
2. READY-TRADING → Rejected(NotReady)
3. MaxPositionPerMarket ($50) → Rejected(MaxPositionPerMarket)
4. MaxPositionTotal ($100) → Rejected(MaxPositionTotal)
5. has_position → Skipped(AlreadyHasPosition)
6. PendingOrder → Skipped(PendingOrderExists)
7. ActionBudget → Skipped(BudgetExhausted)

### セキュリティ/鍵管理（P0-11）

| 項目 | 設計 |
|------|------|
| API wallet分離 | 観測用（読取のみ）と取引用（署名権限）を分離 |
| secret配置 | docker env直書き回避 → ファイルマウント or vault |
| ローテーション | 定期 + 漏洩時緊急手順を文書化 |
| 漏洩時停止 | 検知→全注文キャンセル→取引停止→key無効化→新key発行 |

---

## 7. 非交渉ライン（38項目・抜粋）

設計上必ず守るべき原則。**全38項目**の詳細は `plans/2026-01-18-oracle-dislocation-taker.md` Section 11 参照。

### 安全性

| # | 原則 |
|---|------|
| 1 | **冪等性**: cloid必須、再送で二重発注しない |
| 2 | **停止優先**: 例外時は「継続」ではなく「縮小/停止」に倒す |
| 5 | **仕様変更検知**: tick/lot/fee変更は即停止 |
| 19 | **セキュリティ/鍵管理**: Phase B前に権限分離を確定 |
| 24 | **レート制限優先順位**: reduceOnly > cancel > new post |

### データ整合性

| # | 原則 |
|---|------|
| 6 | **spec_version付与**: 全イベントに刻む（再現性確保） |
| 11 | **Decimal精度保持**: f64は派生フィールドのみ |
| 14 | **monotonic鮮度ベース**: 全ての鮮度判定はローカル受信時刻 |
| 29 | **HIP-3手数料2x**: userFees取得 + HIP-3倍率反映 |
| 33 | **format_price/sizeテストベクタ**: edge判定は丸め後で評価 |

### 接続・通信

| # | 原則 |
|---|------|
| 3 | **READY条件Phase分離**: READY-MD（観測）とREADY-TRADING（取引） |
| 8 | **post inflight分離**: メッセージ数とinflight postは別セマフォ |
| 9 | **Heartbeat無受信基準**: 45秒（60秒ルールの安全マージン） |
| 12 | **single-instance方針**: 1アカウント1インスタンス |
| 25 | **nonce/バッチング**: 100ms周期、IOC/GTCとALO分離 |

### 市場検証

| # | 原則 |
|---|------|
| 10 | **静的プリフライトチェック**: 起動時に購読数/制限を検証 |
| 21 | **BboNull判定**: bestBid/bestAskがnullならREADY除外 |
| 22 | **xyz DEX同定**: Preflightでdex name確定、空文字禁止 |
| 31 | **coin衝突検知**: xyz DEXとデフォルトDEXで同名coin衝突→起動拒否 |
| 35 | **Perps/Spot混在封じ**: spot型なら購読対象から除外 |

### Phase管理

| # | 原則 |
|---|------|
| 36 | **Phase A DoD必須**: 24h連続稼働・レート制限観測・日次指標出力 |
| 37 | **Go条件4項目**: 仕様TODO=0、Preflight堅牢性、WS健全性証明、Perps/Spot封じ |
| 38 | **Nonce制約遵守**: (T-2d, T+1d)範囲 + 最大100個の高nonce保持 |

---

## 8. Phase 別タスク概要

### Phase A（観測のみ）- Week 1-12

**目的**: EV見込みのある市場を特定、Gate停止品質の検証

| Week | タスク | 状態 |
|------|--------|------|
| 1-2 | Cargo workspace、hip3-core、hip3-ws基本 | ✅ |
| 3-4 | SubscriptionManager、HeartbeatManager、RateLimiter | ✅ |
| 5-6 | Feed、Registry、OracleFresh/FeedHealth/ParamChange Gate | ✅ |
| 7-8 | 残りRisk Gate、Detector、Parquet書き込み | ✅ |
| 9-10 | 統合、Testnet接続、Mainnet観測開始 | ✅ |
| 11-12 | データ分析、市場ランキング作成 | 🟡 進行中 |

**成果物**:
- トリガー条件成立回数（MarketKey別）
- edge分布（手数料込みでEV正か）
- Oracle stale率、spread shock率
- EV見込みのある市場ランキング

### Phase B（超小口IOC実弾）- Week 13-16

**目的**: 滑り/手数料込みの実効EVを測定

| タスク | 状態 |
|--------|------|
| hip3-executor実装（IOC発注、cloid冪等性） | ✅ |
| hip3-position実装（PositionTracker、TimeStop） | ✅ |
| 署名機能（EIP-712 PhantomAgent） | ✅ |
| セキュリティ/鍵管理（KeyManager） | ✅ |
| Risk Gates（MaxPosition、HardStop） | ✅ |
| WsSender統合（送信trait、MockWsSender） | ✅ |
| コードレビュー対応（3回、全指摘修正） | ✅ |
| Safety Fixes（pending二重計上、notional上限追従） | ✅ |
| Mainnet少額テスト（xyz:NVDA, $20上限） | 🟡 実行中 |
| BUG-001: subscriptionResponse ACKパース修正 | ✅ 完了 |
| BUG-002: orderUpdates 配列形式対応 | ✅ 完了 |
| BUG-003: Signature r/s 0x prefix追加 | ✅ 完了 |
| BUG-004: 価格/サイズ精度制限適用 | ✅ 完了 |
| BUG-005: Mark price fail closed | ✅ 完了 |
| BUG-008: SpecCache 初期化 | ✅ 完了 |
| BUG-011: xyz perp asset ID修正 | ✅ 完了 |
| BUG-009: Signature v 文字列型 | 🔴 計画DRAFT |
| BUG-010: WS POST JSON形式修正 | 🔴 計画DRAFT |
| シグナル発生・注文送信確認 | - Next（BUG-009/010修正後） |

**パラメータ**:
- `SIZE_ALPHA = 0.05`（Phase Aの半分）
- `MAX_NOTIONAL_PER_MARKET = $100`（超保守的）

**成果物**:
- 実効スリッページ（expected vs actual）
- fill率（accepted/rejected/timeout）
- フラット化品質（flat_time_ms）

**Phase C移行条件**:
- 100回以上の実弾トレード完了
- 手数料+滑り込みでedge正を確認
- 重大な停止漏れなし

### Phase C（停止品質改善）- Week 17-20

**目的**: テール対策強化、例外ケースの網羅

| タスク |
|--------|
| OI cap検知強化（`perpsAtOpenInterestCap` polling） |
| DEX status監視（`perpDexStatus` 定期取得） |
| ParamChange検知精度向上 |
| SpreadShock Gate閾値Adaptive化 |
| 異常検知時Graceful Degradation |
| アラート設定（Slack/Discord通知） |

**Phase D移行条件**:
- 1000回以上の実弾トレード完了
- テール損失が想定内に収まる
- Gate誤検知率が許容範囲

### Phase D（市場自動入替）- Week 21-24

**目的**: 運用自動化、スケール

| タスク |
|--------|
| 市場ランキング自動計算（rolling統計） |
| 上位N市場のみ稼働（動的切り替え） |
| ブラックリスト管理 |
| 資金配分自動調整 |
| 複数MarketKey並行運用 |
| 設定変更ホットリロード |

---

## 9. マイルストーン

| 日付 | マイルストーン | 判定基準 | 状態 |
|------|---------------|----------|------|
| 2026-01-18 | Phase A 観測開始 | Mainnet WS接続成功 | ✅ |
| 2026-01-18〜19 | 15h連続稼働テスト | WS自律復旧確認 | ✅ |
| 2026-01-19 | Phase A 分析完了 | 178,637シグナル分析、6市場で高EV確認 | ✅ |
| 2026-01-19〜21 | Phase B Executor実装 | NonceManager/Signer/Executor/Position完了 | ✅ |
| 2026-01-21 | Phase B コードレビュー完了 | 3回のレビュー、全指摘修正、82テスト通過 | ✅ |
| 2026-01-22 | Phase B WsSender統合完了 | hip3-ws拡張、RealWsSender、125テスト追加 | ✅ |
| 2026-01-22 | Phase B Safety Fixes | pending二重計上修正、notional上限追従、46テスト通過 | ✅ |
| 2026-01-22 | Phase B Mainnet少額テスト開始 | xyz:NVDA, max_notional=$20, Trading mode稼働 | 🟡 実行中 |
| 2026-01-24 | WS ACK/配列形式バグ発見・修正 | BUG-001/002 実装完了、10+テスト追加 | ✅ |
| 2026-01-24 | BUG-003〜005, 008修正 | 0x prefix、価格精度、fail closed、SpecCache | ✅ |
| 2026-01-25 | xyz perp asset ID修正 | meta(dex=xyz) APIから正しいindex取得 | ✅ |
| 2026-01-25 | Mainnet注文成功確認 | xyz:SILVER 0.2 @ $104.63 約定 | ✅ |
| 2026-01-25 | 署名形式バグ発見 | v フィールド数値型（BUG-009）、WS形式（BUG-010） | 🔴 修正待ち |
| TBD | BUG-009/010 修正完了 | v 文字列化 + WS形式修正 | - Next |
| TBD | シグナル発生・Fill確認 | 米国市場時間帯でシグナル検証 | - |
| TBD | Phase B完了 | 100トレード + edge残存確認 | - |
| TBD | Phase C開始 | Phase B移行条件達成 | - |
| TBD | Phase D開始 | テール損失許容内 | - |

---

## 10. 撤退基準

| Phase | 条件 | アクション |
|-------|------|-----------|
| A | 12週間観測してEV正の市場がゼロ | 戦略見直し or 撤退 |
| B | 100トレードでedge負 | Phase Aに戻り閾値見直し |
| C | テール損失が資金の10%超 | 運用停止・Gate見直し |

---

## 11. Known Issues / Open Bugs

現在の未解決事項（リンク付き）:

| ID | 概要 | 影響 | 対応状況 |
|----|------|------|----------|
| BUG-009 | Signature v フィールドが数値型 | High: JSONパースエラー | 計画DRAFT |
| BUG-010 | WS POST JSON形式不正 | High: vaultAddress必須の可能性 | 計画DRAFT |
| BUG-006 | WS shutdown pathがtask終了しない | Medium: graceful shutdown不可 | 計画DRAFT |
| BUG-007 | orderUpdates statusマッピング不完全 | Medium: pending残留リスク | 計画DRAFT |

### 解決済み

| ID | 概要 | 修正日 |
|----|------|--------|
| ~~BUG-001~~ | subscriptionResponse ACKパース仕様ズレ | ✅ 2026-01-24 |
| ~~BUG-002~~ | orderUpdates 配列形式非対応 | ✅ 2026-01-24 |
| ~~BUG-003~~ | Signature r/s に 0x prefix なし | ✅ 2026-01-24 |
| ~~BUG-004~~ | 価格/サイズ精度制限未適用 | ✅ 2026-01-24 |
| ~~BUG-005~~ | Mark price欠損時Gate Fail Open | ✅ 2026-01-24 |
| ~~BUG-008~~ | SpecCache 初期化されていない | ✅ 2026-01-24 |
| ~~BUG-011~~ | xyz perp asset ID計算誤り | ✅ 2026-01-25 |

### BUG-009: Signature v フィールドが数値型 (HIGH)

**発見日**: 2026-01-25
**計画**: `.claude/plans/2026-01-25-signature-v-string-type.md`

**問題**:
- `v: u8` が数値（`28`）としてシリアライズされる
- Hyperliquid API は文字列（`"28"`）を期待
- JSONパースエラーで全注文が失敗

### BUG-010: WS POST JSON形式不正 (HIGH)

**発見日**: 2026-01-25
**計画**: `.claude/plans/2026-01-25-ws-post-json-format-fix.md`

**問題**:
- `vaultAddress` フィールドがNone時に省略されているが、WSでは必須の可能性
- REST APIとWebSocket POSTで要件が異なる可能性
- 実測で確認が必要

### BUG-006/007: WS shutdown / Status mapping

**発見日**: 2026-01-24
**計画**: `.claude/plans/2026-01-24-review-findings-fix.md` (F2, F3)

**過去の解決済みバグ**: `.claude/specs/2026-01-19-24h-test-bugfix.md` 参照

---

## 12. 参照ドキュメント

| ドキュメント | パス | 内容 |
|-------------|------|------|
| 実装計画（メイン） | `.claude/plans/2026-01-18-oracle-dislocation-taker.md` | 戦略定義、非交渉ライン（全38項目）、P0/P1タスク |
| 実装Spec | `.claude/specs/2026-01-18-oracle-dislocation-taker.md` | 実装進捗追跡 |
| Phase A レビュー修正 | `.claude/plans/2026-01-19-phase-a-review-fixes.md` | P0修正項目 |
| 24hテストBugfix | `.claude/specs/2026-01-19-24h-test-bugfix.md` | バグ修正完了記録 |
| **Phase A 分析レポート** | `.claude/specs/2026-01-19-phase-a-analysis.md` | 178,637シグナル分析、EV正市場特定 |
| VPSテスト継続Spec | `.claude/specs/2026-01-19-vps-test-continuation.md` | VPSデプロイ、JSON Lines移行、機能追加記録 |
| **Followup機能Spec** | `.claude/specs/2026-01-20-followup-snapshot-feature.md` | シグナル検証用T+1s/T+3s/T+5sキャプチャ |
| **Phase B 実装計画** | `.claude/plans/2026-01-19-phase-b-executor-implementation.md` | Executor/Position/Risk全体設計 |
| **Phase B 実装Spec** | `.claude/specs/2026-01-19-phase-b-executor-implementation.md` | 実装進捗追跡（完了） |
| **WsSender統合計画** | `.claude/plans/ethereal-sauteeing-galaxy.md` | hip3-ws拡張、RealWsSender設計 |
| **Mainnet少額テストSpec** | `.claude/specs/2026-01-22-mainnet-micro-test.md` | xyz:NVDA、$20上限、Safety Fixes記録 |
| **subscriptionResponse ACK修正計画** | `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md` | ACKパース仕様ズレ修正計画 |
| **subscriptionResponse ACK修正Spec** | `.claude/specs/2026-01-24-subscriptionResponse-ack-fix.md` | 実装完了（BUG-001） |
| **orderUpdates配列形式対応計画** | `.claude/plans/2026-01-24-orderUpdates-array-format-fix.md` | WsOrder[]配列形式対応計画 |
| **orderUpdates配列形式対応Spec** | `.claude/specs/2026-01-24-orderUpdates-array-format-fix.md` | 実装完了（BUG-002） |
| **Mainnetテスト失敗修正計画** | `.claude/plans/2026-01-24-mainnet-test-failure-fix.md` | Signature prefix、価格精度修正（BUG-003/004）- **実装完了** |
| **レビュー指摘修正計画** | `.claude/plans/2026-01-24-review-findings-fix.md` | Mark price、WS shutdown、status mapping（BUG-005/006/007） |
| **SpecCache初期化修正計画** | `.claude/plans/2026-01-24-speccache-initialization-fix.md` | SpecCache populate（BUG-008）- **実装完了** |
| **xyz perp asset ID修正Spec** | `.claude/specs/2026-01-25-xyz-perp-asset-id-fix.md` | meta(dex=xyz) API使用（BUG-011）- **実装完了** |
| **Signature v 文字列型計画** | `.claude/plans/2026-01-25-signature-v-string-type.md` | v フィールド型修正（BUG-009）- DRAFT |
| **WS POST JSON形式修正計画** | `.claude/plans/2026-01-25-ws-post-json-format-fix.md` | vaultAddress等（BUG-010）- DRAFT |
| Phase B コードレビュー | `review/2026-01-21-phase-b-*.md` | 3回のレビュー記録、全指摘修正 |
| 着手判断メモ | `review.md` | Go条件、Phase A DoD |

---

## 13. テスト状況

| 項目 | 結果 |
|------|------|
| `cargo test --workspace` | ✅ 378 passed (2026-01-25) |
| `cargo clippy -- -D warnings` | ✅ 0 warnings |
| `cargo check` | ✅ Pass |
| Testnet接続 | ✅ 検証済み |
| Mainnet観測 | ✅ 15h稼働完了、178,637シグナル取得 |
| Mainnet注文 | ✅ xyz:SILVER 0.2 @ $104.63 約定確認 (2026-01-25) |

---

## 更新履歴

| 日付 | 内容 |
|------|------|
| 2026-01-19 | 初版作成 |
| 2026-01-19 | レビュー反映: P0-19重複修正、テスト数更新(130)、参照整合情報追加、Known Issues追加、DoD確認方法追記、非交渉ライン「抜粋」明示 |
| 2026-01-19 | Phase A分析完了: 178,637シグナル分析、6市場で高EV確認（HOOD, MSTR, NVDA, COIN, CRCL, SNDK）、Phase B準備開始 |
| 2026-01-20 | Parquet→JSON Lines移行: 破損対策として堅牢なファイル形式に変更 |
| 2026-01-20 | best_sizeフィールド追加: トップオブブック深度をSignalRecordに記録 |
| 2026-01-20 | **Followup Snapshot機能追加**: T+1s/T+3s/T+5sでシグナル検証データをキャプチャ、Phase A+として検証中 |
| 2026-01-21 | **Phase B Executor実装完了**: NonceManager、Signer (EIP-712)、BatchScheduler、Executor、PositionTracker、TimeStop、HardStopLatch、MaxPosition Gates、WsSender統合。コードレビュー3回完了、全指摘修正。212+テスト通過。 |
| 2026-01-22 | **WsSender統合完了**: hip3-ws拡張（WsWriteHandle、PostRequest/Response、orderUpdates購読）、RealWsSender実装、39+86テスト追加。全8フェーズ完了、Testnet検証準備完了。 |
| 2026-01-22 | **Safety Fixes**: PositionTracker pending二重計上修正（register_order_actor_only() API追加）、ExecutorConfig notional上限追従（detector.max_notional=$20に同期）、46テスト通過。 |
| 2026-01-22 | **Mainnet少額テスト開始**: xyz:NVDA (index 23)、max_notional=$20、Trading mode稼働中。米国市場時間帯のシグナル発生待ち。 |
| 2026-01-24 | **WS仕様バグ発見**: subscriptionResponse ACKパース仕様ズレ（BUG-001）、orderUpdates配列形式非対応（BUG-002）を発見。計画作成・レビュー完了。 |
| 2026-01-24 | **BUG-001/002 実装完了**: `extract_subscription_type()`, `is_order_updates_channel()`, `process_subscription_response()`, `OrderUpdatesResult`, `as_order_updates()` 実装済み。10+テスト追加。 |
| 2026-01-24 | **追加バグ発見**: Mainnetテスト失敗分析からBUG-003〜008を発見。Signature 0x prefix（CRITICAL）、価格精度（HIGH）、Mark price Gate（HIGH）、SpecCache初期化。 |
| 2026-01-24 | **BUG-003〜005, 008 修正完了**: 0x prefix追加、format_price/size適用、MarketDataUnavailable追加、fetch_dex_meta_indices実装。 |
| 2026-01-25 | **BUG-011: xyz perp asset ID修正**: perpDexsとmeta(dex=xyz)の順序差異を発見。asset_indexフィールド追加、正しいID計算を実装。 |
| 2026-01-25 | **Mainnet初約定成功**: xyz:SILVER 0.2 @ $104.63。正しいasset IDで注文送信・約定を確認。 |
| 2026-01-25 | **BUG-009/010 発見**: v フィールド数値型問題、WS POST形式問題。計画作成中。2テスト失敗（preflight tests要修正）。 |
