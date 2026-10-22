import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The card is rendered from a resolved meta, but the URL bar keeps accepting
// edits. Enqueueing or refreshing through the live input binds that input to
// the meta's video id/title: within the resolve debounce window — or after the
// input was cleared — the backend downloads one video while the row records
// another (it downloads by URL and dedups by id). The card must use the URL
// the meta was resolved from.

const VIDEO_CARD = join(import.meta.dirname, "..", "components", "VideoCard.tsx");
const HOME_PAGE = join(import.meta.dirname, "..", "pages", "HomePage.tsx");

const card = readFileSync(VIDEO_CARD, "utf8");
const home = readFileSync(HOME_PAGE, "utf8");

test("VideoCard enqueues the resolved identity URL", () => {
  assert.match(
    card,
    /url:\s*meta\.webpage_url\s*,/,
    "api.enqueue must carry meta.webpage_url, not the live input",
  );
});

test("VideoCard does not take the live input URL at all", () => {
  assert.ok(
    !/interface VideoCardProps[\s\S]*?\n\s*url:\s*string;/.test(card),
    "the url prop is the live input; the card must not have access to it",
  );
});

test("HomePage renders the card without the live input URL", () => {
  assert.ok(
    !/<VideoCard[\s\S]*?url=\{url\.trim\(\)\}/.test(home),
    "passing url.trim() re-binds the card to whatever the input holds now",
  );
});

test("HomePage refreshes the card through its own URL", () => {
  assert.match(
    home,
    /onRefresh=\{\(\)\s*=>\s*void handleResolve\(meta\.webpage_url,\s*true\)\}/,
    "refresh must re-resolve the card's identity URL, not the live input",
  );
});
