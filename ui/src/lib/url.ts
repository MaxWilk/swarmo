/**
 * Splitting a pasted URL into a base and its query parameters.
 *
 * Params are appended to the URL when a request is sent, so a query left in
 * the URL *and* listed in the params table would be sent twice. Extraction has
 * to take the query out of the URL, not just copy it.
 */
import type { KeyValue } from "../api";

export interface ExtractedUrl {
  /** The URL with its query removed. */
  url: string;
  /** The parameters found in the query, in the order they appeared. */
  params: { key: string; value: string }[];
  /** Anything after `#`, kept on the URL rather than treated as a parameter. */
  fragment: string;
}

/**
 * Decode one query component, tolerating input that is not valid encoding.
 *
 * The component is trimmed *before* decoding, which is the distinction that
 * matters: literal whitespace in the pasted text is an artifact of however the
 * link was copied — out of a chat message, a wrapped email, a log line — while
 * `%20` or `+` is a space somebody meant to send. Trimming after decoding
 * would throw the deliberate ones away too.
 */
function decode(part: string): string {
  // `+` means a space in a query string, which decodeURIComponent does not do.
  const plussed = part.trim().replace(/\+/g, " ");
  try {
    return decodeURIComponent(plussed);
  } catch {
    // A stray % that is not an escape would throw; the raw text is more use
    // than losing the value.
    return plussed;
  }
}

/**
 * Pull the query parameters out of a URL.
 *
 * Returns `null` when there is nothing to extract, so callers can leave the
 * user's text exactly as they typed it rather than rewriting it for no reason.
 */
export function extractQuery(raw: string): ExtractedUrl | null {
  const text = raw.trim();
  const q = text.indexOf("?");
  if (q === -1) return null;

  // A fragment belongs to the URL, and anything after it is not a query —
  // `/path?a=1#section?b=2` has one parameter, not two.
  const hashAt = text.indexOf("#");
  const hasFragmentBeforeQuery = hashAt !== -1 && hashAt < q;
  if (hasFragmentBeforeQuery) return null;

  const base = text.slice(0, q);
  const rest = text.slice(q + 1);
  const fragmentAt = rest.indexOf("#");
  const query = fragmentAt === -1 ? rest : rest.slice(0, fragmentAt);
  const fragment = fragmentAt === -1 ? "" : rest.slice(fragmentAt);

  if (query.trim() === "") return null;

  const params: { key: string; value: string }[] = [];
  for (const pair of query.split("&")) {
    if (pair === "") continue;
    const eq = pair.indexOf("=");
    const key = decode(eq === -1 ? pair : pair.slice(0, eq));
    // A bare `?flag` is a parameter with an empty value, not a missing one.
    const value = eq === -1 ? "" : decode(pair.slice(eq + 1));
    if (key === "") continue;
    params.push({ key, value });
  }

  if (params.length === 0) return null;
  return { url: base + fragment, params, fragment };
}

/**
 * Fold extracted parameters into the ones already listed.
 *
 * Existing entries are updated rather than duplicated, and nothing is removed:
 * pasting a URL should never quietly discard a parameter that was typed by
 * hand. A disabled row that reappears in a pasted URL is re-enabled, since
 * that URL plainly means to send it.
 */
export function mergeParams(
  existing: KeyValue[],
  found: { key: string; value: string }[],
): KeyValue[] {
  const out = existing.map((p) => ({ ...p }));
  // Each existing row absorbs at most one pasted pair, so a repeated key
  // (?id=1&id=2) keeps every value instead of the last overwriting the rest.
  const used = new Set<number>();
  for (const { key, value } of found) {
    const i = out.findIndex((p, j) => j < existing.length && !used.has(j) && p.key === key);
    const match = i >= 0 ? out[i] : undefined;
    if (match) {
      used.add(i);
      match.value = value;
      match.enabled = true;
    } else {
      out.push({ key, value, enabled: true });
    }
  }
  return out;
}
