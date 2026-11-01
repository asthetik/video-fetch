import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Pins the production CSP against the HTML that ships with it. The
// anti-FOUC script must stay external: an inline copy would need
// 'unsafe-inline' (or a byte-exact hash that vite's html re-indentation
// breaks — source and dist bytes were measured to differ on whitespace
// alone), and the dev websocket must live in devCsp, never in csp.

const root = join(import.meta.dirname, "..", "..");
const html = readFileSync(join(root, "index.html"), "utf8");
const conf = JSON.parse(
  readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"),
);
const csp: string = conf.app.security.csp;
const devCsp: string = conf.app.security.devCsp;

function directive(source: string, name: string): string {
  const m = source.match(new RegExp(`${name} ([^;]+)`));
  assert.ok(m, `${name} directive missing`);
  return m[1];
}

test("index.html keeps no inline script body (anti-FOUC lives in an external sync script)", () => {
  assert.doesNotMatch(
    html,
    /<script>\s*\n\S/,
    "an inline classic script would need unsafe-inline or a fragile hash",
  );
  assert.match(
    html,
    /<script src="\/anti-fouc\.js"><\/script>/,
    "the anti-FOUC script must load synchronously before React mounts",
  );
});

test("the external anti-FOUC script exists and still resolves the theme", () => {
  const js = readFileSync(join(root, "public", "anti-fouc.js"), "utf8");
  assert.match(js, /videofetch\.theme/);
  assert.match(js, /prefers-color-scheme/);
});

test("production script-src has no unsafe-inline", () => {
  const scriptSrc = directive(csp, "script-src");
  assert.doesNotMatch(scriptSrc, /unsafe-inline/);
});

test("production connect-src drops the dev websocket", () => {
  const connectSrc = directive(csp, "connect-src");
  assert.doesNotMatch(connectSrc, /ws:\/\//);
});

test("production img-src is allowlisted to the bilibili image CDN", () => {
  const imgSrc = directive(csp, "img-src");
  assert.match(imgSrc, /https:\/\/\*\.hdslb\.com/);
  assert.doesNotMatch(
    imgSrc,
    /(^|\s)https:(\s|$)/,
    "a bare https: token re-opens every image host",
  );
});

test("dev CSP is the production CSP plus only the HMR websocket", () => {
  assert.match(devCsp, /ws:\/\/localhost:\*/);
  assert.equal(
    devCsp.replace(/\s*ws:\/\/localhost:\*/g, ""),
    csp,
    "devCsp must not relax anything except the websocket",
  );
});
