# Review Findings Fix Plan レビュー（再確認 2）

対象: `.claude/plans/2026-01-24-review-findings-fix.md`

## Findings (ordered)

1) High: F1 のテストが実装のアクセス範囲と一致しません。`PositionTrackerHandle::positions_data.insert()` は外部クレートから触れない private フィールドで、`Position::new` も timestamp 引数が必要です。現状のままではコンパイル不能なので、`position_tracker.fill(...).await` でポジションを作るか、テスト専用の公開ヘルパーを追加する方針に修正が必要です。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:175-178`, `.claude/plans/2026-01-24-review-findings-fix.md:209-211`, `.claude/plans/2026-01-24-review-findings-fix.md:266-268`

2) Medium: F1 テストに未定義のヘルパーが複数あります（`setup_executor_with_tracker`, `sample_market_a/b`, `now_ms`, `sample_tracked_order`）。既存の `setup_executor` / `sample_market` / `sample_market_2` に寄せるか、新規に定義する前提を明記してください。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:185-187`, `.claude/plans/2026-01-24-review-findings-fix.md:194`, `.claude/plans/2026-01-24-review-findings-fix.md:205-240`, `.claude/plans/2026-01-24-review-findings-fix.md:260-281`

3) Medium: F2 のテストで `setup_connection_manager` と `cm.run()` を使っていますが現行 API に存在しません。`ConnectionManager::connect()` を使うか、テスト専用のヘルパー／モックサーバー前提を明記する必要があります。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:428-433`

4) Medium: F3 の `make_order_update` が `OrderInfo` の実フィールドと不一致です。`limit_px` ではなく `px` を使い、`timestamp` は `Option<u64>` なので `Some(...)` で初期化する必要があります。
   - 該当箇所: `.claude/plans/2026-01-24-review-findings-fix.md:637-647`

## Change Summary

- 主要ロジックは固まりましたが、テスト記述が実装 API と合っていない箇所が残っています。特に F1 のポジション追加方法と F2 の接続テストの実行方法を現行構造に合わせる必要があります。
