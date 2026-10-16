import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { saveDirRowHint } from "./saveDirNotice.ts";

// Review #161 minor 3: the "保存目录不可用" startup notice was a 4-second
// toast in App.tsx. Miss it once and the whole session has no trace of why
// downloads land in the app-data fallback, while Settings' 打开 still opens
// the configured directory and errors with no context. These pins keep the
// condition discoverable for the whole session in the save-dir row.

test("saveDirRowHint is set iff the backend notice is set", () => {
  assert.equal(saveDirRowHint(null), null);
  assert.equal(saveDirRowHint(undefined), null);
  assert.equal(saveDirRowHint(""), null);
  const hint = saveDirRowHint("保存目录不可用");
  assert.ok(hint);
  // The hint must keep naming the condition and say where downloads actually
  // go this session and that the configured setting is untouched.
  assert.ok(hint.includes("保存目录不可用"));
  assert.match(hint, /应用数据目录/);
  assert.match(hint, /未更改/);
});

test("settings save-dir row renders the hint whenever the notice is set", () => {
  // A pure-helper test cannot see the row: deleting the hint rendering while
  // leaving saveDirRowHint() on disk would stay green. Read SettingsPage so
  // removing the hint turns this red.
  const settings = readFileSync(
    join(import.meta.dirname, "..", "pages", "SettingsPage.tsx"),
    "utf8",
  );
  // The page must read the session notice...
  assert.match(settings, /getSaveDirNotice\(/);
  // ...decide the hint through the helper...
  assert.match(settings, /saveDirRowHint\(/);
  // ...and render the hint inside the save-dir row, not elsewhere. The
  // hint must sit directly inside the conditional — substring pins
  // satisfiable by dead remnants are the failure mode this guards against.
  const saveDirSection = settings.slice(settings.indexOf("保存目录"));
  assert.match(
    saveDirSection,
    /\{saveDirHint \? <p className="settings-hint warn">/,
    "the hint element must render inside the saveDirHint conditional",
  );
  // ...and re-derive it whenever the save dir changes, so the backend's
  // live gate (override dropped => None) actually clears the hint.
  assert.match(
    settings,
    /\}, \[settings\?\.save_dir\]\);/,
    "the notice effect must re-run when save_dir changes",
  );
});

test("the startup toast is kept alongside the persistent hint", () => {
  // The hint supplements the toast; it does not replace it.
  const app = readFileSync(
    join(import.meta.dirname, "..", "App.tsx"),
    "utf8",
  );
  assert.match(app, /getSaveDirNotice\(/);
  assert.match(app, /toast\.warning\(/);
});
