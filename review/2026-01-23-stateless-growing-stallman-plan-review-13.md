# BUG-004 再調査計画レビュー（正しいパス再確認）

対象: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md`

## Findings (ordered)

1) Low: 「Phase 1: 成功時の形式を確認（`subscription` フィールドの構造）」という表現が、直前の方針（**data 自体が subscription**）とズレています。  
   - `subscription` フィールド前提に読めるため、`data の構造確認` へ修正したほうが一貫します。  
   - 計画該当箇所: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:239-241`

2) Low: 「ACK 成功/失敗の判定基準」が実測前にも適用されるように読めます。Step 2b と同様に **“実測後の判定基準”** と明記すると混乱が減ります。  
   - 計画該当箇所: `/Users/taka/crypto_trading_bot/hip3_botv2/.claude/plans/stateless-growing-stallman.md:272-285`

## Resolved vs prior blockers

- `WsError::SubscriptionError` を使用する方針になっており、**未定義バリアント問題は解消**。  
- `drain_and_wait()` でエラー伝播する設計が明記され、**ACK失敗→再接続の経路が明確化**。  
- Step 2a/2b 分離と `data.type` 前提の統一で、**実測前ロジックの過剰具体化が解消**。  

## Change Summary

- 重大ブロッカーは解消済み。残りは文面の一貫性の微修正のみです。

