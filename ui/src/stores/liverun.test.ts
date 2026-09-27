import assert from "node:assert/strict";
import { test } from "node:test";
import { applyDone, applySnapshot, type LiveRun } from "./liveRun.ts";

function run(runId: string, live: boolean, snapshots = 0): LiveRun {
  return {
    runId,
    name: runId,
    live,
    snapshots: Array.from({ length: snapshots }, () => ({}) as never),
    summary: null,
    annotation: {},
  };
}

const snapshot = {} as never;
const summary = { runId: "x" } as never;

test("a snapshot reaches the run being watched", () => {
  const state = { live: run("a", true), current: run("a", true) };
  const patch = applySnapshot(state, "a", snapshot);
  assert.equal(patch.live?.snapshots.length, 1);
  assert.equal(patch.current?.snapshots.length, 1);
});

test("a live run keeps recording while something else is on screen", () => {
  // The bug this exists for: looking at an old report used to discard the
  // running test's snapshots, leaving a hole in its timeline.
  const state = { live: run("a", true, 3), current: run("old", false) };
  const patch = applySnapshot(state, "a", snapshot);
  assert.equal(patch.live?.snapshots.length, 4, "the live run lost a snapshot");
  assert.equal(patch.current, undefined, "the viewed run must not be touched");
});

test("a snapshot for neither run changes nothing", () => {
  const state = { live: run("a", true), current: run("old", false) };
  assert.deepEqual(applySnapshot(state, "somebody-else", snapshot), {});
});

test("a snapshot still lands with no live run at all", () => {
  // Replaying a finished run from history: there is no live run, but the
  // engine may still be draining events for the one being watched.
  const state = { live: null, current: run("a", true) };
  const patch = applySnapshot(state, "a", snapshot);
  assert.equal(patch.current?.snapshots.length, 1);
  assert.equal(patch.live, undefined);
});

test("finishing marks the run done in both places at once", () => {
  const state = { live: run("a", true, 2), current: run("a", true, 2) };
  const patch = applyDone(state, "a", summary);
  assert.equal(patch.live?.live, false);
  assert.equal(patch.current?.live, false);
  assert.equal(patch.live?.summary, summary);
});

test("finishing an unwatched run still completes it", () => {
  const state = { live: run("a", true, 2), current: run("old", false) };
  const patch = applyDone(state, "a", summary);
  assert.equal(patch.live?.live, false, "the run must finish even unwatched");
  assert.equal(patch.current, undefined);
});

test("the accumulated snapshots survive the run finishing", () => {
  const state = { live: run("a", true, 7), current: run("old", false) };
  const patch = applyDone(state, "a", summary);
  assert.equal(patch.live?.snapshots.length, 7);
});
