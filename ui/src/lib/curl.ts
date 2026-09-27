/**
 * Render a request as a cURL command.
 *
 * The output is meant to be pasted into a terminal or handed to someone else
 * verbatim, so it uses POSIX single-quoting throughout: inside single quotes a
 * shell interprets nothing, which is the only way to be safe with arbitrary
 * header and body content.
 */

/** Wrap a value in single quotes, escaping any single quotes inside it. */
export function shellQuote(value: string): string {
  // A single quote cannot appear inside single quotes, so close, emit an
  // escaped quote, and reopen: foo'bar becomes 'foo'\''bar'.
  return `'${value.replace(/'/g, "'\\''")}'`;
}

export interface CurlOptions {
  /** Note that the body kept in history is only its first part. */
  bodyTruncated?: boolean;
  /** Emit --insecure, for a request sent with TLS verification off. */
  insecure?: boolean;
}

export function toCurl(
  method: string,
  url: string,
  headers: [string, string][],
  body?: string | null,
  options: CurlOptions = {},
): string {
  const verb = method.toUpperCase();
  // GET is cURL's default, so naming it adds nothing. Everything else is
  // stated outright rather than left to be inferred from the body.
  const head: string[] = ["curl"];
  const hasBody = body != null && body !== "";
  // HEAD needs --head: with -X HEAD, curl waits for a body that never comes.
  // A GET with a body must say so, or --data-raw turns it into a POST.
  if (verb === "HEAD") head.push("--head");
  else if (verb !== "GET" || hasBody) head.push(`-X ${verb}`);
  if (options.insecure) head.push("--insecure");
  head.push(shellQuote(url));

  // Headers and body each get their own continued line; a command with
  // nothing to continue onto stays on one line.
  const rest: string[] = [];
  for (const [key, value] of headers) {
    rest.push(`-H ${shellQuote(`${key}: ${value}`)}`);
  }
  if (hasBody) {
    rest.push(`--data-raw ${shellQuote(body)}`);
  }

  const command = [head.join(" "), ...rest].join(" \\\n  ");
  if (options.bodyTruncated) {
    // Replaying a half body silently would be worse than not replaying it.
    return `${command}\n# Note: this body is only the first part of what was sent; Swarmo truncates long bodies in history.`;
  }
  return command;
}
