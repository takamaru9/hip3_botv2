# Phase B 実装計画レビュー（3.2 Signer 実装）

対象: `.claude/plans/2026-01-19-phase-b-executor-implementation.md`（3.2 Week 1-2: 署名・発注 / Signer実装）  
確認日: 2026-01-20

## 結論
**未承諾**。Hyperliquid 公式 Python SDK（`hyperliquid/utils/signing.py`）の実装に照らすと、3.2 の署名仕様（hash手順/EIP-712/connectionId/nonceエンコード）が一致していません。次の3点を直してください。

## 指摘（今回の指摘: 3点）

### 1) API整合: `sign_order()` ではなく「Action（Batch）署名」に揃える
現状の3.2は `sign_order(&Order, nonce) -> SignedOrder` ですが、計画後半では `sign_action(&Batch, nonce, post_id)` を前提にしています（設計が二重）。  
→ 3.2のコード例/タスクを、以下のいずれかに統一してください。

- `Signer::sign_action(action: &Action, nonce: u64) -> Signature`（署名の責務だけ）
- `Signer::build_signed_action(batch: &Batch, nonce: u64) -> SignedAction`（Action構築+署名）

また `post_id` は **WSの相関IDであり署名対象ではない** ので、Signerの責務に混ぜない設計にしてください（`WsSender`/post request層で付与）。

### 2) 署名仕様の固定（“何をどうhashして署名するか”）とテストベクトル
「exchange-endpoint docs参照」だけだと、実装時にエンコード/フィールド順/型（EIP-712か否か等）で簡単に事故ります。  
→ 計画に最低限これを明記してください。

- `Action` のJSONスキーマ（orders/cancels/その他必須フィールド）
- 署名対象データ（`action + nonce + ...` のどれを含むか）と、hash/署名の手順（ライブラリ込み）
- **オフライン検証できるgolden test**（入力Action/nonce→期待signature）を用意する方針  
  - Testnet検証は別枠でOKだが、unit testで壊れない土台を先に固定する

### 3) 鍵の取り扱い（ロード元・誤設定検知・秘匿）を計画に入れる
`private_key: SigningKey` を直持ちする例だと、キーのロード/権限分離/API wallet前提/誤設定検知が抜けやすいです。  
→ 少なくとも計画に以下を追加してください。

- キーの供給元（env/config/別プロセス等）と、**Observation用/Trading用の分離**（API wallet想定）
- `address` は **秘密鍵から導出して一致検証**（不一致なら起動失敗）
- ログ/メトリクスに秘密情報を出さない（署名や秘密鍵の誤出力防止）、必要ならメモリ保護（`zeroize`等）方針

---

## 再レビュー（2026-01-20）
初回の3点（API統一/署名仕様の明記/鍵管理の追記）は概ね反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

### 1) 全体整合: 3.4側が旧APIのまま（`sign_action(&batch, nonce, post_id)` が残っている）
3.2で `sign_action(action, nonce)` / `build_and_sign(batch, nonce)` に寄せた一方で、ExecutorLoopが旧APIを呼んでいます。  
例: `.claude/plans/2026-01-19-phase-b-executor-implementation.md:1396`  
→ 計画全体で呼び出しを統一し、ExecutorLoopは `build_and_sign(&batch, nonce)` 等を使う形に直してください（`post_id` はWsSender層で付与のまま）。

### 2) 署名仕様がまだ曖昧（“EIP-712”と実際のhash手順が一致していない）
「EIP-712 TypedData」と書きつつ、例の `hash()` は `action_json + nonce + timestamp + vault?` に独自prefixを付けて `keccak` しており、仕様根拠が弱いです（`vault_address?` の `?` も未確定のまま）。  
→ 公式docs/参照実装（SDK等）に合わせて、**署名対象フィールド・エンコード手順・domain/typehash 等**を“断定できる形”で固定してください（断定できないなら EIP-712 と書かない）。

### 3) Golden test が未完成/不安定（TODOやenv依存が残る）
現状のgolden testは `expected_hash = "0x..." // TODO` のままで、さらに `TEST_PRIVATE_KEY` をセットせず `EnvVar` 参照しており、そのまま実装するとテストが落ちます。  
→ `expected_hash`/`expected_signature` を実値で固定し、テストは環境に依存しない形にしてください（固定timestamp/固定action/固定key）。必要なら `sign_action` 側も時刻注入 or timestamp引数でテスト可能にする方針を追記してください。

---

## 再レビュー（2026-01-20, 追補）
前回の3点（ExecutorLoop側のAPI統一、署名仕様の“EIP-712”撤回とSDK準拠化、golden testの環境非依存化）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

### 1) `connection_id` が未確定（Mainnet/Testnetの定数がプレースホルダー）
`MAINNET_CONNECTION_ID` / `TESTNET_CONNECTION_ID` が `/* ... */` のままで、これでは実装が確定しません。  
→ SDKの値をそのまま定数として本文に埋めるか、「どこから取得するか（例: SDK定数/Docs/Genesis hash）」を断定してください。

### 2) msgpackの互換性が壊れやすい（Optionフィールドのnil混入など）
Rust側の `Action { orders: Option<...>, cancels: Option<...> }` が `None` のとき、serde設定次第で `cancels: nil` のように **キー自体が入ってしまう**可能性があります。Python SDK側のdictと差分が出ると hash が一致しません。  
→ `orders/cancels` を含む **Optionフィールドは `skip_serializing_if = "Option::is_none"` を徹底**し、msgpackのエンコード設定（`rmp-serde`のどの関数/設定で、Python `msgpack.packb(..., use_bin_type=...)` と合わせるか）を明記してください。

### 3) `timestamp_ms` が設計上ブレている（署名に含む/含まないが曖昧）
現状の `SigningPayload` に `timestamp_ms` と `with_timestamp()` が残っていますが、`hash = keccak256(connection_id || action_msgpack || nonce_bytes || vault_bytes)` の式には timestamp が出てきません。  
→ SDK準拠で **timestampが不要なら削除**（Signerからも除去）、必要なら **hash式に含める**形で断定してください（golden testもそれに合わせて固定）。

---

## 再レビュー（2026-01-20, SDKソース突合）
`hyperliquid/utils/signing.py` を確認したところ、現状の 3.2 記載と差分が大きいです。次の3点を修正してください。

### 1) 署名仕様がSDKと不一致（EIP-712/connectionId/hash手順/エンディアン）
3.2 では「EIP-712ではない独自形式」「`hash = keccak(connection_id || action_msgpack || nonce_le || vault_bytes)`」になっていますが、SDK は以下です。

- `action_hash = keccak(msgpack.packb(action) + nonce.to_bytes(8, "big") + vault_tag + (expires_after_tag?))`
- `phantom_agent = {"source": "a" or "b", "connectionId": action_hash}`
- **EIP-712 Typed Data**（domain: chainId=1337, name="Exchange", version="1", verifyingContract=0x0 / primaryType="Agent"）を `encode_typed_data` して署名

→ 計画の `MAINNET_CONNECTION_ID/TESTNET_CONNECTION_ID` 定数は前提ごと削除し、SDKと同一の2段構成（`action_hash` → phantom_agent EIP-712署名）に置き換えてください。`nonce` は **big-endian**、`vault_address=None` の場合も **0x00 のtag 1byteが必ず入る**点も仕様に固定してください。

### 2) Actionスキーマが不足（少なくとも `grouping` が必要）
SDK の `order_wires_to_order_action()` は `{"type":"order","orders":[...],"grouping":"na"}`（+ optional `builder`）を生成します。現状の 3.2 `Action` 例には `grouping` が無く、msgpackが一致しません。  
→ `type=order` の必須キー（`orders`, `grouping` など）をスキーマとして明記し、`Batch -> Action` 変換もそのスキーマに合わせてください（他アクションも同様）。

### 3) Golden test/生成スクリプトがSDK API と不一致
計画の `scripts/generate_golden_test_vectors.py` は `sign_l1_action(..., return_hash=True)` 前提ですが、SDK の `sign_l1_action` は `sign_l1_action(wallet, action, active_pool, nonce, expires_after, is_mainnet)` で、戻り値は `{r,s,v}` です（hashを返しません）。  
→ スクリプトは `action_hash()` を別途呼んで期待hashを出し、`sign_l1_action()` の `{r,s,v}` から Rust 側の期待signature表現（65 bytes/hex など）を断定して固定してください（`vault_address/active_pool` と `expires_after` も含めて固定入力にする）。

---

## 再レビュー（2026-01-20, 修正後）
大幅改訂（2段階署名、`grouping` 追加、`connection_id` 定数削除、SDK API へ寄せたスクリプト）は反映されました。  
ただしまだ **未承諾** で、次の3点を直してください。

### 1) `expires_after` のエンコードがSDKと不一致（tag/存在）
計画では `expires_after=None` でも `0x00` タグ1byteを入れ、`Some` の場合は `0x01 + expires_after(8 bytes)` になっています。  
しかし SDK（`hyperliquid/utils/signing.py:action_hash`）は以下です。

- `expires_after is None` の場合: **何も追加しない**（tag自体が存在しない）
- `expires_after is not None` の場合: **`0x00` + `expires_after.to_bytes(8, "big")`** を追加

→ 本文の擬似コード・`SigningInput::action_hash()`・「重要な仕様ポイント」（`expires_after=None`）を SDK と一致する形に修正してください（ここがズレると golden test が必ず不一致になります）。

### 2) Golden vector スクリプトの署名bytes化が不正（r/s の 32byte 左pad）
SDK の `sign_l1_action()` が返す `r/s` は `to_hex(int)` なので **先頭ゼロが省略**され得ます。  
計画の `bytes.fromhex(sig_result["r"][2:])` は 32 bytes を保証できず、`r||s||v` が 65 bytes になりません。

→ `r/s` は必ず 32 bytes に揃えてください（例: `int(sig["r"],16).to_bytes(32,"big")` / `zfill(64)`）。併せて Rust 側の `Signature` の `v` 表現（27/28 か 0/1）も golden と一致する前提を本文で固定してください。

### 3) 仕様ドキュメントの整合が残っている（API表と wire 型）
- 冒頭の API 表が `sign_action(action, nonce)` のままですが、本文コードは `sign_action(action, nonce, vault_address, expires_after)` になっています。計画全体で表記を統一してください。
- `OrderTypeWire` / `CancelWire` の定義が本文に無く、`order_type: OrderTypeWire::Ioc` のような表記も SDK の wire（`{"t":{"limit":{"tif":"Ioc"}}}` など）と対応が不明です。`exchange.py` の `order`/`bulk_cancel` が生成する wire 形式に合わせて、型（serde表現）を計画内で確定してください。

---

## 再レビュー（2026-01-20, 再修正後）
前回の3点（`expires_after` エンコード、golden生成のpadding/v変換、API表+wire型の確定）は反映されています。  
ただし 3.2 はまだ **未承諾** で、次の3点を直してください。

### 1) 「1 action に orders と cancels を同居」前提が危険（SDKのactionスキーマと不整合の可能性）
Python SDK は `order_wires_to_order_action()` が `{"type":"order","orders":[...],"grouping":"na"}`、cancel は `{"type":"cancel","cancels":[...]}` のように **別action** を生成します（同居しない）。  
一方、計画の `Batch { orders, cancels }` は同一tickで両方を収集し得て、`Action::from_batch()` が **どの action.type で送るのか**が未定義です。

→ 計画として次のどちらかを確定してください（断定できる形で）。
- (A) **同居が仕様として可能**である根拠（docs/SDK）を提示し、Actionスキーマを「orders+cancels同居前提」で固定する
- (B) **同居しない**方針に変更し、`BatchScheduler::tick()` が返す単位を「OrderAction または CancelAction のどちらか」にする（= 1 tick = 1 action.type）。両方が溜まっている場合は cancel を先に送り、order は次tickへ残す等の挙動を明記

### 2) `TriggerOrderType` のフィールド順がSDKと一致しない（将来triggerを使うならhashがズレる）
SDK の trigger wire は `{"isMarket": ..., "triggerPx": ..., "tpsl": ...}` の順でdictが構築されます。  
現状の `TriggerOrderType { triggerPx, isMarket, tpsl }` の並びだと、msgpackのmap順がズレて `action_hash` が一致しません。

→ trigger を今スコープ外にするなら計画から落とす/「未対応」と明記。対応するなら struct フィールド順も SDK と揃えてください（isMarket→triggerPx→tpsl）。

### 3) 図の呼び出しが古いまま（3.4フロー図）
フロー図に `signer.sign_action(batch, nonce)` が残っていますが、3.2 では `build_and_sign(&batch, nonce)` / `sign_action(action, nonce, vault_address, expires_after)` に整理されています。  
→ `.claude/plans/2026-01-19-phase-b-executor-implementation.md` の 3.4 フロー図側も最新APIに合わせて更新してください（実装時の迷い/誤実装の原因になります）。

---

## 再レビュー（2026-01-20, 再々修正後）
前回の3点（`ActionBatch` で orders/cancels 同居排除、Trigger order の扱い、3.4フロー図の更新）は反映されています。  
ただしまだ **未承諾** で、次の3点を直してください。

### 1) `PostRequestManager` が `Batch` のままで、`ActionBatch` と型不整合
`BatchScheduler::tick()` が `Option<ActionBatch>` になった一方で、`PostRequestManager` は `PendingRequest { batch: Batch }` のままです。これだと計画の疑似コードがコンパイルしません（`handle_send_failure(ActionBatch)` に渡せない等）。

→ `PostRequestManager` 側も `Batch` を **全面的に `ActionBatch` に統一**してください。
- `PendingRequest { batch: ActionBatch, ... }`
- `register(..., batch: ActionBatch)`
- `on_response(...) -> Option<(ActionBatch, bool)>`
- `check_timeouts() -> Vec<(u64, ActionBatch, bool)>`
- `on_disconnect() -> (Vec<ActionBatch>, usize)`

### 2) 3.1 の「バッチ単位」説明が古い（orders/cancels を“同居”させるように読める）
`ActionBatch` 導入で「orders と cancels は同居しない」に確定したのに、3.1 の表が `複数 orders/cancels をまとめる` のままです（読み手が誤解します）。  
→ 表記を「複数 **orders または cancels** をまとめる（同居しない）」に修正し、`高水位時の tick` の説明も `CancelBatch を優先` / `cancel が空なら OrderBatch` の形に揃えてください。

### 3) 3.2 のテスト/タスク文言が `Batch` のまま残っている
`Signer` のテスト項目・タスクに `Batch→Action 変換` のような旧表記が残っています。  
→ 3.2 範囲の表記は `ActionBatch→Action 変換` に統一してください（後で実装時に迷いが出ます）。

---

## 再レビュー（2026-01-20, 型整合修正後）
前回の3点（PostRequestManagerの`ActionBatch`統一、3.1の表記更新、3.2の表記統一）は反映されています。  
ただし 3.2 はまだ **未承諾** で、次の3点を計画に追記/明確化してください。

### 1) WSの action payload（署名のwire形式）が未定義
Signer はできましたが、実際に WS `post` の `request.type="action"` で送る payload のスキーマが本文にありません。ここが曖昧だと「署名は合っているのに送信形式でreject」になります。  
→ SDK の `_post_action()` 相当の payload を計画に固定してください（少なくとも以下）。

- `action`: Action
- `nonce`: u64
- `signature`: `{ r: "0x...", s: "0x...", v: 27|28 }`（**SDK準拠**）
- `vaultAddress`: Option<address>（必要なら）
- `expiresAfter`: Option<u64>（必要なら）

併せて、Rust内部の `Signature` 表現（0/1 or 27/28）と、wireの `v`（27/28）への変換責務（Signer側かWsSender側か）を断定してください。

### 2) KeySource::File のフォーマットが不明（実運用で事故る）
`KeySource::File` が `std::fs::read()` をそのまま `from_slice()` に渡す前提ですが、ファイルに「32bytes生鍵」なのか「hex文字列」なのかが不明です。  
→ フォーマットを断定してください（推奨: `0x...` あり/なし両対応のhex文字列＋改行許容）。env/file で同じパーサを使う方針だと安全です。

### 3) 3.1の高水位説明がまだ誤読される（cancelとordersが同一tickで送られるように読める）
`ActionBatch` で「同居しない」に確定したので、3.1 の「高水位時の tick: cancel/reduce_only のみ送信」等の表現は **同一tickで同居送信**に誤読され得ます。  
→ 「cancelがあれば CancelBatch を返す（ordersは次tick）」「cancelが空なら OrderBatch（高水位なら reduce_only のみ）」のように、表/説明を `ActionBatch` 仕様に揃えてください。

---

## 再レビュー（2026-01-20, 修正後）
前回の3点（WS wire payload スキーマの確定、KeySource::File フォーマットの断定、3.1 高水位説明の ActionBatch 仕様統一）は反映されています。  
**3.2 は承諾**します（この内容で実装に進めます）。
