import assert from "node:assert/strict";
import { test } from "node:test";
import { applyRename } from "./tabRename.ts";
import type { Tab } from "./tabs.ts";

function tab(nodeRef: string, name: string): Tab {
  return {
    kind: "http",
    nodeRef,
    name,
    def: { name, url: "https://a.test" },
    saved: { name, url: "https://a.test" },
    subTab: "params",
    result: null,
    sending: false,
    execId: null,
  } as unknown as Tab;
}

test("a renamed request takes its new name everywhere it is held", () => {
  const before = {
    tabs: [tab("collections/C/Old.req.json", "Old")],
    activeRef: "collections/C/Old.req.json",
  };
  const after = applyRename(before, "collections/C/Old.req.json", "collections/C/New.req.json", "New");
  const t = after.tabs[0];

  assert.equal(t.nodeRef, "collections/C/New.req.json");
  assert.equal(t.name, "New");
  // The one that actually matters: saving writes `def`, so a stale name here
  // would put the old name straight back into the file.
  assert.equal(t.def.name, "New");
  // And `saved` too, or the tab reads as unsaved the instant it is renamed.
  assert.equal(t.saved.name, "New");
  assert.equal(after.activeRef, "collections/C/New.req.json");
});

test("a renamed tab does not become dirty", () => {
  const before = { tabs: [tab("a/Old.req.json", "Old")], activeRef: null };
  const after = applyRename(before, "a/Old.req.json", "a/New.req.json", "New");
  const t = after.tabs[0];
  assert.deepEqual(t.def, t.saved, "renaming should not look like an unsaved edit");
});

test("other tabs are left alone", () => {
  const before = {
    tabs: [tab("a/One.req.json", "One"), tab("a/Two.req.json", "Two")],
    activeRef: "a/Two.req.json",
  };
  const after = applyRename(before, "a/One.req.json", "a/Renamed.req.json", "Renamed");

  assert.equal(after.tabs[1].nodeRef, "a/Two.req.json");
  assert.equal(after.tabs[1].name, "Two");
  assert.equal(after.activeRef, "a/Two.req.json", "the active tab should not move");
});

test("renaming a folder moves the tabs inside it", () => {
  // A ref is a path, so a request inside a renamed folder is at a new path
  // too. Left stale, saving it would write into a folder that is gone.
  const before = {
    tabs: [
      tab("collections/Old Folder/Get.req.json", "Get"),
      tab("collections/Elsewhere/Post.req.json", "Post"),
    ],
    activeRef: "collections/Old Folder/Get.req.json",
  };
  const after = applyRename(before, "collections/Old Folder", "collections/New Folder", "");

  assert.equal(after.tabs[0].nodeRef, "collections/New Folder/Get.req.json");
  // The request keeps its own name; only the folder was renamed.
  assert.equal(after.tabs[0].name, "Get");
  assert.equal(after.tabs[0].def.name, "Get");
  assert.equal(after.tabs[1].nodeRef, "collections/Elsewhere/Post.req.json");
  assert.equal(after.activeRef, "collections/New Folder/Get.req.json");
});

test("a folder whose name merely prefixes another is not caught", () => {
  // "Api" must not match "Api v2": only a full path segment counts.
  const before = { tabs: [tab("collections/Api v2/Get.req.json", "Get")], activeRef: null };
  const after = applyRename(before, "collections/Api", "collections/Renamed", "");
  assert.equal(after.tabs[0].nodeRef, "collections/Api v2/Get.req.json");
});

test("moving a request to another collection keeps its name", () => {
  // A move passes no new name; the request is the same request.
  const before = { tabs: [tab("collections/A/Get.req.json", "Get")], activeRef: null };
  const after = applyRename(before, "collections/A/Get.req.json", "collections/B/Get.req.json", "");
  assert.equal(after.tabs[0].nodeRef, "collections/B/Get.req.json");
  assert.equal(after.tabs[0].name, "Get");
  assert.equal(after.tabs[0].def.name, "Get");
});

test("a rename with nothing open changes nothing", () => {
  const after = applyRename({ tabs: [], activeRef: null }, "a", "b", "B");
  assert.deepEqual(after.tabs, []);
  assert.equal(after.activeRef, null);
});

test("the tabs it was given are not mutated", () => {
  const before = { tabs: [tab("a/Old.req.json", "Old")], activeRef: null };
  applyRename(before, "a/Old.req.json", "a/New.req.json", "New");
  assert.equal(before.tabs[0].name, "Old");
  assert.equal(before.tabs[0].def.name, "Old", "the caller's tab was modified");
});
