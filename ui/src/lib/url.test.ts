import assert from "node:assert/strict";
import { test } from "node:test";
import { extractQuery, mergeParams } from "./url.ts";

test("a pasted URL gives up its query", () => {
  const out = extractQuery("https://api.test/v1/items?page=2&limit=50");
  assert.deepEqual(out?.params, [
    { key: "page", value: "2" },
    { key: "limit", value: "50" },
  ]);
  // The query must leave the URL, or sending would append the params again
  // and every one would go out twice.
  assert.equal(out?.url, "https://api.test/v1/items");
});

test("a URL with no query is left exactly as typed", () => {
  assert.equal(extractQuery("https://api.test/v1/items"), null);
  assert.equal(extractQuery("https://api.test/v1/items?"), null);
  assert.equal(extractQuery(""), null);
  // Nothing to extract, so nothing should be rewritten.
  assert.equal(extractQuery("not a url at all"), null);
});

test("encoded values are decoded", () => {
  const out = extractQuery("https://a.test/s?q=hello%20world&tag=a%2Bb");
  assert.equal(out?.params[0].value, "hello world");
  assert.equal(out?.params[1].value, "a+b");
});

test("a plus means a space, as it does in a query", () => {
  const out = extractQuery("https://a.test/s?q=hello+world");
  assert.equal(out?.params[0].value, "hello world");
});

test("malformed encoding keeps the raw text rather than throwing", () => {
  // A stray % would make decodeURIComponent throw; losing the value would be
  // worse than showing it as written.
  const out = extractQuery("https://a.test/s?q=100%&r=ok");
  assert.equal(out?.params[0].value, "100%");
  assert.equal(out?.params[1].value, "ok");
});

test("whitespace from however the link was copied is trimmed off", () => {
  // A link out of a chat message or a wrapped email picks up spaces and
  // newlines around the pairs; none of it belongs in the request.
  const out = extractQuery("https://a.test/s? page=2 & limit=50 ");
  assert.deepEqual(out?.params, [
    { key: "page", value: "2" },
    { key: "limit", value: "50" },
  ]);

  const wrapped = extractQuery("https://a.test/s?page=2&\n  limit=50");
  assert.deepEqual(wrapped?.params, [
    { key: "page", value: "2" },
    { key: "limit", value: "50" },
  ]);
});

test("a space that was actually encoded is kept", () => {
  // The distinction that makes trimming safe: %20 and + are spaces somebody
  // meant to send, so they survive where stray text whitespace does not.
  assert.equal(extractQuery("https://a.test/s?q=hello%20")?.params[0].value, "hello ");
  assert.equal(extractQuery("https://a.test/s?q=%20padded%20")?.params[0].value, " padded ");
  assert.equal(extractQuery("https://a.test/s?q=hello+")?.params[0].value, "hello ");
  // And a space in the middle is untouched either way.
  assert.equal(extractQuery("https://a.test/s?q=hello%20world")?.params[0].value, "hello world");
});

test("a parameter that is only whitespace is dropped, not added blank", () => {
  // `?a=1& &b=2` should give two parameters, not three.
  const out = extractQuery("https://a.test/s?a=1&  &b=2");
  assert.deepEqual(out?.params, [
    { key: "a", value: "1" },
    { key: "b", value: "2" },
  ]);
});

test("a valueless parameter is kept with an empty value", () => {
  const out = extractQuery("https://a.test/s?debug&q=1");
  assert.deepEqual(out?.params, [
    { key: "debug", value: "" },
    { key: "q", value: "1" },
  ]);
});

test("a value containing an equals sign survives", () => {
  // Only the first `=` separates; base64 and JWTs routinely contain more.
  const out = extractQuery("https://a.test/s?token=abc=def==");
  assert.deepEqual(out?.params, [{ key: "token", value: "abc=def==" }]);
});

test("a fragment stays on the URL and does not become a parameter", () => {
  const out = extractQuery("https://a.test/page?a=1#section");
  assert.equal(out?.url, "https://a.test/page#section");
  assert.deepEqual(out?.params, [{ key: "a", value: "1" }]);

  // A `?` inside a fragment is part of the fragment, not a query.
  assert.equal(extractQuery("https://a.test/page#frag?a=1"), null);
});

test("repeated keys are all kept", () => {
  // `?id=1&id=2` is meaningful to plenty of APIs; collapsing it would change
  // the request.
  const out = extractQuery("https://a.test/s?id=1&id=2");
  assert.equal(out?.params.length, 2);
});

test("merging updates a parameter rather than duplicating it", () => {
  const existing = [{ key: "page", value: "1", enabled: true }];
  const merged = mergeParams(existing, [{ key: "page", value: "9" }]);
  assert.equal(merged.length, 1);
  assert.equal(merged[0].value, "9");
});

test("merging never discards a parameter that was typed by hand", () => {
  const existing = [{ key: "apiKey", value: "secret", enabled: true }];
  const merged = mergeParams(existing, [{ key: "page", value: "2" }]);
  assert.equal(merged.length, 2);
  assert.equal(merged[0].key, "apiKey", "the hand-typed parameter was dropped");
});

test("a disabled parameter named in a pasted URL is switched back on", () => {
  const existing = [{ key: "page", value: "1", enabled: false }];
  const merged = mergeParams(existing, [{ key: "page", value: "3" }]);
  assert.equal(merged[0].enabled, true);
  assert.equal(merged[0].value, "3");
});

test("merging does not mutate what it was given", () => {
  const existing = [{ key: "page", value: "1", enabled: true }];
  mergeParams(existing, [{ key: "page", value: "9" }]);
  assert.equal(existing[0].value, "1", "the caller's array was modified");
});

test("mergeParams keeps every value of a repeated key", () => {
  const merged = mergeParams([], extractQuery("https://a.test/s?id=1&id=2")!.params);
  assert.deepEqual(
    merged.map((p) => p.value),
    ["1", "2"],
  );
});
