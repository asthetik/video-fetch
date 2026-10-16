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
// shows a clear stand-in placeholder instead.

test("empty save_dir with a session effective dir shows the stand-in placeholder", () => {
  assert.equal(saveDirPlaceholder(""), "（本次会话使用系统默认下载目录）");
});

test("a configured save_dir is the input value and gets no placeholder", () => {
  assert.equal(saveDirPlaceholder("/Users/me/Downloads"), "");
});

test("the placeholder is a stand-in label, never a melted path", () => {
  const text = saveDirPlaceholder("");
  assert.ok(text.length > 0);
  // U+FFFD is what to_string_lossy melts a raw OS path into; the whole point
  // of the placeholder is to avoid surfacing such a melt as a path.
  assert.ok(!text.includes("\u{FFFD}"));
  // Parenthetical prose, not something a user could mistake for a path.
  assert.ok(!text.includes("/"));
});

test("settings save-dir input renders the placeholder through the helper", () => {
  // A pure-helper test cannot see the input: deleting the placeholder wiring
  // while leaving saveDirPlaceholder() on disk would stay green. Read
  // SettingsPage so removing the wiring turns this red.
  const settings = readFileSync(
    join(import.meta.dirname, "..", "pages", "SettingsPage.tsx"),
    "utf8",
  );
  assert.match(settings, /placeholder=\{saveDirPlaceholder\(/);
});
