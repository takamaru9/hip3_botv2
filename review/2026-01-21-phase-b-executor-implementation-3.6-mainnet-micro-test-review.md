# Phase B 実装計画レビュー（3.6 Mainnet超小口テスト）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（3.6 Week 4: Mainnet超小口テスト）  
確認日: 2026-01-21

## 結論
**未承諾**。3.6 は「やること」の概要はありますが、Mainnet は事故コストが高いので、実施前に **Runbook + 安全装置 + 合否基準** を固定してから着手すべきです。次の3点を追記してください。

## 指摘（今回の指摘: 3点）

### 1) Mainnet 切り替え Runbook が無く、誤設定（URL/モード/鍵）で起動し得る
3.5 では Runbook があるのに、3.6 は「Mainnet設定切り替え」しか書かれていません。

→ 3.6 に **Mainnet 起動Runbook** を追加して固定してください（最低限）:
- 使用する config（例: `config/mainnet.toml` か `config/mainnet_micro.toml`）と `mode = "trading"` の明記
- 起動時に確認するログ（`ws_url`/`info_url` が mainnet、`Signer address`、`allowed_markets`、`max_notional` 等）
- Trading key の供給方法（EnvVar/File）と **mainnet用ウォレット**であることの確認手順
- 緊急停止（4.2）を「誰が/どう実行するか」まで手順化（Ctrl+C、HardStop、手動 Flatten など）

### 2) 対象市場が曖昧（`COIN (xyz:5)` だけだと実装/設定に落ちない）
実行系は `MarketKey(dex_id, asset_idx)` / `coin` 表記など複数の表現があり、`xyz:5` が何を指すか曖昧です（asset_idx=5 なのか、`coin="xyz:COIN"` なのか）。

→ 3.6 で **対象市場の最終表現**を1つに固定してください（例）:
- config に `[[markets]] asset_idx = ? / coin = "xyz:COIN"` を明記して「この1市場だけ購読/取引」になること
- preflight（perpDexs）で「この市場が存在すること」を確認する手順と、存在しない場合の中止条件

### 3) 合否基準（go/no-go）が不足していて、100トレード到達前に止める判断ができない
3.6 のメトリクス定義はありますが、**どの閾値で継続/停止/ロールバックするか**が 3.6 では未確定です（4.3 は抽象度が高い）。

→ 3.6 に「到達時点ごとの判定」を追加してください（例）:
- 10/50/100 トレード時点での継続条件（`fill_rate`, `slippage_bps`, `flat_time_ms`, `pnl_cumulative` など）
- 連続失敗/FlattenFailed/Rejected 多発時の停止基準（3.5 の停止条件を mainnet 用に強化）
- 収集方法（どのログ/メトリクス/永続化から `expected_edge_bps` と `actual_edge_bps` を算出するか）

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（Mainnet 起動Runbook、対象市場の明確化、Go/No-Go 基準と edge 算出の追記）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) `config/mainnet_micro.toml` のスキーマが既存/3.5 と不整合で、起動手順が成立しない
Runbook の設定例が `[network] ws_url` / `[trading] mode` の形になっていますが、現状の実装（`crates/hip3-bot/src/config.rs`）は `ws_url`/`info_url`/`mode` を **トップレベル**でパースします。  
このままだと `mode`/`ws_url` が欠落して **設定パース自体が失敗**します。

また、mainnet では `HIP3_MAINNET_PRIVATE_KEY` を設定していますが、設定例に `[signer] key_source = { type="env_var", var_name="HIP3_MAINNET_PRIVATE_KEY" }` が無く、Signer が鍵を参照できません（testnet Runbook とも不整合）。

→ 3.6 の設定例は、少なくとも 3.5 と同じ形に揃えてください（例: `mode`/`ws_url`/`info_url` はトップレベル、`[signer]`/`[safety]` は table で追加）。

### 2) 計画全体の「初期市場」が 3.6 と矛盾していて、運用で市場を取り違え得る
計画冒頭/1.3 では `初期市場: COIN (xyz:5)` のままですが、3.6 では `SNDK-PERP（asset_idx=28）` を前提にしています。

→ どちらを Phase B の初期市場として採用するかを断定し、**冒頭（初期市場）と 1.3 表**も更新して整合させてください（「Week 4 で市場を切り替える」なら、その切替条件と手順を明記）。

### 3) edge 算出 SQL がそのまま動かない（`actual_edge_bps` が未定義）
`avg_slippage` が `AVG(expected_edge_bps) - AVG(actual_edge_bps)` になっていますが、SELECT で `actual_edge_bps` を定義していません（`avg_actual` のはず）。このままだとクエリが失敗します。

→ `avg_slippage` の式を修正してください（例: `AVG(expected_edge_bps) - AVG(CASE ... END)` か、サブクエリで `avg_actual` を参照）。

---

## 再レビュー（2026-01-21, 修正後）
前回の3点（config スキーマ整合、初期市場の統一、edge 算出 SQL 修正）は反映されています。  
**3.6 Mainnet超小口テストは承諾**します（この内容で実装/運用に進めます）。
