import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { claimSurvivesDiscard } from "./resolveClaim.ts";

// A backend id claim settles after one IPC round-trip, and the comparison
// against requestIdRef alone cannot tell "superseded by a newer claim"
// from "discarded mid-window": discard bumps the ref itself, so the fresh
// id can come back equal to the bumped ref. The discard epoch closes that
// hole; resolveClaim's behavior lives here and HomePage's wiring is pinned
// in source below (Node tests cannot mount React).

test("a claim with no competing resolve or discard survives", () => {
  assert.equal(claimSurvivesDiscard(6, 5, 1, 1), true);
});

test("a claim superseded by a newer one is dropped", () => {
  assert.equal(claimSurvivesDiscard(5, 6, 1, 1), false);
});

test("a claim settled after its discard is dropped even though the id matches the bumped ref", () => {
  // Discard bumped the ref from 5 to 6; the in-flight claim then returned
  // exactly 6. Id comparison alone would let this discarded resolve run.
  assert.equal(claimSurvivesDiscard(6, 6, 1, 2), false);
});

test("a claim from an older discard epoch is dropped regardless of ids", () => {
  assert.equal(claimSurvivesDiscard(9, 6, 3, 4), false);
});

const HOME = join(import.meta.dirname, "..", "pages", "HomePage.tsx");
const homeSrc = readFileSync(HOME, "utf8")
  .replace(/\/\*[\s\S]*?\*\//g, "")
  .replace(/^\s*\/\/.*$/gm, "");

test("HomePage vets the settled claim through claimSurvivesDiscard", () => {
  assert.match(
    homeSrc,
    /claimSurvivesDiscard\(/,
    "the claim must be vetted after it settles, not just compared",
  );
});

test("discard advances the epoch that vets claims", () => {
  assert.match(
    homeSrc,
    /discardEpochRef\.current \+= 1/,
    "discard must change the epoch or the mid-window claim resurrects",
  );
});

test("a failed claim does not become an unhandled rejection", () => {
  assert.match(
    homeSrc,
    /try\s*\{\s*requestId = await api\.nextResolveId\(\);/,
    "the claim must be caught; UrlBar runs this with void/no catch",
  );
});
