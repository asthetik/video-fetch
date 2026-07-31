import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  createPageTransitionState,
  reducePageTransition,
} from "./pageTransition";

describe("reducePageTransition", () => {
  it("starts exiting when target differs while idle", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "history" });
    assert.equal(s.phase, "exiting");
    assert.equal(s.displayedPage, "home");
    assert.equal(s.pendingPage, "history");
  });

  it("ignores target equal to displayed while idle", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "home" });
    assert.deepEqual(s, createPageTransitionState("home"));
  });

  it("exitDone switches displayed and enters", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "settings" });
    s = reducePageTransition(s, { type: "exitDone" });
    assert.equal(s.displayedPage, "settings");
    assert.equal(s.phase, "entering");
    assert.equal(s.pendingPage, null);
  });

  it("enterDone returns to idle", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "about" });
    s = reducePageTransition(s, { type: "exitDone" });
    s = reducePageTransition(s, { type: "enterDone" });
    assert.equal(s.phase, "idle");
    assert.equal(s.displayedPage, "about");
  });

  it("records latest pending while exiting", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "history" });
    s = reducePageTransition(s, { type: "target", page: "about" });
    assert.equal(s.phase, "exiting");
    assert.equal(s.displayedPage, "home");
    assert.equal(s.pendingPage, "about");
  });

  it("after enterDone, follows new pending with another exit", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "history" });
    s = reducePageTransition(s, { type: "target", page: "settings" });
    s = reducePageTransition(s, { type: "exitDone" });
    // pending was settings → displayed settings, entering
    assert.equal(s.displayedPage, "settings");
    assert.equal(s.phase, "entering");
    s = reducePageTransition(s, { type: "enterDone" });
    // no further pending → idle
    assert.equal(s.phase, "idle");
  });

  it("while entering, new target becomes pending then exits after enterDone", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "history" });
    s = reducePageTransition(s, { type: "exitDone" });
    s = reducePageTransition(s, { type: "target", page: "about" });
    assert.equal(s.phase, "entering");
    assert.equal(s.displayedPage, "history");
    assert.equal(s.pendingPage, "about");
    s = reducePageTransition(s, { type: "enterDone" });
    assert.equal(s.phase, "exiting");
    assert.equal(s.displayedPage, "history");
    assert.equal(s.pendingPage, "about");
  });

  it("skipAnimations jumps to target idle", () => {
    let s = createPageTransitionState("home");
    s = reducePageTransition(s, { type: "target", page: "history" });
    s = reducePageTransition(s, { type: "skipAnimations", page: "about" });
    assert.deepEqual(s, {
      displayedPage: "about",
      phase: "idle",
      pendingPage: null,
    });
  });
});
