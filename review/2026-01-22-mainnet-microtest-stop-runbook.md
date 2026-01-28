# 2026-01-22 Mainnet Micro Test: 停止Runbook（cancel → flatten → HardStop相当）

対象: `mode="trading"` の mainnet 少額テスト（例: `config/mainnet-test.toml`）。

## 目的
- これ以上の新規発注を止める
- **未約定注文を全キャンセル**する
- **建玉をゼロに戻す（flatten）**
- “HardStop相当” として **再起動・再発注を防止**する

## HardStop相当（このRunbookでの定義）
実装上の HardStop が自動 flatten まで結線されていない前提で、運用として以下を満たす状態を “HardStop相当” とみなします。

- Bot が停止しており（プロセス停止 / コンテナ停止）
- API Wallet の秘密鍵（`HIP3_TRADING_KEY`）が **実行環境から撤去**され
- 自動再起動（systemd / docker `restart` / supervisor / tmux自動復帰）が **無効化**されている

## 0) 事前に控える（監査ログ）
- 実行時刻（UTC とローカル）
- 実行していた config（例: `config/mainnet-test.toml`）
- `user_address` / `signer_address`
- Bot の PID（または systemd unit / container 名）
- 直近ログ（最後の 200 行）

## 1) Bot停止（最優先・新規発注を止める）

### A. 手元で起動している（`cargo run` / 直起動）
1. Bot を起動している端末で `Ctrl+C`
2. 反応しない場合:
   - `pkill -INT -f "hip3-bot.*mainnet-test\\.toml" || true`
   - 5秒待って残っていたら `pkill -TERM -f "hip3-bot.*mainnet-test\\.toml" || true`
   - 最終手段 `pkill -KILL -f "hip3-bot.*mainnet-test\\.toml" || true`

### B. docker compose で起動している
- `docker compose stop`
- 自動再起動が有効なら `docker compose down`

### C. 確認
- `pgrep -f "hip3-bot" || true` が空（対象プロセスがいない）
- Bot 側ログが増えない（停止を確認）

## 2) cancel（未約定注文の全キャンセル）
目的: 取り残し注文で意図せぬ約定が起きるのを防ぐ。

1. Hyperliquid UI で対象アカウント（`user_address`）に切替
2. Open Orders / Trigger Orders を **全キャンセル**
3. 確認:
   - Open Orders = 0
   - Trigger/Conditional Orders = 0

メモ:
- Bot停止前に cancel しても、Bot が再度発注する可能性があるため **必ず Bot停止を先に**行う。

## 3) flatten（建玉をゼロに戻す）
目的: 価格変動による損失拡大や強制清算を防ぐ。

1. Positions を確認し、対象 market（例: `xyz:NVDA`）の建玉があれば **Close / Flatten**
2. 約定後の確認:
   - Position size = 0
   - Unrealized PnL / Leverage 表示が残っていない

注意:
- “reduce-only” を使える場合は reduce-only を優先（誤って反転ポジションを作らない）。

## 4) HardStop相当（再起動・再発注の防止）

1. **鍵を撤去**（実行環境から消す）
   - `unset HIP3_TRADING_KEY`
   - systemd / docker / supervisor の環境変数定義から削除（`.env` / unit file / secret）
2. **自動再起動を無効化**
   - systemd: `systemctl disable --now <unit>`
   - docker: `restart: unless-stopped` 等がある場合は `down` まで行う
3. **再起動時の誤起動防止**
   - 次回起動は `mode="observation"` の config を使う、または trading 用 config を別名に退避

## 5) 最終確認（事故防止のチェック）
- Open Orders = 0
- Trigger Orders = 0
- Positions = 0
- Bot が動いていない
- （可能なら）5分後にもう一度 UI を更新して取り残しがないか確認

## 付録: helper script
プロセス停止だけを補助するスクリプト:
- `scripts/mainnet_microtest_stop.sh`

