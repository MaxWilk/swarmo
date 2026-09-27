import assert from "node:assert/strict";
import { test } from "node:test";
import { isDirtyPair } from "./dirty.ts";

test("equal content is clean, different content is dirty", () => {
  const saved = { url: "http://x", body: "a" };
  assert.equal(isDirtyPair({ url: "http://x", body: "a" }, saved), false);
  assert.equal(isDirtyPair({ url: "http://x", body: "b" }, saved), true);
});

test("the answer is cached per (edited, saved) identity, not recomputed", () => {
  // A same-content object at a new identity is a new pair and is compared;
  // the same pair asked twice is answered from the cache.
  const saved = { body: "x".repeat(1000) };
  const edited = { body: "x".repeat(1000) };
  const big = { toJSON: () => { throw new Error("must not serialise again"); } };
  assert.equal(isDirtyPair(edited, saved), false);
  // Mutating in place is not how the store works, and the cache is keyed on
  // identity — so this stays "clean" until a *new* def object arrives.
  (edited as { body: string }).body = "changed";
  assert.equal(isDirtyPair(edited, saved), false);
  assert.equal(isDirtyPair({ ...edited }, saved), true);
  void big;
});

test("a new saved copy invalidates the cached answer for the same def", () => {
  const def = { body: "b" };
  assert.equal(isDirtyPair(def, { body: "a" }), true);
  assert.equal(isDirtyPair(def, { body: "b" }), false);
});
