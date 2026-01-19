

# Hyperliquid の HIP-3（Builder-deployed perpetuals）市場メモ

最終更新: 2026-01-18

## 1. HIP-3 とは（何が「市場」なのか）

HIP-3 は Hyperliquid 上で **パーミッションレスに Perp（永久先物）市場をデプロイできる仕組み**（Builder-deployed perpetuals）です。Hyperliquid の HyperCore（高速な板・マージン・清算エンジン）上に、第三者が「自分の Perp DEX（= perp dex）」を立ち上げ、その中に新しい Perp 銘柄（asset）を追加できます。

公式ドキュメント上の要点:
- デプロイヤー（市場運営者）は **(1) 市場定義（オラクル定義・契約仕様）** と **(2) 市場運用（オラクル価格更新・レバレッジ設定・必要時の清算/決済）** を担う。これは HIP-3 の中核的な特徴で、通常の「バリデータ運営 Perp」に比べて運営主体の裁量が大きい。 

## 2. 重要な仕様（Spec）

### 2.1 ステーク要件
- **メインネットのステーク要件は 500,000 HYPE**。
- 直近の要件を超える分はアンステーク可能。
- すべてのデプロイヤー Perp が停止（halt）された後も **30日間** は要件が維持される。

### 2.2 「perp dex」とは
- ステーク要件を満たすデプロイヤーは **1つの perp dex** をデプロイ可能。
- 各 perp dex は **独立したマージン、板（order books）、デプロイヤー設定** を持つ。

### 2.3 コラテラル（担保）/ クオート資産
- perp dex の担保（collateral）として **任意のクオート資産** を選べる。
- クオート資産が「permissionless quote asset」要件を満たさなくなった場合、オンチェーンのバリデータ投票でクオート資産として無効化され、当該資産を担保にする perp dex も無効化され得る。
- 一方で、公式文脈では「クオート資産に関するスラッシングは HIP-3 デプロイヤーには適用されない」とされており、主にプロダクト/手数料設計上の重要要素である（致命的リスクとしては扱われていない、という立て付け）。

### 2.4 新規 asset の追加（オークション）
- 各 perp dex の **最初の3 asset** はオークション不要。
- それ以降の asset は HIP-1 と同様のハイパーパラメータを持つ **Dutch auction** を通過する。
- HIP-3 の追加 Perp オークションは、全 perp dex で共有される。

### 2.5 マージンモード
- **Isolated-only（分離証拠金のみ）** が必須。
- Cross margin は将来アップグレードで対応予定。

## 3. 手数料（Fees）と Growth Mode

### 3.1 HIP-3 の基本フィー構造
- 公式仕様では、デプロイヤー側の取り分（fee share）は **50%で固定**。
- ユーザー側の手数料は、バリデータ運営 Perp に比べて **ベースで 2倍**（ただし、ステーキング割引・紹介等の通常のディスカウントは同様に適用される）。
- ネット効果として、プロトコル側の取り分は「HIP-3 かバリデータ運営か」で大きく変わらないよう設計されている。

### 3.2 Growth Mode（主に新市場の流動性立ち上げ用途）
- HIP-3 Perp で growth mode を有効化すると、**protocol fees / rebates / volume contribution / L1 user rate limit contribution が 90% 低下**する。
- HIP-3 デプロイヤーは追加の fee share を設定でき、通常は **0–300%**、growth mode 時は **0–100%** の範囲。
- share が 100% を超える場合、protocol fee も deployer fee に合わせて増える（結果としてユーザー支払コストが増える方向）。

注: 実際のティア（14d volume ベースの maker/taker）や割引（aligned collateral 等）は公式 Fees 表を参照。

## 4. トレーダー視点のチェックポイント（HIP-3 特有の実務）

HIP-3 は「板のエンジンは HyperCore だが、運用はデプロイヤー」という構造です。したがって、同じ Hyperliquid 上でも **市場ごとの運用リスク差** が出ます。

### 4.1 オラクル運用リスク
- HIP-3 はデプロイヤーがオラクル価格更新・レバレッジ制限・必要時の決済を担う。
- オラクル更新が遅い/不適切だと、マーク/清算価格に影響し得る。
- 一般論として「外部価格を先に見られる参加者が stale price を抜く」形のアービトラージが成立しうるため、薄商い HIP-3 は特に注意。

### 4.2 流動性・スプレッド
- HIP-3 は新規銘柄が出やすい一方で、初期は流動性が薄くスプレッドが大きくなりやすい。
- 約定/建玉のサイズを抑える、指値中心、レバレッジを下げるなど、運用側で調整する。

### 4.3 OI cap（建玉上限）
- Hyperliquid にはオラクル操作対策として open interest cap（建玉上限）があり、到達すると新規ポジションが取れなくなる等の制限が入る。
- HIP-3 各 dex/asset でも同様に cap が存在し、デプロイヤーが per-asset cap を設定できる。

## 5. 開発者視点（Bot 実装で深掘り）

ここでは「トレーダー Bot（HIP-3 市場で売買する）」を中心に、必要に応じて「デプロイヤー Bot（自分が perp dex を運営する）」も触れます。

### 5.1 HIP-3 は “market” が二重構造（DEX と asset）

実装上の最重要ポイントは、HIP-3 が
- **perp dex（= DEX 単位の証拠金・板・設定の境界）**
- **asset（= DEX の中の個別 Perp 銘柄）**

の二重構造である点です。

Bot の内部表現は、最低でも次の3つを分離して持つのが安全です。

- `DexId`（HIP-3 dex を一意に識別）
- `AssetId`（perp asset を一意に識別。通常 perps と HIP-3 perps で namespace が異なることがある）
- `MarketKey = (DexId, AssetId)`（売買・ポジション・板の最小単位）

推奨: 文字列（例: `HIP3:<dex>:<asset>`）に正規化してログ・DB・メトリクスのキーに使う。

### 5.2 市場列挙（Discovery）とメタデータ同期

HIP-3 は銘柄の追加が頻繁に起こり得るため、**起動時だけでなく定期的に market universe を同期**する設計が必要です。

推奨フロー:
1. `universe`（市場一覧）を取得して、HIP-3 DEX と asset を抽出
2. 各 `MarketKey` について、取引仕様（tick size / lot size / max leverage / fee schedule / OI cap 等）をキャッシュ
3. キャッシュ更新のたびに「仕様差分」をログに出す（運用上の事故予防）

設計上の注意:
- **“銘柄名（symbol）” は表示名に過ぎない**（衝突・変更の可能性）。Bot は `MarketKey` を真の識別子にする。
- `tick/lot` が変わると注文の丸め（quantization）が壊れるため、更新検知を必須にする。

### 5.3 価格・板・約定の取り方（Poll vs WS）

HIP-3 は薄商い/スプレッド拡大が多く、**板の変化が疎**になりがちです。したがって、Bot は以下のいずれかを選びます。

- **WebSocket（推奨）**: best bid/ask、L2 book、trades をサブスクライブし、状態をインメモリで維持
- **REST poll**: 低頻度でも成立する戦略（例: funding/arbitrage/mean-revert ではなく、イベント駆動でない運用）

共通の要点:
- 板が薄いほど、**約定後の adverse selection** と **スリッページ** が支配的。板の厚み（top-of-book の数量、数段の depth）を必ず特徴量に入れる。
- 「オラクル更新遅延」を疑うなら、
  - `mark price` と `mid price` の乖離、
  - `funding` の急変、
  - 直近 trade の価格帯
  を監視し、乖離時はレバレッジ/サイズを自動で落とす。

### 5.4 注文生成（Quantization / IOC / Post-only）

HIP-3 の注文事故の多くは、**丸め規則**と**注文タイプ**の組み合わせで起きます。

推奨ルール:
- 価格: `price = round_to_tick(px, tick_size)`
- 数量: `size = floor_to_lot(qty, lot_size)`
- `size == 0` なら注文を出さない（例外扱い）

注文タイプの使い分け:
- **maker（post-only）中心**: 薄商いでは taker だとコストが極端に悪化しやすい
- **IOC/market は非常時のみ**: ストップ/清算回避など、明確な優先順位がある場合だけ

キャンセル・置換:
- “毎秒 cancel/replace” は不要なことが多い。HIP-3 では板が疎なため、
  - オラクル更新（または指数/現物の変化）
  - スプレッドが閾値を超えた
  - 自分の指値が best から外れた
  といったイベントで更新するほうがコスト・レート制限両面で安定する。

### 5.5 ポジション管理（Isolated-only を前提にする）

HIP-3 は isolated-only のため、Bot は **DEX ごと**、さらに可能なら **MarketKey ごと**にリスクを閉じる設計が自然です。

推奨データモデル:
- `DexAccountState`: dex 単位の available margin / maintenance margin / unrealized PnL / liquidation buffer
- `PositionState[MarketKey]`: size / entry / mark / funding accrual / realized PnL

リスク制御（最低限）:
- `max_notional_per_market`
- `max_leverage_effective`
- `min_liquidation_buffer_pct`
- `halt_on_oracle_staleness`

### 5.6 担保移動（DEX abstraction）と資金繰り

HIP-3 の運用で地味に効くのが「担保の所在」です。

- DEX abstraction が **無効**: その dex に対して事前に担保を入れておかないと、注文が通らない/想定サイズが出せない。
- DEX abstraction が **有効**: 取引アクションが、条件に応じて「validator-operated USDC perps balance（または spot）」から担保移動を伴う。

Bot 観点の設計:
- `CollateralManager` を独立コンポーネントにする。
- 取引前に `DexAccountState.available_margin` を見て、
  - 不足なら入金（transfer）
  - 余剰なら回収（withdraw）
  を行う。

事故パターン:
- 複数 dex を同時に回していると、担保移動がボトルネックになり、約定機会を逃す。
- abstraction を ON にしたつもりが OFF で、注文が連続 reject になる。

→ したがって、起動時に「abstraction 設定状態」を明示ログし、稼働中も定期的に確認する。

### 5.7 レート制限・再送・冪等性

HIP-3 で銘柄数を増やすほど、
- 板サブスクライブ
- 状態取得
- cancel/replace

が増え、レート制限が先に壊れます。

推奨:
- 注文系は **冪等キー（client order id）** を必ず付与し、再送時に重複発注しない。
- “cancel all” を濫用しない。市場単位のキャンセルと差分置換を優先。
- 例外時は「指数バックオフ + jitter」を実装。

### 5.8 OI cap / ハルト / 仕様変更の検知

HIP-3 はデプロイヤー運用であるため、Bot は **仕様変更を“異常”として扱う**必要があります。

監視すべきイベント:
- OI cap 到達（新規建て不可）
- tick/lot/leverage/fee の変更
- ハルト（取引停止）
- オラクル更新停止や mark の不連続

対応ポリシー（例）:
- OI cap 到達: 新規建てを止め、既存ポジのみ縮小/クローズに限定
- tick/lot 変更: 直ちに全指値キャンセル → 新仕様で再クォート
- ハルト: 全注文キャンセル → ポジション縮小フローに移行（可能なら）

### 5.9 推奨アーキテクチャ（hip3_botv2 を想定）

コンポーネント分割（最小）:
- `MarketRegistry`（universe 同期、tick/lot/leverage/fee キャッシュ）
- `Feed`（WS/REST、L2・trades・mark・funding を集約）
- `SignalEngine`（戦略ロジック）
- `Execution`（発注、cancel/replace、冪等性）
- `PositionManager`（平均単価、PnL、リスク、縮小ロジック）
- `CollateralManager`（dex ごとの入出金、abstraction 状態）
- `RiskController`（ゲート/ブレーカー/サイズ上限）
- `Persistence`（SQLite/ClickHouse 等へ MarketKey 単位で保存）

ログ/メトリクス（運用で効く）:
- `reject_rate_by_reason`（丸め/不足/ハルト/レート制限）
- `effective_spread_paid`（実効スプレッドコスト）
- `oracle_mark_mid_divergence`（乖離監視）
- `inventory_and_liq_buffer`（在庫と清算バッファ）

### 5.10 デプロイヤー Bot（自分で HIP-3 DEX を運営する場合）

トレーダー Bot と比べ、追加で必要になるのは以下です。

- `OraclePublisher`: 外部価格取得 → 署名して onchain action を投げる
- `ParamGovernor`: leverage、OI cap、（必要なら）growth mode や fee share の運用
- `IncidentResponse`: オラクル停止・価格異常・ハルト時の手順自動化

実務上は「オラクルの品質」が全てです。
- 取得元の多重化（複数 CEX/DEX 参照）
- 欠損・スパイク除外（median/trimmed mean）
- タイムスタンプ監査（stale を絶対に出さない）

が最低条件になります。

## 6. 関連: Hyperps（HIP-3 とは別概念）


Hyperps は「Hyperliquid-only perps」で、外部スポット/インデックスのオラクルを必須とせず、ハイパー独自の EMA ベースの参照価格で funding を決める設計。
- 目的: pre-launch 先物のように外部参照が弱い局面でも、価格操作耐性を上げる。
- HIP-3（builder-deployed）と混同しないこと。


## 7. xyz（UNIT）エアドロ狙い：HIP-3 銘柄での自動売買アプローチ（案）

前提：エアドロ配賦基準が完全に公開されていないケースを想定し、「出来高/継続稼働」を満たしつつ **コスト最小 + テールリスク最小** を優先する。

### 7.1 目的関数（最初に固定する）
- 最大化：`Expected Airdrop Value / (取引コスト + テールリスク + 運用負荷)`
- HIP-3 は「板・清算は HyperCore、運用はデプロイヤー」という構造のため、市場ごとの運用リスク差（オラクル更新、パラメータ変更、ハルト等）が期待値を大きく左右する。

### 7.2 対象銘柄の選び方（出来高 Top5 を“動的”に回す）
- 銘柄固定ではなく、**定期的に xyz DEX 内の出来高 Top5 を取り直す**（日次〜数時間ごと）。
- 実装イメージ：
  - DEX 一覧（perpDexs）→ xyz DEX を特定 → meta/ctx 系の取得で各 asset の指標を同期 → 出来高でソート → 上位5つを採用
- 仕様変更（tick/lot/leverage/fee）や asset 追加が起こり得るため、MarketRegistry で差分検知し、事故を予防する。

### 7.3 戦略の本命：低コストのメイカー型 Market Making（post-only）
- 方針：**post-only の両建て指値**で「薄く長く」約定させる（方向当てはしない）。
- 具体：
  - 1段目：best bid/ask 近傍（約定率を確保）
  - 2段目：外側（急変時の吸収、在庫解消の保険）
- 在庫制御（inventory skew）：
  - 目標在庫=0
  - ロングが増えたら bid を下げ/ask を上げて自然に解消（ショートも同様）
- 更新は毎秒 cancel/replace ではなく **イベント駆動**（mid の変化、best からの乖離、スプレッド拡大、在庫偏り、オラクル異常など）に寄せる。

### 7.4 Growth Mode の扱い（スコア不確実性へのヘッジ）
- Growth mode は **all-in fees を大幅に低下**させる一方で、**rebates と volume-contribution（クレジット）が大幅に低下**するため、採点方式次第で「スコア効率」が変わり得る。
- 現実的な運用方針（どちらか）：
  - 方針A（コスト最優先）：Growth mode 対象銘柄だけで Top5 を回す（対象外は次点を採用）
  - 方針B（不確実性ヘッジ）：出来高の 80–90% は Growth mode、10–20% は対象外にも振る（スコア方式が volume-contribution 寄りだった場合の保険）

### 7.5 $10K 規模の資金配分（安定稼働優先）
- 推奨：Top5 同時運用を前提に、
  - 運用証拠金：$7,500（= $1,500 × 5 markets）
  - 予備バッファ：$2,500（追証・偏り吸収・担保移動の遅延対策）
- 実効レバレッジ：1–3x を基本（薄板・オラクル運用差でテールが出やすい）。
- ガードレール（例）：
  - `max_notional_per_market`：$3,000–$5,000（流動性に応じて調整）
  - `min_liq_buffer_pct`：30–40%（割ったら新規約定を抑制/停止）
  - `max_inventory_skew`：top-of-book depth の数倍程度まで（過大在庫を避ける）

### 7.6 HIP-3 特有の停止条件（ブレーカー）が最重要
- Oracle/Mark 異常：`|mark - mid| / mid` が閾値超えで継続 → 新規停止 + 全キャンセル
- 薄板：top-of-book depth が閾値以下 → サイズを 1/2–1/5 に縮小（さらに悪化で停止）
- スプレッド拡大：平常時の 3–5倍 → 外側に逃がす/停止
- 仕様変更：tick/lot/leverage/fee 変更検知 → 全キャンセル → 新仕様で再開
- ハルト：検知次第、全キャンセル（可能ならポジ整理）


### 7.7 実装の非交渉ライン（安定稼働のための要件）
- WebSocket 常時接続 + 自動復旧（板/約定/状態）
- client order id による冪等性（再送で二重発注しない）
- レート制限前提のイベント駆動更新（不要な cancel/replace を減らす）
- メトリクス：reject reason、fill率、在庫、実効スプレッド、mark-mid 乖離、ダウンタイム


### 7.8 ヘッジ無しで利益を狙う勝ち筋（HIP-3 前提）

前提：日本の既存金融（先物/CFD/FX 等）で DEX 並の速度で常時ヘッジを回すのは現実的ではない。
したがって外部ヘッジ前提（デルタ中立）ではなく、**在庫（inventory）を構造的に増やさない**設計で期待値を作る。

#### 勝ち筋 1：在庫を増やさない MM（擬似マーケットニュートラル）
- 目標在庫 = 0 を厳格に維持（方向性を持たない）。
- 約定で在庫が偏ったら「スプレッド取り」より **在庫解消を最優先**。
  - ロング過多：bid を下げる/引く、ask を寄せる（解消優先の skew）。
  - ショート過多：ask を上げる/引く、bid を寄せる。
- `max_inventory`（最大在庫）をハード制約にする。
  - 超過時は「両建て継続」ではなく、**片側のみ提示して解消モード**（または全停止）。

#### 勝ち筋 2：毒フロー回避（HIP-3 の“負け筋”を事前に遮断）
外部ヘッジがないほど、1回のテール損失が回収しにくい。従って **ブレーカーが利益**になる。

推奨の停止条件（発火で新規停止 + 全キャンセル）：
- `mark–mid` 乖離が閾値超えで継続
- スプレッド急拡大（平常時の 3–5倍 など）
- top-of-book の厚み低下（薄板化）
- 仕様変更（tick/lot/leverage/fee/OI cap）検知
- ハルト検知

#### 勝ち筋 3：イベント駆動の更新（cancel/replace を最小化）
HIP-3 は毎秒 cancel/replace を回すほど、レート制限・不要コスト・逆選択が悪化しやすい。
更新は以下のイベントに限定し、平常時は「置きっぱなし」に寄せる。
- mid の変化（一定幅以上）
- 自分の指値が best から外れた
- スプレッド/板厚が閾値を跨いだ
- 在庫が閾値を跨いだ
- `mark–mid` 乖離が閾値を跨いだ（停止含む）

#### 例外：同一暗号圏内の“低頻度ヘッジ”（B'）
「常時デルタ中立」ではなく「テールを切る」目的なら、ミリ秒級でなくても成立する場合がある。
- 在庫が閾値超えの時だけ、数秒〜十数秒に1回のリバランス
- 目的は常時中立ではなく、在庫片寄りの上限（テール）を抑える


#### 次に固定すべき最重要パラメータ（ヘッジ無し運用）
- `max_inventory`：どこまで在庫を許容するか（市場の板厚に比例させる）
- 在庫超過時の動作：解消モード（片側のみ）か、全停止か
- ブレーカー閾値：`mark–mid`、板厚、スプレッド倍率
- 対象市場の選定ルール：低毒・安定板を優先し、固定せず入れ替える


ここまではエアドロ狙いの参加者誰もが思いつくことであり，逆に言えば損してもいいと思っているルーズな参加者の思考である．
以下でそこから利益を狙う戦略を考えこちらを開発していく．

## 8. 逆張り発想：HIP-3 の「低コストMM勢」から利益を得る戦略（逆MM）

前提：HIP-3 では「板・清算は HyperCore だが、運用はデプロイヤー」という構造により、薄板・更新疎・仕様変更/ハルト等が起こりやすい。
この環境で低コストMMを継続する参加者は、構造的に
- stale（更新遅延）
- adverse selection（逆選択）
- inventory 偏り（在庫圧）
に弱く、ここを収益機会として狙う。


### 8.1 型2：Oracle/Mark Dislocation Trade（mark–mid–fair の歪みを収益化）

狙い：`mark`（清算/リスク基準）と `mid`（板中心）と `fair`（外部）の不整合が出た瞬間に「残っている古い指値」を小さく踏む。

典型パターン：
- 外部 `fair` が先に動く
- HIP-3 側の板更新が遅れて `mid` が古い
- `mark` も追随が遅い/不連続
- その間に古いbestが残る → そこを踏む

重要：テール（ハルト、仕様変更、OI cap、突然のレバ制限）に弱いので、以下をハード制約にする。
- `halt` / `param_change` 検知時は新規禁止
- `max_notional_per_market` を低く固定
- `min_liq_buffer_pct` を高く維持（isolated-only 前提）


### 8.2 ：Spread Shock Harvest（スプレッド急拡大時の置きっぱなし歪みを取る）

狙い：MMが一斉に「広げる/引く」瞬間に発生する流動性真空で、古い指値・遠い置きっぱなし指値が残ることがある。急拡大直後の短時間だけ、小さく確実に踏む。

- 指標：`spread_now / spread_baseline`（平常時の何倍に拡大したか）
- ルール：急拡大検知 → クールダウン時間を短く区切り、その間だけ実行

### 8.5 実装（最小コンポーネント）

- `FairValueEngine`: 外部価格の統合（median + 欠損/スパイク除外）
- `DislocationDetector`: `best vs fair` / `mark vs mid` / `spread_shock` / `inventory_skew` を判定
- `EdgeToOrderSizer`: edge と板厚からサイズ決定（lot/tick 丸め込み）
- `TakerExecutor`: IOC中心（冪等ID、部分約定許容、再送で二重発注しない）
- `HardRiskGate`（最重要）：halt/仕様変更/OI cap/異常乖離で即停止、isolated buffer 割れで新規禁止

### 8.6 非交渉ライン（戦略転換で最初に固定する）

逆MMは taker 寄りになりやすく、手数料とスリッページに負けると期待値が消える。したがって戦略ロジックより先に、以下を固定する。

1) **fee込み edge 閾値（銘柄ごと）**
2) **板厚連動サイズ（欲張らない）**
3) **停止条件（mark–mid、板厚、スプレッド倍率、halt、仕様変更、OI cap）**


### 8.7 外部有料データ無しでの「逆MM」：推奨は 8.2（主軸）+ 8.4（フィルタ）

8.1（外部フェア価格 vs 古いbest）は TradFi の market data 購読コストが支配的になりやすい。
したがって「Hyperliquid 内部で完結する参照価格」を用いて、同種の dislocation を狙う。

#### 結論（優先順位）
- **最優先：8.1 Oracle/Mark Dislocation Taker**（外部データ不要・再現性が高い）
- **安全装置：8.2 Spread Shock**（危険時間帯の新規を止める/サイズを落とすフィルタとして組み込む）

---

#### 8.1 を「実装仕様」まで落とす

**参照価格（fair の代替）**
- `oraclePx` と `markPx` を一次参照とする（入手できる範囲で `midPx` も併用）。
- 狙いは「板（best/mid）が参照価格に追随できていない瞬間に残る置きっぱなし注文」を小さく踏むこと。

**必要データ（HL 内で完結）**
- L2（best bid/ask、可能なら数段 depth）
- `oraclePx`, `markPx`, `midPx`
- 市場ステータス（halt 等）、仕様（tick/lot/leverage/fee/OI cap）

**主要特徴量（MarketKey ごと）**
- `mid = (best_bid + best_ask)/2`
- `spread_bps = (best_ask - best_bid)/mid * 1e4`
- `depth_top = bid_sz1 + ask_sz1`（または上位 N 段の合計）
- `d_oracle_bps = (mid - oraclePx)/oraclePx * 1e4`
- `d_mark_bps   = (mid - markPx)/markPx * 1e4`
- `mark_mid_gap_bps = (markPx - mid)/mid * 1e4`
- `oracle_age_ms`（取得できない場合は `oraclePx` の「最終変化時刻」で代替）

**エントリー条件（例：Buy / Sell は符号反転）**
- *前提ゲート*：`halt == false`、`param_change == false`、`OI cap` による新規禁止でない
- `oracle_age_ms <= ORACLE_FRESH_MS`
- `spread_bps <= MAX_SPREAD_BPS`、`depth_top >= MIN_DEPTH`
- *踏める価格*が参照を跨ぐ：
  - Buy：`best_ask <= oraclePx * (1 - (FEE_BPS + SLIP_BPS + EDGE_BPS)/1e4)`
  - Sell：`best_bid >= oraclePx * (1 + (FEE_BPS + SLIP_BPS + EDGE_BPS)/1e4)`
- **mid の乖離だけで入らない**（best が抜ける構造のときだけ実行）

**サイズ（欲張らない）**
- `size = min(alpha * top_of_book_size, max_notional_per_market / mid)`
- `alpha = 0.10–0.25` を初期値、必ず lot に `floor_to_lot`

**エグジット（保有を伸ばさない）**
- 原則：短期でフラット回帰（数秒〜十数秒）。
- `oraclePx` 近傍への指値で即時逃がす（または dislocation 解消で成行/IOC クローズ）。
- Time stop / reduce-only 優先。

---

#### ハードリスクゲート（ここが利益そのもの）
- **Oracle staleness**：`oracle_age_ms` が閾値超で新規禁止
- **Mark–Mid 異常**：`abs(markPx - mid)/mid > Y_bps` が継続で新規禁止
- **Spread shock**：`spread_now > k * spread_baseline` でクールダウン（新規禁止、必要ならサイズ 1/5）
- **Param change / Halt / OI cap**：検知で即停止（新規禁止 + 全キャンセル、可能なら縮小モード）
- **Isolated buffer**：`liq_buffer_pct` を割ったら新規禁止（縮小・撤退優先）

---

#### 市場（銘柄）選定ルール（EV が出る市場だけ触る）
- oracle 更新が一定以上安定（stale 頻発市場は除外）
- best に最低限の板厚がある（踏んで逃げられる）
- 常時ワイドすぎる市場は避ける（テイクで往復負けしやすい）
- ハルト/仕様変更が頻発する市場はブラックリスト化（MarketRegistry 差分ログで管理）

---

#### 開発ロードマップ（推奨）
1) **観測のみ（紙トレ）**：条件成立回数・成立時間・乖離幅・板厚を MarketKey 別に記録
2) **超小口 IOC で実弾検証**：滑り/手数料込みの実効 EV を測る
3) **停止品質の改善**：oracle stall / spread shock / param change 検知を強化

### 8.8 Oracle/Mark Dislocation Taker：より詳細な実装計画（現実的な段階導入）

方針：外部有料データ無しで「板の best が oraclePx を跨ぐ瞬間だけ」IOC で踏み、原則として短時間でフラットへ戻す。勝ち筋はロジックそのものより **停止品質（Hard Risk Gate）** と **市場選定（EV が出る MarketKey だけ触る）** に置く。

#### 8.8.1 実装定義（最小のブレない仕様）
- 参照価格：`oraclePx` を一次参照、`markPx` を検算側（異常検知）
- エントリー（best が跨いだ時のみ）
  - Buy：`best_ask <= oraclePx * (1 - (FEE_BPS + SLIP_BPS + EDGE_BPS)/1e4)`
  - Sell：`best_bid >= oraclePx * (1 + (FEE_BPS + SLIP_BPS + EDGE_BPS)/1e4)`
  - **mid の乖離だけで入らない**（best が抜ける構造のときだけ実行）
- 執行：IOC（taker）を原則、ポジションは短時間で解消（time stop + reduce-only を基本）

#### 8.8.2 HIP-3 前提の Market Discovery（Dex 境界の明確化）
- HIP-3 は `perp dex`（DEX 単位）と `asset`（銘柄）が二重構造。
- 実装キーは `MarketKey=(DexId, AssetId)`（表示名/symbol は識別子にしない）。
- 初手でやること：
  1) `perpDexs` から対象 DEX を解決（xyz 等）
  2) `meta(dex)` / `metaAndAssetCtxs(dex)` で universe と仕様（tick/lot/leverage/fee/OI cap 等）を同期
  3) tick/lot/fee 変更は「異常」として扱い、検知したら該当 MarketKey を停止

#### 8.8.3 取得データ（WS 中心、REST は補助）
- WS（最小）
  - `activeAssetCtx(coin)`：`oraclePx/markPx/(optional)midPx/OI/funding` 等
  - `bbo(coin)` または `l2Book(coin)`：best bid/ask と top depth
  - `orderUpdates(user)`（任意で `userFills(user)`）
- REST / info（補助）
  - `perpsAtOpenInterestCap(dex)`：OI cap 到達銘柄
  - `perpDexStatus(dex)`：DEX 側ステータス（停止/異常の把握）

#### 8.8.4 Hard Risk Gate（利益そのもの：必ず先に実装）
- Oracle 鮮度：`oraclePx` の最終変化時刻を保持し、`now - last_change > ORACLE_FRESH_MS` で新規禁止
  - 初期値：`ORACLE_FRESH_MS = 8000ms`（3秒更新の 2〜3回分を想定して保守的に）
- Mark–Mid 異常：`abs(markPx - mid)/mid > Y_bps` が継続なら新規禁止（mid は bbo 由来で可）
- Spread shock：`spread_now > k * EWMA(spread)` でクールダウン（新規禁止 or サイズ 1/5）
- OI cap：該当銘柄は新規禁止（既存ポジ縮小のみ）
- Param change / Halt 相当：検知で即停止（新規禁止 + 全キャンセル、可能なら縮小モード）
- Isolated buffer：清算バッファが閾値割れで新規禁止（撤退優先）

#### 8.8.5 最小アーキテクチャ（運用に耐える分割）
- `MarketRegistry`：perpDexs/meta 同期、仕様差分検知
- `Feed`：WS 集約（activeAssetCtx + bbo/l2Book + orderUpdates）
- `RiskGate`：上記 Hard Gate を一元実装（pass/fail + size multiplier を返す）
- `DislocationDetector`：edge 計算、best を跨いだときのみシグナル生成
- `TakerExecutor`：IOC 発注、client order id（cloid）で冪等性
- `PositionManager`：「持たない」強制（time stop + reduce-only）
- `Telemetry/Persistence`：トリガー→注文→約定→PnL を 1行で保存（検証の主戦場）

#### 8.8.6 段階導入（壊れない順に進める）
- Phase A：観測のみ（紙トレ）
  - 条件成立回数、継続時間、edge 分布、spread/depth、oracle stall・mark-mid 異常頻度を MarketKey 別に記録
  - 成果：EV が出そうな MarketKey 候補のランキング
- Phase B：超小口 IOC（実弾、損益より“滑り”の実測）
  - top-of-book 連動の小サイズで入り、即 reduce-only で戻す
  - 成果：手数料+滑り込みで edge が残るか、残ポジ率/フラット化品質
- Phase C：停止品質の改善（テール対策の強化）
  - OI cap / dex status / param change の検知を強化し、例外なく停止する
- Phase D：対象市場の自動入れ替え
  - rolling 統計で上位 N 市場のみ稼働、ブラックリスト（oracle stall 多発・halt 多発等）導入

#### 8.8.7 初期パラメータ（まず動かすための現実解）
- `ORACLE_FRESH_MS = 8000ms`
- `EDGE_BPS = (想定taker fee + 想定slippage + safety margin)`（最終的には Phase B の実測で上書き）
- `SIZE_ALPHA = 0.10`（top-of-book の 10% 上限）
- `MAX_NOTIONAL_PER_MARKET`：isolated-only 前提で小さく固定（市場ごとに段階的に解放）
- `MAX_SPREAD_BPS` / `MIN_DEPTH_TOP`：固定値よりも分布/EWMA ベースで市場別に設定


#### 8.8.8 次の一手（最短で前進する順）
1) `perpDexs` から対象 HIP-3 DEX を確定
2) Phase A（観測のみ）を実装：`activeAssetCtx + bbo` で“跨ぎ”トリガーを MarketKey 別に記録
3) RiskGate を最初から有効化（oracle fresh / spread shock / OI cap）
4) ログから 2〜3 市場を選び、Phase B（超小口 IOC）へ


### 8.9 競合に負けない技術スタック（案1：勝ちに行く／低遅延・高信頼）

狙い：Oracle/Mark Dislocation Taker（検知→IOC）で勝ち筋を作るために、(A) データ取り込み遅延、(B) 執行レイテンシ、(C) レート制限下での安定稼働、(D) 異常停止の速さを最優先で最適化する。

#### 8.9.1 実運用コア（Feed + Detection + Execution）
- **Rust（tokio）** をコア言語にする（単一プロセスで状態機械を持つ）。
- **WS は自前実装**（購読管理・再接続・バックプレッシャ・スナップショット処理まで含める）。
- 戦略は「イベント駆動（跨ぎ・異常・在庫偏り等）で IOC 発火」。cancel/replace 合戦を避ける。

#### 8.9.2 執行（Exchange endpoint）
- 取引は **Exchange endpoint（HTTP）** を使用。
- 署名は「最小依存」で実装（SDK に寄りかからない）。

#### 8.9.3 注文冪等性（cloid）
- **client order id（cloid）を必須**にし、再送で二重発注しない。
- `OrderStateMachine`（NEW→ACK→PARTIAL→FILLED→CANCELED）をイベントで遷移。
- `CloidGenerator`（衝突ゼロ：起動ID + 単調増加 + 日付等）を用意。

#### 8.9.4 リスク／停止（Hard Risk Gate を最優先で実装）
- `oracle fresh / spread shock / OI cap / param change / halt / isolated buffer` を一元ゲート化。
- ゲートは「pass/fail + size multiplier（例：1.0 / 0.2 / 0）」を返し、実行側は例外なく従う。

#### 8.9.5 ログ／観測（勝敗を決める主戦場）
- **Prometheus + Grafana**：稼働監視（WS接続、レート制限、reject理由、トリガー回数など）。
- **ClickHouse**（推奨）または Parquet：Phase A/B の全イベントを MarketKey 別に保存し、EV が出る市場をランキング。
- 重要メトリクス例：`edge_bps_after_fees`、`effective_slippage`、`oracle_stall_rate`、`mark_mid_gap_bps`、`spread_shock_rate`、`flat_time_ms`。

#### 8.9.6 研究／検証（オフライン分析）
- **Python（Polars + numpy）** を検証専用にする（実運用コアには入れない）。
- 目的：市場選定・閾値最適化・滑り/手数料込みの実効 EV 推定。

#### 8.9.7 デプロイ／運用
- **Docker Compose + systemd**（再起動保証、ログローテ、健全性チェック）。
- 単一プロセス前提（分散は遅延と運用負荷が増えるため、まずは避ける）。
- 配置リージョンは `api.hyperliquid.xyz` への RTT を測って最小地点に寄せる。

#### 8.9.8 実務上の方針（非交渉ライン）
- 速度より先に **冪等性・停止品質・レート制限耐性** を固定する（事故らないことが EV を作る）。
- 例外時は「継続」ではなく **縮小／停止** に倒す（HIP-3 のテール対策）。


## 参考リンク（一次情報中心）
- Hyperliquid Docs: HIP-3: Builder-deployed perpetuals
- Hyperliquid Docs: Fees
- Hyperliquid Docs: Risks
- Hyperliquid Docs: HIP-3 deployer actions（API）
- Hyperliquid Docs: Exchange endpoint（DEX abstraction）
- Hyperliquid Docs: Hyperps


