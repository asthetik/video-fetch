import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { AUTHOR_URL, LICENSE_URL, REPO_URL, THIRD_PARTY_URL, releaseUrl } from "./links.ts";

test("releaseUrl builds the v-prefixed release-tag URL", () => {
  assert.equal(releaseUrl("0.4.0"), "https://github.com/asthetik/video-fetch/releases/tag/v0.4.0");
});

test("link constants point at the repository's real resources", () => {
  assert.equal(REPO_URL, "https://github.com/asthetik/video-fetch");
  assert.equal(AUTHOR_URL, "https://github.com/asthetik");
  assert.equal(LICENSE_URL, "https://github.com/asthetik/video-fetch/blob/main/LICENSE");
  assert.equal(THIRD_PARTY_URL, "https://github.com/asthetik/video-fetch/blob/main/THIRD_PARTY.md");
});

// release.yml:48 only publishes tags matching ^v[0-9]+\.[0-9]+\.[0-9]+$ (checked
// against package.json), so the composed link must follow the same scheme.
test("package version composes into the tag scheme release.yml accepts", () => {
  const pkg = JSON.parse(readFileSync(join(import.meta.dirname, "..", "..", "package.json"), "utf8")) as {
    version: string;
  };
  assert.match(
    releaseUrl(pkg.version),
    /^https:\/\/github\.com\/asthetik\/video-fetch\/releases\/tag\/v[0-9]+\.[0-9]+\.[0-9]+$/,
  );
});
