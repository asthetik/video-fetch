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
    /!loading && loadError &&\n        \(/,
    "the error block must show whenever a load failed and loading has settled",
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

test("both load paths clear the error right where the load succeeds", () => {
  assert.match(
    queue,
    /setJobs\(sortJobs\(list\)\);\n      setLoadError\(null\);/,
    "the queue must clear the error on the success path itself, not in some retry handler",
  );
  assert.match(
    history,
    /setAllJobs\(list\);\n      setLoadError\(null\);/,
    "history must clear the error on the success path itself, not in some retry handler",
  );
});

test("with data on screen a failed refresh degrades to a banner, not a fake page", () => {
  for (const [name, src] of [
    ["queue", queue],
    ["history", history],
  ] as const) {
    assert.match(
      src,
      /列表刷新失败：\{loadError\}/,
      `${name} must show a slim refresh banner when data is already on screen`,
    );
    assert.match(
      src,
      /data-action="retry-load"\n\s+onClick=\{retryLoadJobs\}/,
      `${name}'s banner retry must be wired to the retry callback`,
    );
    assert.match(
      src,
      /const retryLoadJobs = useCallback\(\(\) => \{\n    setLoading\(true\);\n    void loadJobs\(\);/,
      `${name}'s retry must reset loading first so clicks cannot stack concurrent loads`,
    );
    assert.match(
      src,
      /onAction=\{retryLoadJobs\}/,
      `${name}'s empty-state retry must be wired to the retry callback`,
    );
  }
});

test("the filter empty state does not masquerade over a failed refresh", () => {
  assert.match(
    history,
    /\{!loading &&\n        visibleJobs\.length > 0 &&\n        !loadError &&\n        !showActive &&\n        groups\.length === 0 && \(/,
    "没有匹配的记录 must carry the !loadError guard in its own condition",
  );
});
