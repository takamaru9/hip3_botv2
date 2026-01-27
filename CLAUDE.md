# hip3_botv2 Project Configuration

## Plan Mode Settings (MANDATORY)

**⚠️ CRITICAL: These settings MUST be followed without exception.**

### Plan File Location

| Setting | Value |
|---------|-------|
| **Absolute Path** | `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/` |
| **Relative Path** | `.claude/plans/` |
| **Naming Convention** | `YYYY-MM-DD-<feature-name>.md` |

### Rules (MUST FOLLOW)

1. **Plan Creation**: When entering plan mode, ALL plan files MUST be saved to `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/`
2. **Plan Reference**: When transitioning from plan mode to implementation, MUST reference plans from this same folder
3. **Extended Thinking**: Always enable extended thinking (thinking mode) when in plan mode
4. **Never Deviate**: Do not save plan files to any other location under any circumstances
5. **Use Agents During Planning**: Actively use project-specific agents for planning tasks (see below)
6. **Verify Primary Sources**: All external API/library specs MUST be verified from official documentation (see below)

### Primary Source Verification (MANDATORY - 絶対遵守)

**🚨 ABSOLUTE PROHIBITION: Planning or implementing based on memory/training data is FORBIDDEN.**

All technical decisions MUST be based on verified primary sources. Memory-based assumptions lead to:
- Incorrect API usage
- Outdated specifications
- Runtime failures in production
- Wasted development time

#### Source Priority (MUST follow this order)

| Priority | Source Type | Tools | Example |
|----------|-------------|-------|---------|
| 1st | **Official Documentation** | WebFetch | GitBook, ReadTheDocs, official sites |
| 2nd | **API Reference** | WebFetch | Swagger/OpenAPI specs, REST/WS docs |
| 3rd | **Source Code** | codebase-explorer, GitHub | When docs are incomplete |
| 4th | **Library Docs** | Context7 MCP | For Rust crates, npm packages |
| Last | **Web Search** | WebSearch | Only when above fail |

#### Verification Requirements

| When | What to Verify | How |
|------|----------------|-----|
| **External API Usage** | Endpoint URLs, request/response format, auth | WebFetch official docs |
| **WebSocket Protocol** | Message format, channel names, ACK structure | WebFetch + real testing |
| **Library/Crate Usage** | Function signatures, error types, behavior | Context7 or WebFetch docs |
| **Exchange Integration** | Rate limits, order types, error codes | WebFetch exchange docs |

#### Search Until Found

**一次情報が見つかるまで探索を続けること:**

```
1. WebFetch: 公式ドキュメントURL（GitBook, ReadTheDocs等）
2. WebSearch: "[サービス名] [機能] documentation" で検索
3. WebFetch: 検索結果のURLを順に確認
4. Context7: ライブラリドキュメントを検索
5. WebFetch: GitHub リポジトリの README, docs/
6. 見つからない場合: ユーザーに報告し、実測テストを計画に含める
```

#### Documentation in Plan

計画には必ず以下を記載：

```markdown
## 参照した一次情報

| 項目 | ソース | URL | 確認日 |
|------|--------|-----|--------|
| WebSocket仕様 | Hyperliquid GitBook | https://... | 2026-01-24 |
| Rate Limit | 同上 | https://... | 2026-01-24 |

## 未確認事項（実測必須）

| 項目 | 理由 | 実測方法 |
|------|------|----------|
| エラー形式 | ドキュメントに記載なし | Testnetで意図的エラー発生 |
```

#### Violation Response

If primary source cannot be found:
1. **STOP** - Do not proceed with assumptions
2. **REPORT** - Tell user what information is missing
3. **PLAN TESTING** - Include real-world verification in the plan
4. **NEVER GUESS** - Do not fill gaps with training data

### Agent Usage During Planning (MANDATORY)

**⚠️ When creating or refining plans, you MUST use these agents to gather accurate information:**

| Planning Phase | Agent | Purpose |
|----------------|-------|---------|
| **情報収集** | `codebase-explorer` | 既存実装の調査、型・関数の検索 |
| **影響範囲分析** | `codebase-explorer` | 変更が影響するファイル・モジュールの特定 |
| **Risk評価** | `risk-gate-analyzer` | Risk Gate関連の変更時、発火条件の確認 |
| **WebSocket関連** | `ws-debugger` | WS接続・通信の計画時、現状把握 |
| **既存計画確認** | `spec-manager` | 過去のPlan/Specとの整合性確認 |

**Planning Workflow:**
```
1. ユーザーから要件を受け取る
2. 【一次情報確認 - 必須】
   a. WebFetch: 関連する外部API/サービスの公式ドキュメントを取得
   b. Context7: 使用するライブラリ/crateのドキュメントを検索
   c. WebSearch: 上記で不足する情報を検索
   d. 見つからない場合 → ユーザーに報告、実測計画を含める
3. Task(codebase-explorer): 関連コードの調査
4. Task(spec-manager): 既存Plan/Specとの整合性確認
5. [必要に応じて] Task(risk-gate-analyzer) or Task(ws-debugger)
6. 収集した一次情報とコード調査を基に計画を作成
   - 「参照した一次情報」セクションを必ず含める
   - 「未確認事項（実測必須）」セクションを必ず含める
7. .claude/plans/に保存
```

**Why This Matters:**
- **一次情報に基づく計画**: 記憶ではなく公式ドキュメントから正確な仕様を取得
- 推測ではなく実際のコードに基づいた計画が作れる
- 既存実装との整合性を保てる
- 非交渉ラインの違反を事前に検出できる
- メイン会話のコンテキストを消費せずに調査できる
- **実測計画の明確化**: ドキュメントにない仕様は実測で確認する計画を含める

## Implementation Rules (MANDATORY)

**🚨 ABSOLUTE PROHIBITION: Implementing based on memory/training data is FORBIDDEN.**

### Before Writing Code

1. **Verify Plan Exists**: Implementation MUST be based on an approved plan in `.claude/plans/`
2. **Check Primary Sources**: If plan references external APIs, verify the documented specs are still current
3. **No Guessing**: If uncertain about API behavior, error formats, or edge cases:
   - Check documentation again
   - Ask user for clarification
   - Plan a test to verify behavior

### During Implementation

- Code MUST match the specifications documented in the plan
- If docs are ambiguous, add defensive code with clear comments explaining the uncertainty
- Log unexpected responses for future debugging

---

## Code Save Workflow (MANDATORY)

**CRITICAL**: After writing or modifying any code file, you MUST execute the following checks in order:

### Python Files (.py)

| Step | Command | Purpose |
|------|---------|---------|
| 1. Lint | `ruff check --fix <file>` | Code quality & error detection |
| 2. Format | `ruff format <file>` | Code formatting |
| 3. Type Check | `mypy <file>` | Type safety verification |
| 4. Simplify | Task tool with `code-simplifier` agent | Code simplification & refactoring |

### Rust Files (.rs)

| Step | Command | Purpose |
|------|---------|---------|
| 1. Format | `cargo fmt` | Code formatting |
| 2. Lint | `cargo clippy -- -D warnings` | Static analysis (warnings as errors) |
| 3. Check | `cargo check` | Type & compile check (faster than build) |
| 4. Simplify | Task tool with `code-simplifier` agent | Code simplification & refactoring |

### Execution Examples

**Python:**
```bash
# After saving src/example.py
ruff check --fix src/example.py
ruff format src/example.py
mypy src/example.py
```

**Rust:**
```bash
# After saving src/example.rs
cargo fmt
cargo clippy -- -D warnings
cargo check
```

Then invoke the `code-simplifier` agent via Task tool to review and simplify the modified code.

### Rules

1. **Never skip these checks** - All 4 steps are mandatory
2. **Fix all errors** - Do not proceed if lint/format/type errors exist
3. **Apply simplifications** - Accept code-simplifier suggestions that improve clarity
4. **Preserve functionality** - Simplification must not change behavior

## Hooks (Auto-Triggers)

**グローバル設定（~/.claude/settings.json）で定義済み。**

### PostToolUse: Rust ファイル編集後

| Trigger | Action | Purpose |
|---------|--------|---------|
| `Edit` on `*.rs` | `cargo fmt && cargo clippy` | 自動フォーマット・静的解析 |

**効果**: .rs ファイル編集後に自動で fmt/clippy が実行される。エラーがあれば出力に表示。

### PreToolUse: git push 前

| Trigger | Action | Purpose |
|---------|--------|---------|
| `Bash(git push*)` | リマインダー表示 | レビュー/テスト完了確認 |

**効果**: git push 実行前に確認リマインダーが表示される。

### 注意事項

- Hooks はグローバル設定のため全プロジェクトに適用
- エラー時は Hook 出力を確認してから修正
- 無効化が必要な場合は `~/.claude/settings.json` を編集

## Context Management (MANDATORY)

**Goal**: Keep main conversation context clean by delegating work appropriately.

### Delegation Priority

| Priority | Tool | When to Use |
|----------|------|-------------|
| 1st | **Subagent (Task tool)** | High-volume output, isolated work, parallel tasks |
| 2nd | **Skills** | Reusable workflows, automatic triggers |
| 3rd | **MCP Plugins** | External tools/data (Context7, etc.) |
| Last | **Main Conversation** | Quick edits, iterative refinement only |

### Rules (MUST FOLLOW)

1. **Use Subagents for**:
   - Codebase exploration (`Explore` type)
   - Test execution and log analysis
   - Documentation generation
   - Any task producing verbose output

2. **Use Skills for**:
   - Commit workflows (`/commit`)
   - Code review (`/code-review`)
   - Domain-specific guidance

3. **Use MCP Plugins for**:
   - Library documentation lookup (Context7)
   - External API access

4. **Main conversation only for**:
   - Simple file edits (< 3 files)
   - Quick clarifications
   - Final review and confirmation

### Context Cleanup

- Use `/clear` between distinct tasks
- Use `/compact` when context grows large
- Resume subagents instead of restarting

## Implementation Spec Workflow (MANDATORY)

### Purpose
Specs document what was actually implemented vs. what was planned. They serve as:
- Implementation progress tracker
- Deviation record from original plan
- Future reference for discussions

### Spec File Location

| Setting | Value |
|---------|-------|
| **Absolute Path** | `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/specs/` |
| **Naming Convention** | `YYYY-MM-DD-<feature-name>.md` (same date as source plan) |

### Workflow Rules (MUST FOLLOW)

1. **Spec Creation**:
   - When transitioning from plan to implementation, create spec file in `.claude/specs/`
   - Copy plan structure, add status tracking columns

2. **Status Tracking**:
   | Badge | Meaning |
   |-------|---------|
   | `[x] DONE` | Fully implemented and tested |
   | `[~] PARTIAL` | Partially implemented |
   | `[ ] TODO` | Not yet started |
   | `[-] SKIPPED` | Intentionally deferred |
   | `[!] BLOCKED` | Waiting for dependency |

3. **Update Timing**:
   - After each implementation session
   - When deviating from original plan
   - When completing/blocking on items

4. **Deviations**:
   - Any deviation from original plan MUST be documented
   - Include: Original quote, Actual implementation, Reason

5. **Completion**:
   - Mark spec as `[COMPLETED]` when all items accounted for
   - Spec remains as permanent reference

### Spec File Structure

```markdown
# <Feature Name> Implementation Spec

## Metadata
| Item | Value |
|------|-------|
| Plan Date | YYYY-MM-DD |
| Last Updated | YYYY-MM-DD |
| Status | `[IN_PROGRESS]` / `[COMPLETED]` |
| Source Plan | `.claude/plans/YYYY-MM-DD-feature.md` |

## Implementation Status

| ID | Item | Status | Notes |
|----|------|--------|-------|
| P0-1 | 項目名 | [x] DONE | 実装メモ |

## Deviations from Plan
(計画からの逸脱を記録)

## Key Implementation Details
(実装の重要ポイント)
```

## Project-Specific Agents (MANDATORY)

This project has custom subagents in `.claude/agents/`. **Use these agents via Task tool for their designated purposes.**

**⚠️ CRITICAL: These agents MUST be used proactively, not just when explicitly requested.**

### Available Agents

| Agent | Purpose | When to Use | 計画時 |
|-------|---------|-------------|--------|
| `rust-builder` | fmt/clippy/check実行 | Rustコード保存後（Code Save Workflow Step 1-3） | - |
| `code-simplifier` | コード簡素化提案 | Rustコード保存後（Code Save Workflow Step 4） | - |
| `test-runner` | テスト実行・失敗分析 | テスト実行時、CI失敗時 | - |
| `codebase-explorer` | コードベース探索 | 型・関数・パターンの検索 | **必須** |
| `code-reviewer` | 詳細コードレビュー | PRレビュー、実装完了時（review/に出力） | - |
| `security-reviewer` | セキュリティ脆弱性検出 | API/認証変更時、本番デプロイ前（review/に出力） | - |
| `spec-manager` | Plan/Spec整合性管理 | 計画と実装の乖離確認 | **必須** |
| `ws-debugger` | WebSocket専門デバッグ | 接続問題、Heartbeat、RateLimit分析 | WS関連時 |
| `risk-gate-analyzer` | Risk Gate分析 | Gate発火条件・履歴分析 | Risk関連時 |

### Usage Examples

```
# Rust Code Save Workflow
Task(rust-builder): "fmt/clippy/checkを実行"
Task(code-simplifier): "crates/hip3-executor/src/batch.rsを簡素化提案"

# Testing
Task(test-runner): "cargo test --workspaceを実行して結果を要約"

# Exploration
Task(codebase-explorer): "MarketKeyの定義と使用箇所を検索"

# Review
Task(code-reviewer): "crates/hip3-executor/src/batch.rsをレビュー"

# Security Review
Task(security-reviewer): "crates/hip3-executor/src/signer.rsのセキュリティレビュー"

# Spec Management
Task(spec-manager): ".claude/plans/と.claude/specs/の整合性を確認"

# Domain-Specific Debug
Task(ws-debugger): "hip3-wsの接続管理とHeartbeat実装を調査"
Task(risk-gate-analyzer): "Risk Gateの発火条件を一覧化"
```

### Agent Configuration

All agents are configured with:
- **model**: opus
- **think**: on (extended thinking enabled)

Agent definition files: `.claude/agents/*.md`

---

## VPS Deployment (Production)

### Connection Info

| Item | Value |
|------|-------|
| **IP Address** | `5.104.81.76` |
| **Provider** | Contabo |
| **OS** | Ubuntu 22.04.5 LTS |
| **User** | `root` |
| **Password** | `RD3lDP8x8Xa2vQ3pVWwZ9dAr0` |
| **Deploy Path** | `/opt/hip3-bot` |

### SSH Commands

```bash
# Quick SSH
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76

# Check logs
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76 \
  "docker compose -f /opt/hip3-bot/docker-compose.yml logs --tail 50"

# Container status
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76 \
  "docker compose -f /opt/hip3-bot/docker-compose.yml ps"
```

### Deployment Workflow

```bash
# 1. Push to GitHub
git push origin master

# 2. SSH to VPS and update
sshpass -p 'RD3lDP8x8Xa2vQ3pVWwZ9dAr0' ssh root@5.104.81.76 << 'EOF'
cd /opt/hip3-bot
git pull
docker compose build
docker compose up -d
docker compose logs --tail 20
EOF
```

### Dashboard Access

| Item | Value |
|------|-------|
| **URL** | `http://5.104.81.76:8080` |
| **WebSocket** | `ws://5.104.81.76:8080/ws` |

---

## Project-Specific Rules

This project inherits rules from the parent `/Users/taka/crypto_trading_bot/CLAUDE.md`.

See parent CLAUDE.md for:
- Python/TypeScript development rules
- Database rules (PostgreSQL, TimescaleDB, Redis)
- Docker container rules
- Trading strategy rules
- WebSocket rules
