import assert from "node:assert/strict";
import { test } from "node:test";
import { shellQuote, toCurl } from "./curl.ts";

test("a value with a quote in it stays one argument", () => {
  // The classic shell-injection shape: inside single quotes nothing is
  // interpreted, so the quote has to be closed and reopened around it.
  assert.equal(shellQuote("it's"), "'it'\\''s'");
  assert.equal(shellQuote("plain"), "'plain'");
});

test("nothing in a body can escape its quoting", () => {
  const nasty = `{"x":"'; rm -rf /; echo '"}`;
  const out = toCurl("POST", "https://a.test/", [], nasty);
  // Every single quote in the payload is escaped, so the command has no
  // unbalanced quote that could end the argument early.
  const quotes = out.split("").filter((c) => c === "'").length;
  assert.equal(quotes % 2, 0, out);
  assert.ok(out.includes("'\\''"), out);
});

test("GET does not spell out the default verb", () => {
  const out = toCurl("GET", "https://a.test/x", []);
  assert.ok(!out.includes("-X"), out);
  assert.ok(out.startsWith("curl 'https://a.test/x'"), out);
});

test("any other verb is stated outright", () => {
  assert.ok(toCurl("DELETE", "https://a.test/x", []).includes("-X DELETE"));
  assert.ok(toCurl("post", "https://a.test/x", [], "{}").includes("-X POST"));
});

test("headers each get their own -H", () => {
  const out = toCurl("GET", "https://a.test/", [
    ["accept", "application/json"],
    ["x-trace", "abc"],
  ]);
  assert.ok(out.includes("-H 'accept: application/json'"), out);
  assert.ok(out.includes("-H 'x-trace: abc'"), out);
});

test("an empty body is omitted rather than sent as an empty string", () => {
  assert.ok(!toCurl("POST", "https://a.test/", [], "").includes("--data-raw"));
  assert.ok(!toCurl("POST", "https://a.test/", [], null).includes("--data-raw"));
  assert.ok(toCurl("POST", "https://a.test/", [], "x").includes("--data-raw 'x'"));
});

test("a truncated body says so, so nobody replays half a request", () => {
  const out = toCurl("POST", "https://a.test/", [], "half", { bodyTruncated: true });
  assert.ok(out.includes("# Note:"), out);
  assert.ok(out.includes("truncates long bodies"), out);
});

test("TLS verification being off is carried across", () => {
  assert.ok(toCurl("GET", "https://a.test/", [], null, { insecure: true }).includes("--insecure"));
  assert.ok(!toCurl("GET", "https://a.test/", []).includes("--insecure"));
});

test("a GET with a body keeps its method", () => {
  // --data-raw alone would turn it into a POST.
  const cmd = toCurl("GET", "https://a.test/_search", [], '{"q":1}');
  assert.match(cmd, /^curl -X GET /);
});

test("HEAD uses --head rather than -X HEAD", () => {
  assert.equal(toCurl("HEAD", "https://a.test/", []), "curl --head 'https://a.test/'");
});
