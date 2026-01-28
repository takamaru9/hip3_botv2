# 板の流動性（best_size）を考慮した取引ロジック改善レビュー

日付: 2026-01-23  
対象: `crates/hip3-detector` / `config/default.toml`

## 結論（総合評価）
**概ね良好（Conditional Approved）**  
流動性（best_size）を使ったシグナル抑制とサイズ縮小は要件に沿っており、境界・補間・実動作のテストも揃っています。  
一方、**設定値の不正（min >= normal）や負値**に対する防御がないため、運用前にバリデーション追加を推奨します。

---

## 変更概要
1. **DetectorConfig に流動性しきい値を追加**
   - `min_book_notional`, `normal_book_notional`（serde default + Default 実装）
   - デフォルト値: 500 / 5000
2. **流動性係数の導入**
   - `liquidity_factor(book_notional)` で 0.0〜1.0 の線形補間
3. **サイズ計算の修正**
   - `calculate_size()` が流動性係数を掛けた `alpha` を使用
   - 係数が 0 の場合はシグナル自体をスキップ
4. **テスト追加**
   - 係数の境界/補間
   - 低流動性でのシグナル抑制
   - 部分流動性でのサイズ縮小

---

## 詳細レビュー

### 1) Config 追加は妥当
- `min_book_notional` と `normal_book_notional` を **serde default** 付きで追加しており、既存設定との互換性は維持されています。  
- デフォルト値は十分保守的（$500 / $5000）で、急な薄板取引を抑制する狙いに合致。

**懸念**  
`min_book_notional >= normal_book_notional` の場合、`(normal - min)` が 0 または負になり、  
線形補間が不正になります（除算エラーや逆転係数）。  
起動時バリデーションまたは clamp を推奨。

---

### 2) 流動性係数（liquidity_factor）
- **意図は明確**で、範囲外は 0 / 1 にクリップされます。  
- 中間は `(book_notional - min) / (normal - min)` の線形補間で単純かつ予測可能。

**注意点**
- `book_notional` が **負値**（理論上あり得るが誤設定時など）でも 0 に落ちるため、  
異常系の検知としては弱い。  
設定値が負の場合は **明示的に弾く**のが安全。

---

### 3) calculate_size のロジック
- **best_size（bid/ask size）を使用**しているため、要件の「best_sizeを考慮」は達成されています。  
- `liquidity_factor == 0` なら **シグナル自体を無効化**する流れは安全性が高い。  
- サイズは `min(alpha * factor * best_size, max_notional / mid)` の最小値で決定。

**対応済み**
- `book_notional` は **Buy=ask price / Sell=bid price** に変更され、方向非対称が解消。  
- `max_notional / mid` は引き続き mid を使用しており、実勢との僅差は残るが許容範囲。

**軽微な指摘**
- `config.rs` のコメントが「mid_price」表記のままなので更新推奨。

---

### 4) テスト追加の網羅性
- **境界値**（min / normal）、**線形補間**、**低流動性での抑制**、  
  **部分流動性での縮小**がテストされており、基本的な挙動は確認済み。

**不足しているテスト候補**
- Sell 側の price 基準（`bid_price × bid_size`）の回帰テスト  
- ドキュメント/コメントの整合確認（テストではなくレビュー項目）

---

## 追加の提案（更新版）

### ✅ 実装済み
1. **設定バリデーションの追加**
   - `min_book_notional >= normal_book_notional` をエラー化
   - `min_book_notional < 0` / `normal_book_notional <= 0` をエラー化

2. **book_notional の価格基準を side に合わせる**
   - Buy: `ask_price × ask_size`  
   - Sell: `bid_price × bid_size`

3. **log/metrics 強化**
   - `liquidity_factor` / `adjusted_alpha` を debug 出力

---

## 修正対応（実装確認済み）
`~/.claude/plans/ticklish-wobbling-sloth.md` の内容が実装に反映されていることを確認。

### ✅ 設定バリデーション追加
- `DetectorConfig::validate()` を追加  
- `min_book_notional < normal_book_notional` を必須化  
- `min_book_notional >= 0` / `normal_book_notional > 0` を必須化  
- バリデーションテストを追加（min>=normal, negative min/normal, zero normal など）

### ✅ book_notional の価格基準修正
- Buy: `ask_price × ask_size`  
- Sell: `bid_price × bid_size`  
- mid 使用による方向非対称は解消

### ✅ ログ強化
- low-liquidity スキップ時に `normal_book_notional` / `liquidity_factor` を出力  
- 通常時に `liquidity_factor` / `adjusted_alpha` を debug 出力

### 付随修正
- `DislocationDetector::new` / `with_user_fees` が `Result` を返すよう変更  
- 呼び出し側（`hip3-bot` とテスト）で `?` / `unwrap()` 対応  
- サイズ関連テストは side price 基準に合わせて許容誤差を調整

---

## テスト実行状況
**実行済み**（2026-01-23）  
`cargo test -p hip3-detector` → **PASS (37 tests + 1 doc test)**  

---

## 参考：計算式まとめ
- `book_notional = best_size × side_price (buy=ask / sell=bid)`  
- `liquidity_factor = clamp((book_notional - min) / (normal - min), 0..1)`  
- `size = min(alpha × liquidity_factor × best_size, max_notional / mid_price)`
