#!/usr/bin/env node
/**
 * Turn a TensorFlow Serving signature into a Swarmo Predict message.
 *
 * A model's signature is the only authority on what it wants; hand-writing the
 * request means transcribing dozens of field names and their dtypes, and a
 * model that gains a feature silently invalidates the whole message. This
 * reads the signature and writes the request, so the two cannot drift.
 *
 * Usage:
 *   node tools/tf-signature-to-swarmo.mjs metadata.json [--model NAME]
 *                                         [--signature NAME] [--reuse old-message.json]
 *
 * `metadata.json` is whatever GetModelMetadata returned — either the REST
 * response from /v1/models/<name>/metadata, or the JSON Swarmo shows for a
 * gRPC GetModelMetadata call. Both carry the same signature_def.
 *
 * `--reuse` carries the value lists out of a message you already had working.
 * The signature says which fields exist and what type they are, but not what
 * a categorical field's valid values are — that knowledge only lives in the
 * message you wrote, and retyping it every time the model changes is how it
 * gets lost.
 */
import { readFileSync } from "node:fs";

const args = process.argv.slice(2);
const file = args.find((a) => !a.startsWith("--"));
const flag = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};

if (!file) {
  console.error(
    "Usage: node tools/tf-signature-to-swarmo.mjs <metadata.json> [--model NAME] [--signature NAME] [--reuse old-message.json]",
  );
  process.exit(2);
}

const raw = JSON.parse(readFileSync(file, "utf8"));

/**
 * Pull each field's existing generator out of a previous message.
 *
 * Read line by line rather than with JSON.parse because the message is a
 * Swarmo template: `"floatVal":[{{$float(0,1,2)}}]` is deliberately not valid
 * JSON. One field per line is how these are written and how they stay
 * readable, so a line is a safe unit to match against.
 */
function reusableGenerators(path) {
  const found = new Map();
  const namePattern = /^\s*"([A-Za-z0-9_]+)"\s*:/;
  const valuePattern =
    /"(stringVal|int64Val|intVal|floatVal|doubleVal|boolVal)"\s*:\s*\[(.*)\]\s*\}/;

  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    const name = line.match(namePattern);
    const value = line.match(valuePattern);
    if (name && value && value[2].includes("{{")) {
      found.set(name[1], { field: value[1], value: value[2].trim() });
    }
  }
  return found;
}

const reuseFile = flag("reuse", null);
const reused = reuseFile ? reusableGenerators(reuseFile) : new Map();

/**
 * Find the map of signatures, wherever this response shape put it.
 *
 * Keying off the name "signature_def" is not enough: TF Serving's REST
 * response nests it twice (`metadata.signature_def.signature_def`), so the
 * first match is a wrapper, not the map. What identifies the real one is its
 * shape — a map whose values are signatures, and a signature has `inputs`.
 */
function findSignatures(root) {
  const seen = new Set();
  const queue = [root];
  while (queue.length) {
    const node = queue.shift();
    if (!node || typeof node !== "object" || seen.has(node)) continue;
    seen.add(node);

    const values = Object.values(node);
    const looksLikeSignatureMap =
      values.length > 0 &&
      values.every((v) => v && typeof v === "object") &&
      values.some((v) => "inputs" in v || "Inputs" in v);
    if (looksLikeSignatureMap) return node;

    queue.push(...values);
  }
  return null;
}

const signatures = findSignatures(raw);
if (!signatures) {
  console.error("No signature_def found in that file. Is it the GetModelMetadata response?");
  process.exit(1);
}

const wanted = flag("signature", "serving_default");
const signature = signatures[wanted] ?? signatures[Object.keys(signatures)[0]];
const inputs = signature?.inputs ?? signature?.Inputs;
if (!inputs) {
  console.error(
    `No usable signature named "${wanted}". Found: ${Object.keys(signatures).join(", ")}`,
  );
  process.exit(1);
}

/**
 * A plausible random value for one input.
 *
 * The point is a message that is accepted, with every field varying per
 * request; the ranges are placeholders to be tuned, not claims about the data.
 * Names are only a hint — a field ending "_cos" is bounded at plus or minus
 * one whatever else is true of it.
 */
function generatorFor(name, dtype) {
  const n = name.toLowerCase();

  if (dtype === "DT_STRING") {
    // No way to know the vocabulary from the signature, so emit one obvious
    // placeholder rather than inventing categories the model has never seen.
    return { field: "stringVal", value: "\"{{$pick('CHANGE_ME') | base64}}\"" };
  }
  if (dtype === "DT_INT64") {
    // int64 is a string in proto3 JSON; int32 is a number.
    return { field: "int64Val", value: '"{{$int(0,31)}}"' };
  }
  if (dtype === "DT_INT32") {
    return { field: "intVal", value: "{{$int(0,31)}}" };
  }
  if (dtype === "DT_BOOL") {
    return { field: "boolVal", value: "{{$pick('true','false')}}" };
  }

  const field = dtype === "DT_DOUBLE" ? "doubleVal" : "floatVal";
  // Cyclical encodings are always in [-1, 1]; counters never go negative;
  // anything comparative swings both ways.
  if (n.endsWith("_sin") || n.endsWith("_cos")) {
    return { field, value: "{{$float(-1,1,4)}}" };
  }
  if (n.includes("streak") || n.includes("count")) {
    return { field, value: "{{$float(0,10,0)}}" };
  }
  if (n.includes("delta") || n.includes("_vs_") || n.includes("t_value")) {
    return { field, value: "{{$float(-2,2,4)}}" };
  }
  return { field, value: "{{$float(0,10,4)}}" };
}

const names = Object.keys(inputs).sort();
const lines = names.map((name) => {
  const spec = inputs[name];
  const dtype = spec.dtype ?? spec.dType ?? "DT_FLOAT";
  const dims = (spec.tensor_shape ?? spec.tensorShape)?.dim ?? [{ size: "1" }, { size: "1" }];
  const shape = dims.map((d) => `{"size":"${d.size ?? 1}"}`).join(",");

  // A value that already worked beats a generated placeholder, but only if it
  // is still the same kind of field — a float that became a string must not
  // keep its old numeric generator.
  const carried = reused.get(name);
  const fresh = generatorFor(name, dtype);
  const { field, value } = carried && carried.field === fresh.field ? carried : fresh;

  return `    ${JSON.stringify(name)}: {"dtype":"${dtype}","tensorShape":{"dim":[${shape}]},"${field}":[${value}]}`;
});

const modelName = flag("model", raw?.model_spec?.name ?? raw?.modelSpec?.name ?? "CHANGE_ME");

console.log(`{
  "modelSpec": { "name": ${JSON.stringify(modelName)}, "signatureName": ${JSON.stringify(wanted)} },
  "inputs": {
${lines.join(",\n")}
  }
}`);

// Everything below goes to stderr, so the message itself can be piped to a file.
console.error(`\n${names.length} inputs written.`);

if (reuseFile) {
  const carried = names.filter((n) => reused.has(n));
  console.error(`${carried.length} kept the values they already had.`);
  const dropped = [...reused.keys()].filter((n) => !(n in inputs));
  if (dropped.length) {
    console.error(`\nNo longer in the signature, so left out:\n  ${dropped.join(", ")}`);
  }
}

// Only string fields genuinely need a human: every other type has a usable
// range, but a category the model has never seen is just a miss.
const needsValues = names.filter((n) => (inputs[n].dtype ?? "") === "DT_STRING" && !reused.has(n));
if (needsValues.length) {
  console.error(
    `\n${needsValues.length} string fields still say CHANGE_ME — a signature does not carry a category's valid values:\n  ${needsValues.join(", ")}`,
  );
}
