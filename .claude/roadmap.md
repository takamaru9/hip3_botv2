# hip3_botv2 ロードマップ

**作成日**: 2026-01-19
**最終更新**: 2026-01-19
**プロジェクト**: Oracle/Mark Dislocation Taker Bot (HIP-3 / xyz限定)

---

## 参照整合情報

| 項目 | 値 |
|------|-----|
| **対象コミット** | `5ff3b13` |
| **検証実行日** | 2026-01-19 |
| **検証環境** | macOS (Darwin 25.0.0) / Rust 1.x |
| **日次出力先** | `data/daily/YYYY-MM-DD.parquet` |
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
    ├── hip3-executor   # IOC執行（Phase B以降）[skeleton]
    ├── hip3-position   # ポジション管理（Phase B以降）[skeleton]
    ├── hip3-telemetry  # Prometheus、構造化ログ、日次統計
    ├── hip3-persistence # Parquet保存（signals, events）
    └── hip3-bot        # メインアプリケーション統合
```

---

## 2. Phase区分と状態

| Phase | 目的 | 期間 | 状態 | 進捗 |
|-------|------|------|------|------|
| **Phase A** | 観測・EV市場特定 | Week 1-12 | **完了**（分析済み） | 100% |
| **Phase B** | 超小口IOC実弾 | Week 13-16 | **準備開始** | 5% |
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
| **日次出力指標** | ✅ 完了 | 対象銘柄ごとに日次で以下が出力される | ファイル: `data/mainnet/signals/*.parquet` 存在確認 |

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

### Phase B 関連（未着手）

| ID | タスク | 状態 | 詳細 |
|----|--------|------|------|
| P0-11 | セキュリティ/鍵管理 | ⏳ | API wallet分離、ローテーション手順 |
| P0-19a | NonceManager (0起点禁止) | ⏳ | now_unix_msで初期化 |
| P0-19b | Batching (100ms周期) | ⏳ | IOC/GTC と ALO 分離 |
| P0-25 | NonceManager serverTime同期 | ⏳ | ドリフト監視 |
| P0-29 | ActionBudget制御アルゴリズム | ⏳ | 優先度付きキュー + token bucket |

---

## 6. Phase B タスク詳細

### hip3-executor 実装

```
hip3-executor/
├── nonce.rs          # NonceManager（0起点禁止、serverTime同期）
├── batch.rs          # BatchConfig（100ms周期、IOC/GTC vs ALO分離）
├── order.rs          # IOC発注、cloid冪等性
├── key.rs            # KeyManager（権限分離）
└── budget.rs         # ActionBudget（優先度キュー、token bucket）
```

### NonceManager 設計

- **初期化**: `now_unix_ms` へ fast-forward（0起点禁止）
- **serverTime同期**: `webData3.userState.serverTime` でドリフト監視
- **制約遵守**: (T-2d, T+1d) 範囲 + 最大100個の高nonce保持

### ActionBudget 優先順位

| 優先度 | 送信種別 | 理由 |
|--------|----------|------|
| **1 (最優先)** | ReducePosition/Close | テール損失回避 |
| **2** | Cancel | 意図しない約定回避 |
| **3 (最抑制)** | NewOrder | 機会損失より事故回避 |

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
| hip3-executor実装（IOC発注、cloid冪等性） | ⏳ |
| hip3-position実装（PositionTracker、TimeStop） | ⏳ |
| 署名機能（Exchange endpoint用） | ⏳ |
| セキュリティ/鍵管理（P0-11） | ⏳ |
| Testnet実弾テスト | - |
| Mainnet超小口テスト（$10-50/注文） | - |

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
| 2026-01-19 | Phase B 準備開始 | executor/nonce/鍵管理着手 | 🟡 |
| TBD | Phase B Testnet実弾 | Testnetで10トレード成功 | - |
| TBD | Phase B Mainnet開始 | COIN (xyz:5) で超小口テスト | - |
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
| - | （現時点で未解決バグなし） | - | - |

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
| 着手判断メモ | `review.md` | Go条件、Phase A DoD |

---

## 13. テスト状況

| 項目 | 結果 |
|------|------|
| `cargo test --workspace` | ✅ 130 tests passed |
| `cargo clippy -- -D warnings` | ✅ 0 warnings |
| `cargo check` | ✅ Pass |
| Testnet接続 | ✅ 検証済み |
| Mainnet観測 | ✅ 15h稼働完了、178,637シグナル取得 |

---

## 更新履歴

| 日付 | 内容 |
|------|------|
| 2026-01-19 | 初版作成 |
| 2026-01-19 | レビュー反映: P0-19重複修正、テスト数更新(130)、参照整合情報追加、Known Issues追加、DoD確認方法追記、非交渉ライン「抜粋」明示 |
| 2026-01-19 | Phase A分析完了: 178,637シグナル分析、6市場で高EV確認（HOOD, MSTR, NVDA, COIN, CRCL, SNDK）、Phase B準備開始 |
