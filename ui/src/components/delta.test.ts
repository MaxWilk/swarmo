import assert from "node:assert/strict";
import { test } from "node:test";
import { fileStamp, fmtDelta, fmtDuration } from "./format.ts";

test("a lower-is-better metric going down is an improvement", () => {
  const d = fmtDelta(100, 80, "lower");
  assert.equal(d.cls, "ok");
  assert.equal(d.percent, "−20.0%");
});

test("a lower-is-better metric going up is a regression", () => {
  const d = fmtDelta(100, 130, "lower");
  assert.equal(d.cls, "err");
  assert.equal(d.percent, "+30.0%");
});

test("a higher-is-better metric reads the other way round", () => {
  assert.equal(fmtDelta(100, 130, "higher").cls, "ok");
  assert.equal(fmtDelta(100, 80, "higher").cls, "err");
});

test("a metric with no better direction is never coloured", () => {
  assert.equal(fmtDelta(100, 130, "none").cls, "neutral");
  assert.equal(fmtDelta(100, 80, "none").cls, "neutral");
});

test("no change is neutral whichever way is better", () => {
  for (const better of ["lower", "higher", "none"] as const) {
    const d = fmtDelta(50, 50, better);
    assert.equal(d.cls, "neutral");
    assert.equal(d.text, "no change");
  }
});

test("a percent change against zero is undefined, not infinite", () => {
  // 0 → 5 is not "+∞%", and printing one would be a lie.
  const d = fmtDelta(0, 5, "higher");
  assert.equal(d.percent, "—");
  assert.equal(d.cls, "ok");
});

test("the absolute change is signed and formatted", () => {
  assert.equal(fmtDelta(10, 25, "higher", (n) => `${n.toFixed(1)} ms`).text, "+15.0 ms");
  assert.equal(fmtDelta(25, 10, "lower", (n) => `${n.toFixed(1)} ms`).text, "−15.0 ms");
});

test("a short run keeps the precision that is its whole result", () => {
  // A batch benchmark finishing in 73ms must not render as "0 s".
  assert.equal(fmtDuration(0.073), "73 ms");
  assert.equal(fmtDuration(1.304), "1.304 s");
  assert.equal(fmtDuration(38.27), "38.3 s");
});

test("a long run switches to minutes and hours", () => {
  assert.equal(fmtDuration(60), "1m 00s");
  assert.equal(fmtDuration(185), "3m 05s");
  assert.equal(fmtDuration(3600), "1h 00m 00s");
  assert.equal(fmtDuration(3725), "1h 02m 05s");
});

test("a nonsensical duration is not rendered as a number", () => {
  assert.equal(fmtDuration(NaN), "—");
  assert.equal(fmtDuration(-1), "—");
});

test("an export stamp is sortable and filename-safe", () => {
  // Built from local-time parts, so this is stable wherever it runs.
  const d = new Date(2026, 8, 1, 16, 45, 3);
  assert.equal(fileStamp(d.getTime()), "2026-09-01_164503");
  // Every component is zero-padded, or names would not sort.
  const early = new Date(2026, 0, 2, 3, 4, 5);
  assert.equal(fileStamp(early.getTime()), "2026-01-02_030405");
  // Nothing in it may need escaping in a filename.
  assert.match(fileStamp(d.getTime()), /^[\w-]+$/);
});

test("a nonsensical timestamp yields no stamp rather than junk", () => {
  assert.equal(fileStamp(NaN), "");
});
