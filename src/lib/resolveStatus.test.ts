import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  initialResolveStatus,
  reduceResolveStatus,
  shouldDispatchComplete,
  type ResolveStatus,
} from "./resolveStatus.ts";

function loading(): ResolveStatus {
  return reduceResolveStatus(initialResolveStatus, { type: "resolve_started" });
}

describe("reduceResolveStatus", () => {
  it("resolve_rejected (cache-write failure, no formats_failed event) ends formatsLoading and sets formatsError", () => {
    // The cache-write `?` in resolve_url returns Err with no follow-up
    // formats_failed event. The command rejection is the only terminal the UI
    // sees, so it must end the spinner and surface the message in-card.
    const afterPartial = reduceResolveStatus(loading(), { type: "partial" });
    const next = reduceResolveStatus(afterPartial, {
      type: "resolve_rejected",
      message: "写入缓存失败",
      hasCard: true,
    });
    assert.equal(next.formatsLoading, false);
    assert.equal(next.formatsError, "写入缓存失败");
  });

  it("resolve_rejected resets the formats row even when a card is already rendered", () => {
    // Regression pin: the catch branch used to skip the reset behind
    // `if (!metaRef.current)`, so a card rendered from an earlier partial (or a
    // force refresh keeping old meta) spun forever with no error and no retry.
    const afterPartial = reduceResolveStatus(loading(), { type: "partial" });
    const next = reduceResolveStatus(afterPartial, {
      type: "resolve_rejected",
      message: "解析失败",
      hasCard: true,
    });
    assert.equal(next.formatsLoading, false);
    assert.equal(next.formatsError, "解析失败");
    // In-card message only: with a card rendered the page error stays clear.
    assert.equal(next.error, null);
  });

  it("resolve_rejected without a card still carries the message in the page error", () => {
    const next = reduceResolveStatus(loading(), {
      type: "resolve_rejected",
      message: "网络错误",
      hasCard: false,
    });
    assert.equal(next.formatsLoading, false);
    assert.equal(next.formatsError, "网络错误");
    assert.equal(next.error, "网络错误");
  });

  it("resolve_rejected reset is the same with and without a card (unconditional)", () => {
    const withCard = reduceResolveStatus(loading(), {
      type: "resolve_rejected",
      message: "boom",
      hasCard: true,
    });
    const withoutCard = reduceResolveStatus(loading(), {
      type: "resolve_rejected",
      message: "boom",
      hasCard: false,
    });
    assert.equal(withCard.formatsLoading, withoutCard.formatsLoading);
    assert.equal(withCard.formatsError, withoutCard.formatsError);
    assert.equal(withCard.formatsLoading, false);
  });

  it("formats_failed ends formatsLoading with the event message", () => {
    const afterPartial = reduceResolveStatus(loading(), { type: "partial" });
    const next = reduceResolveStatus(afterPartial, {
      type: "formats_failed",
      message: "清晰度获取失败",
    });
    assert.equal(next.formatsLoading, false);
    assert.equal(next.formatsError, "清晰度获取失败");
  });

  it("cache-write dual-fire with a card converges to one terminal state in either order", () => {
    // The cache-write failure path emits resolve://formats_failed AND rejects
    // the resolve promise with the same message. Both signals must fold into
    // the same terminal state regardless of arrival order: spinner off, the
    // message rendered once in the in-card formatsError, and the page error
    // left clear (a card is on screen, so the message must not leak into it).
    const message = "写入缓存失败";
    const afterPartial = reduceResolveStatus(loading(), { type: "partial" });

    const failFirst = reduceResolveStatus(
      reduceResolveStatus(afterPartial, {
        type: "formats_failed",
        message,
      }),
      { type: "resolve_rejected", message, hasCard: true },
    );
    const rejectFirst = reduceResolveStatus(
      reduceResolveStatus(afterPartial, {
        type: "resolve_rejected",
        message,
        hasCard: true,
      }),
      { type: "formats_failed", message },
    );

    assert.deepEqual(failFirst, rejectFirst);
    assert.deepEqual(failFirst, {
      error: null,
      formatsLoading: false,
      formatsError: message,
    });

    // Replaying either signal must not restart the spinner or duplicate the
    // message: the terminal state is stable under double-fire.
    assert.deepEqual(
      reduceResolveStatus(failFirst, { type: "formats_failed", message }),
      failFirst,
    );
    assert.deepEqual(
      reduceResolveStatus(failFirst, {
        type: "resolve_rejected",
        message,
        hasCard: true,
      }),
      failFirst,
    );
  });

  it("cache-write dual-fire without a card converges to one terminal state in either order", () => {
    // Same double signal before any partial: no card is rendered, so the
    // message is also the page error. Either order must land on the same
    // state — the second signal neither restarts the spinner, duplicates the
    // message, nor clears what the first one set.
    const message = "写入缓存失败";

    const failFirst = reduceResolveStatus(
      reduceResolveStatus(loading(), { type: "formats_failed", message }),
      { type: "resolve_rejected", message, hasCard: false },
    );
    const rejectFirst = reduceResolveStatus(
      reduceResolveStatus(loading(), {
        type: "resolve_rejected",
        message,
        hasCard: false,
      }),
      { type: "formats_failed", message },
    );

    assert.deepEqual(failFirst, rejectFirst);
    assert.deepEqual(failFirst, {
      error: message,
      formatsLoading: false,
      formatsError: message,
    });

    assert.deepEqual(
      reduceResolveStatus(failFirst, { type: "formats_failed", message }),
      failFirst,
    );
    assert.deepEqual(
      reduceResolveStatus(failFirst, {
        type: "resolve_rejected",
        message,
        hasCard: false,
      }),
      failFirst,
    );
  });

  it("complete ends formatsLoading and clears formatsError", () => {
    const failed = reduceResolveStatus(loading(), {
      type: "formats_failed",
      message: "old",
    });
    const next = reduceResolveStatus(failed, { type: "complete" });
    assert.equal(next.formatsLoading, false);
    assert.equal(next.formatsError, null);
  });

  it("complete leaves a pre-existing page error untouched", () => {
    // Contract pin: `complete` only settles the formats row. The page-level
    // error is owned by the no-card branch and must survive a complete
    // dispatch instead of being silently cleared.
    const withPageError = reduceResolveStatus(loading(), {
      type: "settled_page_error",
      message: "无效的空间链接",
    });
    const next = reduceResolveStatus(withPageError, { type: "complete" });
    assert.equal(next.error, "无效的空间链接");
    assert.equal(next.formatsLoading, false);
    assert.equal(next.formatsError, null);
  });

  it("partial and formats_progress keep formatsLoading true", () => {
    const partial = reduceResolveStatus(loading(), { type: "partial" });
    assert.equal(partial.formatsLoading, true);
    assert.equal(partial.formatsError, null);
    const progress = reduceResolveStatus(partial, { type: "formats_progress" });
    assert.equal(progress.formatsLoading, true);
  });

  it("resolve_started clears stale errors and restarts the spinner (in-card retry path)", () => {
    const failed = reduceResolveStatus(loading(), {
      type: "resolve_rejected",
      message: "boom",
      hasCard: true,
    });
    const retry = reduceResolveStatus(failed, { type: "resolve_started" });
    assert.equal(retry.formatsLoading, true);
    assert.equal(retry.formatsError, null);
    assert.equal(retry.error, null);
  });

  it("settled_page_error and settled_idle model the no-card branches", () => {
    const idle = reduceResolveStatus(loading(), { type: "settled_idle" });
    assert.deepEqual(idle, initialResolveStatus);
    const pageError = reduceResolveStatus(loading(), {
      type: "settled_page_error",
      message: "无效的空间链接",
    });
    assert.equal(pageError.formatsLoading, false);
    assert.equal(pageError.formatsError, null);
    assert.equal(pageError.error, "无效的空间链接");
  });
});

describe("shouldDispatchComplete", () => {
  it("video formats non-empty → true", () => {
    assert.equal(
      shouldDispatchComplete({
        formats: [{ format_id: "1", label: "1080p" }],
        audio_formats: [],
      }),
      true,
    );
  });

  it("video empty + audio non-empty → true (audio-only content must not leave the spinner stuck)", () => {
    // Cache hits return Ok(meta) with NO events. A resolve that yielded only
    // audio formats must still dispatch complete here, or the formats spinner
    // never ends and the download button stays disabled.
    assert.equal(
      shouldDispatchComplete({
        formats: [],
        audio_formats: [{ format_id: "a1", label: "FLAC" }],
      }),
      true,
    );
  });

  it("both empty → false (degenerate meta is left to complete / formats_failed events)", () => {
    assert.equal(
      shouldDispatchComplete({ formats: [], audio_formats: [] }),
      false,
    );
  });
});
