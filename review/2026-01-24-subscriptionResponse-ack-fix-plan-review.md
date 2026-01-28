# subscriptionResponse ACK パース修正計画レビュー

対象: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md`

## Findings (ordered)

1) High: チャネル名の扱いが「実測後に修正」になっていますが、公式仕様はデータメッセージの `channel` が subscription type そのものと明記しています。現状コードは `starts_with("orderUpdates:")` 前提のため、`orderUpdates` 単体を取りこぼす可能性が高いです。P2 ではなく P0/P1 で **両対応**（`== "orderUpdates"` or `==` + `starts_with`）を入れてから実測で絞る方が安全です。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:74-79`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:173-202`

2) Medium: テスト計画では `handle_text_message` の ACK 処理を検証すると書かれていますが、実装詳細のテストは `extract_subscription_type` のユニットテストのみです。`order_updates_ready` が更新されることを実際のメッセージ経路で検証する統合テストが必要です。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:119-136`, `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:256-333`

3) Medium/Low: `subscriptionResponse` で Ready を立てる際に `data.method == "subscribe"` のガードがありません。もし `unsubscribe` や error 形式が同チャネルで来た場合、誤って Ready を立てる恐れがあります。最低限 `method` をチェックする方が安全です。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:226-245`

4) Low: 参照元に Python SDK が挙げられていますが、該当ファイル/行や根拠が書かれていません。後から検証できるよう、どの実装を参照したか（パス/コミット/行）を明記した方がよいです。
   - 該当箇所: `.claude/plans/2026-01-24-subscriptionResponse-ack-fix.md:16-21`

## Change Summary

- ACK 形式の修正方針は妥当だが、チャネル名の取り扱いとテスト粒度のギャップが残っています。上記 4 点の調整で計画の確度が上がります。
