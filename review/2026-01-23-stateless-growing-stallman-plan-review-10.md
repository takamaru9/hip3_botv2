# BUG-004 再調査計画レビュー（追加指摘）

## Findings (ordered)

1) Low: `subscriptionResponse` の data 形状の前提が **計画内で矛盾**しています。  
   - Step 2a の暫定ロジックは `channel_msg.data.get("subscription")` を前提にしていますが、  
     `subscriptionResponse` の説明では「data は元の subscription を返す」と記載されています。  
   - どちらかに統一しないと、実装時に ACK 判定条件がブレます。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:151` `.claude/plans/stateless-growing-stallman.md:226` `.claude/plans/stateless-growing-stallman.md:276`

## Suggested plan edits

- **案A（docs記載に合わせる）**: `subscriptionResponse.data` を subscription 本体として扱う  
  - Step 2a を `channel_msg.data.get("type")` ベースに修正  
  - ACK 成功判定も `data.type == "orderUpdates"` に統一

- **案B（実測で data.subscription がある場合）**: data が `{ subscription: {...} }` 形式であることを明記  
  - 「data は元の subscription を返す」の記述を修正  
  - Step 2a/判定基準を `data.subscription` 前提で統一

## Change Summary

- 残っている問題は **文面の前提不一致のみ**で、実装上の重大なブロッカーではありません。

