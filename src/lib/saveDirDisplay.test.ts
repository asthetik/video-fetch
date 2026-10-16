import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { saveDirPlaceholder } from "./saveDirDisplay.ts";

// Review #161 minor 5: on a first run whose default save dir is not
// representable as a String (non-UTF-8 OS path), A7 keeps the path out of
// settings instead of melting it into U+FFFD — and the settings save-dir
// input then rendered an empty path, which reads as "not configured" while
// the session really downloads to the system default directory. The input
// shows a clear stand-in placeholder instead — except when the
// unavailability hint is shown (the session fell back to the app-data dir,
// which the stand-in's wording would contradict).

test("empty save_dir with no session notice shows the stand-in placeholder", () => {
  assert.equal(
    saveDirPlaceholder("", false),
    "（本次会话使用系统默认下载目录）",
  );
});

test("empty save_dir with the unavailability notice shown gets no placeholder", () => {
  // The hint below the field reports the app-data fallback; a "system
  // default" stand-in above it would claim the opposite.
  assert.equal(saveDirPlaceholder("", true), "");
});

test("a non-empty save_dir gets no placeholder", () => {
  assert.equal(saveDirPlaceholder("/Users/me/Downloads", false), "");
  assert.equal(saveDirPlaceholder("/Users/me/Downloads", true), "");
});

test("settings save-dir input renders the placeholder through the helper", () => {
  // A pure-helper test cannot see the input: deleting the placeholder wiring
  // while leaving saveDirPlaceholder() on disk would stay green. Read
  // SettingsPage so removing the wiring — or reverting it to the
  // notice-blind call — turns this red.
  const settings = readFileSync(
    join(import.meta.dirname, "..", "pages", "SettingsPage.tsx"),
    "utf8",
  );
  assert.match(
    settings,
    /placeholder=\{saveDirPlaceholder\(settings\.save_dir, saveDirHint !== null\)\}/,
  );
});
