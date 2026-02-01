# Core Philosophy Addition to CLAUDE.md

## Date
2026-02-01

## Change Type
Documentation Enhancement

## Summary
Added the core trading philosophy to the project's CLAUDE.md as the first section after the title.

## Added Content

```markdown
## Trading Philosophy (Core Principle - 絶対遵守)

**戦略の本質:**

> **正しいエッジ**: オラクルが動いた後、マーケットメーカーの注文が追従していない「取り残された流動性」を取る

This is the fundamental edge of HIP-3 strategy:
- Oracle moves first (price discovery)
- Market maker quotes lag behind (stale liquidity)
- We capture the difference before quotes update

**すべての設計・実装判断はこの原則に基づくこと。**
```

## Rationale
This principle is the foundational concept of the HIP-3 trading strategy. All design decisions, feature implementations, and code reviews should be evaluated against this core principle.

## Related Changes
The `oracle_direction_filter` feature recently added to `detector.rs` and `config.rs` directly implements this philosophy:
- Buy signals: Only when oracle is rising (stale ask from before oracle rise)
- Sell signals: Only when oracle is falling (stale bid from before oracle fall)

This ensures we only trade "stale liquidity" and not "oracle lag" scenarios.

## File Modified
- `/Users/taka/crypto_trading_bot/hip3_botv2/CLAUDE.md`

## Location
Added as the first section after the title, before "Plan Mode Settings".
