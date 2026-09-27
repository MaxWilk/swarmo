import assert from "node:assert/strict";
import { test } from "node:test";
import {
  GENERATORS,
  INNER_GENERATORS,
  NO_TRANSFORMS,
  buildSpec,
  initialArgs,
  type GenSpec,
  type TransformValues,
} from "./generatorToken.ts";

const gen = (name: string): GenSpec => {
  const g = GENERATORS.find((x) => x.name === name);
  assert.ok(g, `no generator named ${name}`);
  return g;
};

/** The token as the picker would insert it. */
const token = (
  name: string,
  args: Record<string, string> = {},
  transforms: Partial<TransformValues> = {},
  inner = "",
) => {
  const g = gen(name);
  const spec = buildSpec(
    g,
    { ...initialArgs(g), ...args },
    { ...NO_TRANSFORMS, ...transforms },
    inner,
  );
  return `{{$${spec}}}`;
};

test("a generator with no arguments needs no parentheses", () => {
  assert.equal(token("uuid"), "{{$uuid}}");
  assert.equal(token("seq"), "{{$seq}}");
});

test("numeric arguments are passed through unquoted", () => {
  assert.equal(token("int", { min: "1", max: "50" }), "{{$int(1, 50)}}");
});

test("pick quotes each choice and drops the spacing around commas", () => {
  assert.equal(
    token("pick", { choices: "free, pro, enterprise" }),
    "{{$pick(free, pro, enterprise)}}",
  );
  // A choice containing a comma or space has to be quoted or it would split.
  assert.equal(token("pick", { choices: "a b,c" }), "{{$pick('a b', c)}}");
});

test("transforms are chained in a fixed, sensible order", () => {
  // pad before fmt: padding after wrapping would pad the prefix instead.
  assert.equal(
    token("seq", {}, { padWidth: "6", format: "creative_{}" }),
    "{{$seq | pad(6) | fmt('creative_{}')}}",
  );
  // base64 last, or the rest would encode the encoding.
  assert.equal(
    token("uuid", {}, { casing: "upper", encode: "base64" }),
    "{{$uuid | upper | base64}}",
  );
});

test("quote is appended last, after every other transform", () => {
  assert.equal(token("uuid", {}, { quote: true }), "{{$uuid | quote}}");
  // Quoting turns the value into a JSON string, so nothing may follow it.
  assert.equal(
    token("seq", {}, { padWidth: "6", format: "creative_{}", quote: true }),
    "{{$seq | pad(6) | fmt('creative_{}') | quote}}",
  );
  assert.equal(
    token("uuid", {}, { casing: "upper", encode: "base64", quote: true }),
    "{{$uuid | upper | base64 | quote}}",
  );
  // The vector encodings sit in the same slot as plain base64.
  assert.equal(
    token("float", { min: "0", max: "1", decimals: "4" }, { encode: "b64f32" }),
    "{{$float(0, 1, 4) | b64f32}}",
  );
});

test("the JSON-array shape the picker exists to make easy", () => {
  const seq = INNER_GENERATORS.find((g) => g.name === "seq")!;
  const inner = buildSpec(
    seq,
    initialArgs(seq),
    { ...NO_TRANSFORMS, padWidth: "6", format: "creative_{}", quote: true },
    "",
  );
  assert.equal(
    token("repeatUnique", { count: "1000" }, {}, inner),
    "{{$repeatUnique(1000, $seq | pad(6) | fmt('creative_{}') | quote)}}",
  );
});

test("a blank or malformed transform is left out entirely", () => {
  assert.equal(token("uuid", {}, { padWidth: "" }), "{{$uuid}}");
  assert.equal(token("uuid", {}, { padWidth: "0" }), "{{$uuid}}");
  // A format with no {} placeholder would be rejected by the backend, so it
  // is never emitted in the first place.
  assert.equal(token("uuid", {}, { format: "creative" }), "{{$uuid}}");
});

test("repeat nests the element generator and omits a default separator", () => {
  const seq = INNER_GENERATORS.find((g) => g.name === "seq")!;
  const inner = buildSpec(seq, initialArgs(seq), { ...NO_TRANSFORMS, padWidth: "6", format: "creative_{}" }, "");
  assert.equal(inner, "seq | pad(6) | fmt('creative_{}')");

  // The default comma stays implicit, keeping the common case readable.
  assert.equal(
    token("repeat", { count: "1000" }, {}, inner),
    "{{$repeat(1000, $seq | pad(6) | fmt('creative_{}'))}}",
  );
});

test("a non-default separator is quoted and emitted", () => {
  // The exact shape for a JSON array of quoted strings.
  assert.equal(
    token("repeat", { count: "3", separator: '","' }, {}, "uuid"),
    `{{$repeat(3, $uuid, '","')}}`,
  );
  // A separator that already contains a single quote falls back to double.
  assert.equal(
    token("repeat", { count: "2", separator: "' '" }, {}, "uuid"),
    `{{$repeat(2, $uuid, "' '")}}`,
  );
});

test("repeatUnique builds the same shape as repeat", () => {
  assert.equal(
    token("repeatUnique", { count: "500" }, {}, "int(0, 9999)"),
    "{{$repeatUnique(500, $int(0, 9999))}}",
  );
});

test("every listed generator produces a token that at least parses as one", () => {
  for (const g of GENERATORS) {
    const t = token(g.name, {}, {}, "uuid");
    assert.match(t, /^\{\{\$[A-Za-z]/, `${g.name} produced ${t}`);
    assert.ok(t.endsWith("}}"), `${g.name} produced ${t}`);
    // Balanced parentheses, or the backend would reject the whole token.
    const opens = (t.match(/\(/g) ?? []).length;
    const closes = (t.match(/\)/g) ?? []).length;
    assert.equal(opens, closes, `${g.name} produced unbalanced ${t}`);
  }
});

test("a blank argument ahead of a filled one keeps its slot", () => {
  // Arguments are positional: dropping min would read max as min.
  assert.equal(token("float", { min: "", max: "5", decimals: "2" }), "{{$float(0, 5, 2)}}");
  // Trailing blanks can still be left off.
  assert.equal(token("float", { min: "1", max: "", decimals: "" }), "{{$float(1)}}");
});
