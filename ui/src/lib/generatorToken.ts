/**
 * Building `{{$generator}}` tokens from the picker's fields.
 *
 * Pure and separate from the component so the assembly rules — argument order,
 * quoting, which transforms apply in which order — can be tested directly.
 * They are easy to get subtly wrong and the failure is silent: an unrecognised
 * token is sent through as literal text rather than raising anything.
 */
/** One argument of a generator, rendered as a labelled field. */
export interface ArgSpec {
  key: string;
  label: string;
  placeholder?: string;
  /** Numeric fields get a number input; "csv" is a free list like pick's. */
  kind: "number" | "text" | "csv";
  initial: string;
  hint?: string;
}

export interface GenSpec {
  name: string;
  label: string;
  desc: string;
  args: ArgSpec[];
  /** Repeat-style generators take another generator as their element. */
  takesInner?: boolean;
}

/** Generators offered by the picker, in the order they are listed. */
export const GENERATORS: GenSpec[] = [
  {
    name: "int",
    label: "Random integer",
    desc: "A whole number between two bounds, inclusive.",
    args: [
      { key: "min", label: "Min", kind: "number", initial: "1" },
      { key: "max", label: "Max", kind: "number", initial: "1000" },
    ],
  },
  {
    name: "float",
    label: "Random decimal",
    desc: "A decimal number, with a fixed number of places.",
    args: [
      { key: "min", label: "Min", kind: "number", initial: "0" },
      { key: "max", label: "Max", kind: "number", initial: "1" },
      { key: "decimals", label: "Decimals", kind: "number", initial: "4" },
    ],
  },
  {
    name: "string",
    label: "Random text",
    desc: "Random characters of a given length.",
    args: [
      { key: "length", label: "Length", kind: "number", initial: "12" },
      {
        key: "charset",
        label: "Characters",
        kind: "text",
        initial: "alnum",
        hint: "alnum, alpha, lower, digits, hex or base36",
      },
    ],
  },
  {
    name: "pick",
    label: "One of a list",
    desc: "Chooses one of the values you list.",
    args: [
      {
        key: "choices",
        label: "Values",
        kind: "csv",
        initial: "free,pro,enterprise",
        hint: "Separate with commas.",
      },
    ],
  },
  { name: "uuid", label: "UUID", desc: "A random UUID.", args: [] },
  { name: "bool", label: "true / false", desc: "One or the other.", args: [] },
  {
    name: "seq",
    label: "Position in a repeat",
    desc: "Counts 0, 1, 2 … inside a repeat. Guaranteed unique.",
    args: [],
  },
  {
    name: "now",
    label: "Timestamp",
    desc: "The current time, RFC 3339, with an optional offset.",
    args: [
      { key: "offset", label: "Offset (seconds)", kind: "number", initial: "0" },
    ],
  },
  { name: "epoch", label: "Unix seconds", desc: "The current time in seconds.", args: [] },
  { name: "epochMs", label: "Unix milliseconds", desc: "The current time in ms.", args: [] },
  {
    name: "repeat",
    label: "A list of values",
    desc: "The value below, repeated. Use for arrays and id lists.",
    takesInner: true,
    args: [
      { key: "count", label: "How many", kind: "number", initial: "100" },
      {
        key: "separator",
        label: "Separator",
        kind: "text",
        initial: ",",
        hint: 'Use ","  (quote-comma-quote) between quoted JSON strings.',
      },
    ],
  },
  {
    name: "repeatUnique",
    label: "A list of distinct values",
    desc: "Like the list above, but duplicates are redrawn.",
    takesInner: true,
    args: [
      { key: "count", label: "How many", kind: "number", initial: "100" },
      { key: "separator", label: "Separator", kind: "text", initial: "," },
    ],
  },
];

/** Generators that make sense as the element of a repeat. */
export const INNER_GENERATORS = GENERATORS.filter((g) => !g.takesInner);

export type ArgValues = Record<string, string>;

export function initialArgs(gen: GenSpec): ArgValues {
  const out: ArgValues = {};
  for (const a of gen.args) out[a.key] = a.initial;
  return out;
}

/** Quote an argument when it contains anything that would confuse the parser. */
export function quoteArg(raw: string): string {
  if (raw === "") return "''";
  if (/^[A-Za-z0-9_.\-+]+$/.test(raw)) return raw;
  // Single quotes are the norm; fall back to double when the value has one.
  return raw.includes("'") ? `"${raw}"` : `'${raw}'`;
}

export interface TransformValues {
  padWidth: string;
  format: string;
  casing: "" | "upper" | "lower";
  /**
   * "" none · "base64" the text · "b64f32"/"b64f64" the numbers packed as
   * little-endian IEEE-754 bytes and then encoded, which is what an
   * embeddings endpoint means by a base64 vector.
   */
  encode: "" | "base64" | "b64f32" | "b64f64" | "b64u128";
  /** Wrap the value in double quotes as a JSON string. */
  quote: boolean;
}

export const NO_TRANSFORMS: TransformValues = {
  padWidth: "",
  format: "",
  casing: "",
  encode: "",
  quote: false,
};

/**
 * Build the transform chain.
 *
 * The order is fixed — pad, format, case, base64, then quote — because that is
 * the only order that makes sense: padding a value after wrapping it in a
 * prefix would pad the prefix, encoding has to come after the text transforms
 * or it would encode the encoding, and quoting comes last of all because it
 * turns the value into a JSON string that nothing else should reach inside.
 */
export function transformChain(t: TransformValues): string {
  const parts: string[] = [];
  const width = Number(t.padWidth);
  if (t.padWidth.trim() !== "" && Number.isFinite(width) && width > 0) {
    parts.push(`pad(${Math.floor(width)})`);
  }
  if (t.format.trim() !== "" && t.format.includes("{}")) {
    parts.push(`fmt(${quoteArg(t.format)})`);
  }
  if (t.casing) parts.push(t.casing);
  if (t.encode) parts.push(t.encode);
  if (t.quote) parts.push("quote");
  return parts.map((p) => ` | ${p}`).join("");
}

/** Assemble one generator call (without the surrounding braces). */
export function buildSpec(gen: GenSpec, args: ArgValues, transforms: TransformValues, inner: string): string {
  const positional: string[] = [];

  if (gen.takesInner) {
    positional.push(args.count?.trim() || "1");
    positional.push(`$${inner}`);
    // Only emit a separator when it is not the default comma, so the common
    // case stays short and readable.
    const sep = args.separator ?? ",";
    if (sep !== ",") positional.push(quoteArg(sep));
  } else if (gen.name === "pick") {
    for (const choice of (args.choices ?? "").split(",")) {
      positional.push(quoteArg(choice.trim()));
    }
  } else {
    // Arguments are positional, so only trailing blanks can be omitted; a
    // blank ahead of a filled one takes its initial value to hold its slot.
    const values = gen.args.map((a) => (args[a.key] ?? "").trim());
    while (values.length && values[values.length - 1] === "") values.pop();
    values.forEach((v, i) => {
      const a = gen.args[i];
      const value = v === "" ? a.initial : v;
      positional.push(a.kind === "number" ? value : quoteArg(value));
    });
  }

  const call = positional.length ? `${gen.name}(${positional.join(", ")})` : gen.name;
  return `${call}${transformChain(transforms)}`;
}

