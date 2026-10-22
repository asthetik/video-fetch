import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { videoFormatsHint } from "./videoFormatsHint.ts";

// An audio-only resolve leaves the video tab with an empty, disabled format
// select and a greyed download button, with nothing explaining why. The hint
// must send the user to the tab that can actually download, and the both-empty
// case needs its own wording rather than the same instruction.

test("a video with formats needs no hint", () => {
  assert.equal(videoFormatsHint(3, 2), null);
});

test("an audio-only result points at the audio tab", () => {
  assert.equal(
    videoFormatsHint(0, 2),
    "该视频仅提供音频格式，请切换到「仅音频」",
  );
});

test("a result with neither kind says so", () => {
  assert.equal(videoFormatsHint(0, 0), "该视频暂无可下载的清晰度");
});

// Source-level wiring guard: node tests cannot render React, so pin that the
// video tab actually renders the hint through the decision above.
const VIDEO_CARD = join(import.meta.dirname, "..", "components", "VideoCard.tsx");
const card = readFileSync(VIDEO_CARD, "utf8");

test("VideoCard renders the video-tab hint through videoFormatsHint", () => {
  assert.match(
    card,
    /videoFormatsHint\(\s*meta\.formats\.length\s*,\s*meta\.audio_formats\.length\s*,?\s*\)/,
    "the video tab must classify its empty state through videoFormatsHint",
  );
  assert.match(
    card,
    /!formatsLoading\s*&&\s*!formatsError\s*&&\s*formatsHint\s*&&\s*\(\s*<p className="url-hint">/,
    "the hint must render only after loading settles and without an error row",
  );
});
