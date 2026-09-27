/**
 * Pure formatting helpers.
 *
 * Deliberately free of JSX so they can be unit tested directly with the Node
 * test runner, which strips types but does not compile JSX.
 */

/** Formats milliseconds for display without pretending to false precision. */
export function fmtMs(ms: number): string {
  if (!Number.isFinite(ms)) return "—";
  if (ms >= 10000) return `${(ms / 1000).toFixed(1)} s`;
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)} s`;
  if (ms >= 100) return `${Math.round(ms)} ms`;
  return `${ms.toFixed(1)} ms`;
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(2)} MB`;
}

export function fmtNum(n: number): string {
  return n.toLocaleString(undefined, { maximumFractionDigits: 1 });
}

export function fmtPct(rate: number): string {
  return `${(rate * 100).toFixed(rate > 0 && rate < 0.001 ? 3 : 1)}%`;
}

const DAY_MS = 24 * 60 * 60 * 1000;

/**
 * Group a timestamp the way a log is read: "Today", "Yesterday", or a date.
 *
 * Shared by every list that shows things in the order they happened, so the
 * Runs and History sidebars group identically.
 */
export function dayLabel(at: number): string {
  const start = new Date();
  start.setHours(0, 0, 0, 0);
  const today = start.getTime();
  if (at >= today) return "Today";
  if (at >= today - DAY_MS) return "Yesterday";
  return new Date(at).toLocaleDateString();
}

/** The time of day alone, for rows already grouped under a day heading. */
export function clockTime(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/**
 * Group already-sorted-newest-first items into day buckets.
 *
 * Walking in order is enough because the input is sorted, so the groups come
 * out in the right sequence without a second pass.
 */
export function groupByDay<T>(items: T[], at: (item: T) => number): { day: string; items: T[] }[] {
  const out: { day: string; items: T[] }[] = [];
  for (const item of items) {
    const day = dayLabel(at(item));
    const last = out[out.length - 1];
    if (last && last.day === day) last.items.push(item);
    else out.push({ day, items: [item] });
  }
  return out;
}

/** Which direction of change counts as an improvement. */
export type Better = "lower" | "higher" | "none";

export interface Delta {
  /** Signed change, already formatted with its sign. */
  text: string;
  /** Percent change against the baseline, or "—" when there is none. */
  percent: string;
  /** "ok" when the change is an improvement, "err" when it is a regression. */
  cls: "ok" | "err" | "neutral";
}

/**
 * Describe a change from one run to another.
 *
 * A percent change against a baseline of zero is undefined, not infinite — it
 * renders as "—" rather than inventing a number.
 */
export function fmtDelta(
  baseline: number,
  candidate: number,
  better: Better,
  format: (n: number) => string = (n) => fmtNum(n),
): Delta {
  const diff = candidate - baseline;
  const sign = diff > 0 ? "+" : diff < 0 ? "−" : "";
  const text = diff === 0 ? "no change" : `${sign}${format(Math.abs(diff))}`;
  const percent =
    baseline === 0 ? "—" : `${sign}${((Math.abs(diff) / baseline) * 100).toFixed(1)}%`;

  let cls: "ok" | "err" | "neutral" = "neutral";
  if (diff !== 0 && better !== "none") {
    const improved = better === "lower" ? diff < 0 : diff > 0;
    cls = improved ? "ok" : "err";
  }
  return { text, percent, cls };
}

export function fmtTime(unixMs: number): string {
  return new Date(unixMs).toLocaleString();
}

/**
 * A run's wall time, at a precision that suits its length.
 *
 * A batch run can finish in 73 milliseconds and a soak can last an hour, and
 * one format cannot serve both: "0 s" throws away the whole result of the
 * first, while "4931.271 s" is unreadable for the second. Sub-minute runs keep
 * their fractions, longer ones switch to minutes and hours.
 */
export function fmtDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "—";
  if (seconds < 1) return `${(seconds * 1000).toFixed(0)} ms`;
  if (seconds < 10) return `${seconds.toFixed(3)} s`;
  if (seconds < 60) return `${seconds.toFixed(1)} s`;

  const whole = Math.floor(seconds);
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const sec = whole % 60;
  if (h > 0) return `${h}h ${String(m).padStart(2, "0")}m ${String(sec).padStart(2, "0")}s`;
  return `${m}m ${String(sec).padStart(2, "0")}s`;
}

/**
 * A sortable, filename-safe stamp for an exported file.
 *
 * Local time rather than UTC, because the name is read by the person who ran
 * the test, and `2026-09-01_164530` sorts chronologically in any file listing
 * where a locale-formatted date would not.
 *
 * Taken from the run's own start time rather than the moment of export, so a
 * run always exports under the same name: re-exporting overwrites the file it
 * replaces instead of littering the folder with near-duplicates.
 */
export function fileStamp(unixMs: number): string {
  const d = new Date(unixMs);
  if (Number.isNaN(d.getTime())) return "";
  const p2 = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}` +
    `_${p2(d.getHours())}${p2(d.getMinutes())}${p2(d.getSeconds())}`
  );
}
