import test from "node:test";
import assert from "node:assert/strict";
import { parseThemeMode, resolveTheme } from "./theme.ts";

test("resolveTheme follows system in system mode", () => {
  assert.equal(resolveTheme("system", true), "dark");
  assert.equal(resolveTheme("system", false), "light");
});

test("resolveTheme ignores system when overridden", () => {
  assert.equal(resolveTheme("light", true), "light");
  assert.equal(resolveTheme("dark", false), "dark");
});

test("parseThemeMode validates stored values", () => {
  assert.equal(parseThemeMode("dark"), "dark");
  assert.equal(parseThemeMode("light"), "light");
  assert.equal(parseThemeMode("system"), "system");
  assert.equal(parseThemeMode(null), "system");
  assert.equal(parseThemeMode("nonsense"), "system");
});
