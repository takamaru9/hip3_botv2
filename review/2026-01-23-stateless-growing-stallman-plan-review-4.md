# BUG-004 再調査計画レビュー（最新版）

## Findings (ordered)

1) High: Option A のコード例で `resp.error` を参照していますが、後段で定義している `SubscriptionResponse` には `error` フィールドがありません。**コンパイル不能になる不整合**なので、構造体へ `error` を追加するか、エラー判定方法を別途定義する必要があります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:103-119` `.claude/plans/stateless-growing-stallman.md:287-295` `.claude/plans/stateless-growing-stallman.md:334-342`

2) Medium: ACK 失敗時の動作で「即時再接続トリガー」とありますが、**どこで/どうやって再接続を発火させるか**が未定義です。`ConnectionManager` の状態遷移や戻り値（`Err`）でトリガーするのか、明記しないと実装が止まります。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:337-342`

3) Medium: `SubscriptionResponse` の payload 仕様は「要実測」としつつ、同時に固定の struct 定義を提示しています。実測前に struct を固定すると **再修正が前提**になるため、実測後に struct を確定する旨を明記した方が安全です。  
   - 計画該当箇所: `.claude/plans/stateless-growing-stallman.md:297-326` `.claude/plans/stateless-growing-stallman.md:287-295`

## Open Questions

- `subscriptionResponse` の **失敗時フィールド名**（`error`/`message`/`success` など）を実測で確定させた後、どの形で `SubscriptionResponse` に持たせるか。  
- ACK 失敗を **再接続トリガー**にする場合、`ConnectionManager` のどの経路で発火させるか（例: エラー返却で外側ループ再接続）。  

## Suggested plan edits

- `SubscriptionResponse` に `error` を含めるか、**エラー判定ロジックを別関数化**して明記。  
- 再接続トリガーの **具体的な実装経路**（戻り値/状態遷移）を追記。  
- 「実測 → struct 確定 → 実装」順にする旨を Option A の実装手順に明記。  

## Change Summary

- 実装手順の具体性は改善されていますが、`SubscriptionResponse` の不整合と **失敗時ハンドリングの実装経路**がまだ不足しています。

