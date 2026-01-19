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

## Project-Specific Rules

This project inherits rules from the parent `/Users/taka/crypto_trading_bot/CLAUDE.md`.

See parent CLAUDE.md for:
- Python/TypeScript development rules
- Database rules (PostgreSQL, TimescaleDB, Redis)
- Docker container rules
- Trading strategy rules
- WebSocket rules
