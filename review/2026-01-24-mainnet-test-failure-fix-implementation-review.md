# 2026-01-24 Mainnet Test Failure Fix - Implementation Review

## Scope
- Plan: .claude/plans/2026-01-24-mainnet-test-failure-fix.md
- Files reviewed: 
  - crates/hip3-executor/src/executor_loop.rs
  - crates/hip3-executor/src/signer.rs
  - crates/hip3-executor/src/ws_sender.rs
  - crates/hip3-executor/src/error.rs
  - crates/hip3-ws/src/message.rs
  - crates/hip3-bot/src/app.rs
  - crates/hip3-risk/src/gates.rs
  - config/mainnet-test.toml

## Findings
1) **CRITICAL**: `SpecCache` がどこでも更新されておらず、注文バッチが常に失敗する可能性
   - `ExecutorLoop::batch_to_action` が `spec_cache.get()` 失敗時に `MarketSpecNotFound` を返し、バッチ全体を失敗させます。
     - 参照: `crates/hip3-executor/src/executor_loop.rs:456-469`
   - `Application::run_preflight()` では `perpDexs` を取得しますが、`SpecCache::update()`/`parse_spec()` の呼び出しがなく、`SpecCache` が空のままです。
     - 参照: `crates/hip3-bot/src/app.rs:108-205`
   - 影響: 新規注文は `handle_batch_conversion_failure()` でドロップされ、reduce_only は再キューされ続けるため、実運用では注文が一切送信されないリスク。
   - 対応案: preflightで `perpDexs` から `MarketSpec` を生成し `SpecCache` に投入、または `SpecCache` 未充足時のフォールバック（暫定specや送信停止）を明示。

## Open Questions / Assumptions
- **P4 (BBO stale 閾値調整)**: `config/mainnet-test.toml` に `max_bbo_age_ms` の上書きがなく、`RiskGateConfig` のデフォルト 2000ms のままです。計画上の修正を適用するか確認が必要。
- **P5 (CL 市場調査)**: これは運用/検証タスクでありコード反映は見当たりません。意図的に未実施であれば問題なし。

## Tests
- ローカル実行は未実施。
