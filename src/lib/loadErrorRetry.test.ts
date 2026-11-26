import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// A failed list load used to surface as the regular empty state (暂无下载
// 任务 / 还没有下载记录) with only a transient toast — the empty state lied.
// Both surfaces now render the designed EmptyState error variant with a
// retry button, and their empty states are guarded on !loadError. Node
// tests cannot mount React, so the wiring is pinned in source.
const QUEUE = join(import.meta.dirname, "..", "components", "DownloadQueue.tsx");
const HISTORY = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");
const queue = readFileSync(QUEUE, "utf8");
const history = readFileSync(HISTORY, "utf8");

test("the queue renders a persistent load-error state with retry", () => {
  assert.match(
    queue,
    /const \[loadError, setLoadError\] = useState<string \| null>\(null\)/,
    "the queue must hold the load error as state, not just a toast",
  );
  assert.match(
    queue,
    /title="队列加载失败"/,
    "the queue's failed load must render the EmptyState error variant",
  );
  assert.match(
    queue,
    /actionLabel="重试"/,
    "the queue's error state must offer a retry action",
  );
  assert.match(
    queue,
    /!loading && loadError && \(/,
    "the error state must show whenever a load failed and loading has settled",
  );
});

test("the queue's empty state does not masquerade as a failed load", () => {
  assert.match(
    queue,
    /!loadError &&[\s\S]{0,60}<p className="queue-empty">暂无下载任务/,
    "暂无下载任务 must be guarded on !loadError; a failed load is not an empty queue",
  );
});

test("history renders a persistent load-error state with retry", () => {
  assert.match(
    history,
    /const \[loadError, setLoadError\] = useState<string \| null>\(null\)/,
    "history must hold the load error as state, not just a toast",
  );
  assert.match(
    history,
    /title="历史加载失败"/,
    "history's failed load must render the EmptyState error variant",
  );
  assert.match(
    history,
    /visibleJobs\.length === 0 && !loadError/,
    "还没有下载记录 must be guarded on !loadError; a failed load is not an empty history",
  );
});

test("both load paths clear the error on success", () => {
  for (const [name, src] of [
    ["queue", queue],
    ["history", history],
  ] as const) {
    assert.match(
      src,
      /setLoadError\(null\)/,
      `${name} must clear the load error when a load succeeds`,
    );
  }
});
