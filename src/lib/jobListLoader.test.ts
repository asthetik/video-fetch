import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { loadJobsFromStore, resetJobListLoaderForTests } from "./jobListLoader.ts";

const QUEUE = join(import.meta.dirname, "..", "components", "DownloadQueue.tsx");
const HISTORY = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");
const queue = readFileSync(QUEUE, "utf8");
const history = readFileSync(HISTORY, "utf8");

// Terminal events trigger reloads from several places at once (progress
// handler, onJobsChanged bump, refreshToken effect). Without coalescing,
// one settled download fires 3-4 identical list_jobs IPC calls. The shared
// loader merges concurrent calls into one request and frees the slot when
// it settles, so the next call always refetches fresh data.

function makeDelayedFetch(delayMs: number, counting: { calls: number }) {
  return async () => {
    counting.calls += 1;
    await new Promise((r) => setTimeout(r, delayMs));
    return [{ id: `job-${counting.calls}` }];
  };
}

test("concurrent loadJobs calls share one in-flight request", async () => {
  resetJobListLoaderForTests();
  const counting = { calls: 0 };
  const fetchJobs = makeDelayedFetch(10, counting);

  const [a, b, c] = await Promise.all([
    loadJobsFromStore(fetchJobs),
    loadJobsFromStore(fetchJobs),
    loadJobsFromStore(fetchJobs),
  ]);

  assert.equal(counting.calls, 1, "three concurrent calls must fire one fetch");
  assert.deepEqual([a.length, b.length, c.length], [1, 1, 1]);
});

test("after settling, the next call refetches fresh data", async () => {
  resetJobListLoaderForTests();
  let generation = 0;
  const fetchJobs = async () => {
    generation += 1;
    return [{ id: `gen-${generation}` }];
  };

  await loadJobsFromStore(fetchJobs);
  const second = await loadJobsFromStore(fetchJobs);

  assert.equal(second[0].id, "gen-2", "no staleness cache: settle frees the slot");
});

test("a failed load rejects every waiter and frees the slot", async () => {
  resetJobListLoaderForTests();
  const failing = async () => {
    throw new Error("db locked");
  };
  await assert.rejects(loadJobsFromStore(failing), /db locked/);
  // The slot must clear on failure too — the next attempt retries.
  const retry = await loadJobsFromStore(async () => [{ id: "retry-ok" }]);
  assert.equal(retry.length, 1);
});

test("both surfaces route their reloads through the shared loader", () => {
  for (const [name, src] of [["queue", queue], ["history", history]] as const) {
    assert.match(
      src,
      /loadJobsFromStore\(async \(\) => api\.listJobs\(\)\)/,
      `${name} must inject api.listJobs into the shared loader (its only listJobs path)`,
    );
  }
});

test("the loader pins its in-flight slot by name", () => {
  const loader_src = readFileSync(
    join(import.meta.dirname, "..", "lib", "jobListLoader.ts"),
    "utf8",
  );
  assert.match(loader_src, /inFlight/);
  assert.match(loader_src, /inFlight = null/);
});
