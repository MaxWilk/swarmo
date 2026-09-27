import { useEffect, useMemo, useState } from "react";
import uPlot from "uplot";
import * as api from "../../api";
import type { RunSummary, Snapshot, StatusCount, TagStats } from "../../api";
import { describeStatus } from "../../api";
import {
  EmptyState,
  Icons,
  fmtBytes,
  fmtDelta,
  fmtMs,
  fmtNum,
  fmtPct,
  fmtTime,
  type Better,
} from "../../components/ui";
import { useLoad } from "../../stores/load";
import { reportError } from "../../stores/toast";
import { Chart, cssVar, useThemeVersion } from "./Chart";

/** One run, with everything the comparison needs to describe it. */
interface Side {
  runId: string;
  title: string;
  summary: RunSummary;
  timeline: Snapshot[];
}

async function loadSide(runId: string): Promise<Side> {
  const [summary, timeline, annotation] = await Promise.all([
    api.runGet(runId),
    api.runTimeline(runId),
    api.runAnnotationGet(runId),
  ]);
  return {
    runId,
    title: annotation.label || summary.scenarioName,
    summary,
    timeline,
  };
}

/** A headline number with how it moved against the baseline. */
function DeltaTile({
  label,
  value,
  baseline,
  candidate,
  better,
  format,
}: {
  label: string;
  value: string;
  baseline: number;
  candidate: number;
  better: Better;
  format?: (n: number) => string;
}) {
  const d = fmtDelta(baseline, candidate, better, format);
  return (
    <div className="stat-tile">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      <div className={`stat-sub delta ${d.cls}`}>
        {d.text}
        {d.percent !== "—" && ` (${d.percent})`}
      </div>
    </div>
  );
}

/** Candidate value with its change, for a table cell. */
function DeltaCell({
  baseline,
  candidate,
  better,
  format,
}: {
  baseline: number;
  candidate: number;
  better: Better;
  format: (n: number) => string;
}) {
  const d = fmtDelta(baseline, candidate, better, format);
  return (
    <td>
      {format(candidate)}{" "}
      <span className={`delta ${d.cls}`} title={`Baseline ${format(baseline)}`}>
        {d.text === "no change" ? "=" : d.text}
      </span>
    </td>
  );
}

export function CompareScreen() {
  const compare = useLoad((s) => s.compare);
  const closeCompare = useLoad((s) => s.closeCompare);
  const themeVersion = useThemeVersion();

  const [sides, setSides] = useState<{ baseline: Side; candidate: Side } | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    if (!compare) return;
    let cancelled = false;
    setLoading(true);
    void (async () => {
      try {
        const [baseline, candidate] = await Promise.all([
          loadSide(compare.baseline),
          loadSide(compare.candidate),
        ]);
        if (!cancelled) setSides({ baseline, candidate });
      } catch (e) {
        reportError("Could not load these runs for comparison", e);
        // The only Close button lives in the loaded branch, so a failed load
        // would otherwise leave a spinner with no way back to the list.
        if (!cancelled) closeCompare();
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [compare]);

  // Both runs are plotted against elapsed seconds, so runs that started hours
  // apart still line up at zero. uPlot needs one shared x, so the series are
  // sampled onto the union of both runs' elapsed values.
  const charts = useMemo(() => {
    if (!sides) return null;
    const { baseline, candidate } = sides;
    const xs = Array.from(
      new Set([
        ...baseline.timeline.map((s) => s.elapsedSec),
        ...candidate.timeline.map((s) => s.elapsedSec),
      ]),
    ).sort((a, b) => a - b);

    const pick = (t: Snapshot[], f: (s: Snapshot) => number) => {
      const byX = new Map(t.map((s) => [s.elapsedSec, f(s)]));
      return xs.map((x) => byX.get(x) ?? null);
    };

    return {
      rps: [xs, pick(baseline.timeline, (s) => s.rps), pick(candidate.timeline, (s) => s.rps)],
      p95: [xs, pick(baseline.timeline, (s) => s.p95), pick(candidate.timeline, (s) => s.p95)],
      vus: [
        xs,
        pick(baseline.timeline, (s) => s.activeVus),
        pick(candidate.timeline, (s) => s.activeVus),
      ],
    } as Record<string, uPlot.AlignedData>;
  }, [sides]);

  const seriesDef = useMemo<uPlot.Series[]>(
    () => [
      {},
      {
        label: "Baseline",
        stroke: cssVar("--text-faint"),
        width: 2,
        dash: [4, 4],
      },
      { label: "Candidate", stroke: cssVar("--accent"), width: 2 },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [themeVersion],
  );

  // An outer join: a step that exists in only one run must still be listed,
  // or a comparison would quietly hide the step that was added or removed.
  const steps = useMemo(() => {
    if (!sides) return [];
    const byTag = (rows: TagStats[]) => new Map(rows.map((r) => [r.tag, r]));
    const a = byTag(sides.baseline.summary.perTag);
    const b = byTag(sides.candidate.summary.perTag);
    const tags = Array.from(new Set([...a.keys(), ...b.keys()])).sort();
    return tags.map((tag) => ({ tag, baseline: a.get(tag), candidate: b.get(tag) }));
  }, [sides]);

  const statuses = useMemo(() => {
    if (!sides) return [];
    const key = (sc: StatusCount) => `${sc.protocol}-${sc.code}`;
    const a = new Map(sides.baseline.summary.statusCodes.map((sc) => [key(sc), sc]));
    const b = new Map(sides.candidate.summary.statusCodes.map((sc) => [key(sc), sc]));
    return Array.from(new Set([...a.keys(), ...b.keys()]))
      .map((k) => {
        const sc = (a.get(k) ?? b.get(k)) as StatusCount;
        return { sc, baseline: a.get(k)?.count ?? 0, candidate: b.get(k)?.count ?? 0 };
      })
      .sort((x, y) => y.candidate + y.baseline - (x.candidate + x.baseline));
  }, [sides]);

  if (!compare) return null;
  if (loading || !sides || !charts) {
    return (
      <div className="empty">
        <div className="spinner" />
      </div>
    );
  }

  const { baseline, candidate } = sides;
  const b = baseline.summary;
  const c = candidate.summary;
  const differentScenarios = b.scenarioRef !== c.scenarioRef;

  return (
    <div className="scroll">
      <div className="panel-header">
        <span className="grow">
          {baseline.title} <span className="muted">vs</span> {candidate.title}
        </span>
        <button className="btn sm ghost" onClick={closeCompare}>
          <Icons.Close /> Close
        </button>
      </div>

      <div className="col pad" style={{ gap: 6 }}>
        <div className="hint">
          Baseline <strong>{baseline.title}</strong> ({fmtTime(b.startedAt)}) → candidate{" "}
          <strong>{candidate.title}</strong> ({fmtTime(c.startedAt)}). Every change below is
          the candidate measured against the baseline.
        </div>
        {differentScenarios && (
          <div className="status-pill warn" style={{ alignSelf: "flex-start" }}>
            <Icons.Alert /> These runs come from different scenarios — compare with care.
          </div>
        )}
      </div>

      <div className="stat-row">
        <DeltaTile
          label="Requests"
          value={fmtNum(c.totalRequests)}
          baseline={b.totalRequests}
          candidate={c.totalRequests}
          better="none"
        />
        <DeltaTile
          label="Requests / sec"
          value={fmtNum(c.rps)}
          baseline={b.rps}
          candidate={c.rps}
          better="higher"
        />
        <DeltaTile
          label="Error rate"
          value={fmtPct(c.errorRate)}
          baseline={b.errorRate}
          candidate={c.errorRate}
          better="lower"
          format={(n) => fmtPct(n)}
        />
        <DeltaTile
          label="p50"
          value={fmtMs(c.overall.p50)}
          baseline={b.overall.p50}
          candidate={c.overall.p50}
          better="lower"
          format={fmtMs}
        />
        <DeltaTile
          label="p95"
          value={fmtMs(c.overall.p95)}
          baseline={b.overall.p95}
          candidate={c.overall.p95}
          better="lower"
          format={fmtMs}
        />
        <DeltaTile
          label="p99"
          value={fmtMs(c.overall.p99)}
          baseline={b.overall.p99}
          candidate={c.overall.p99}
          better="lower"
          format={fmtMs}
        />
        <DeltaTile
          label="Data received"
          value={fmtBytes(c.bytesIn)}
          baseline={b.bytesIn}
          candidate={c.bytesIn}
          better="none"
          format={fmtBytes}
        />
      </div>

      <div className="col pad" style={{ gap: 16 }}>
        <Chart
          title="Requests/sec"
          data={charts.rps}
          series={seriesDef}
          height={180}
          yLabels={{ y: "Requests / sec" }}
        />
        <Chart
          title="p95 latency"
          data={charts.p95}
          series={seriesDef}
          height={180}
          yLabels={{ y: "Latency (ms)" }}
        />
        <Chart
          title="Active virtual users"
          data={charts.vus}
          series={seriesDef}
          height={180}
          yLabels={{ y: "Virtual users" }}
        />
      </div>

      <div className="pad">
        <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>
          By step
        </div>
        <div className="scroll" style={{ maxHeight: 320 }}>
          <table className="data-table">
            <thead>
              <tr>
                <th>Step</th>
                <th>Count</th>
                <th>Errors</th>
                <th>p50</th>
                <th>p95</th>
                <th>p99</th>
              </tr>
            </thead>
            <tbody>
              {steps.map(({ tag, baseline: bs, candidate: cs }) => {
                if (!bs || !cs) {
                  return (
                    <tr key={tag}>
                      <td>{tag}</td>
                      <td colSpan={5}>
                        <span className="status-pill warn">
                          only in {bs ? "baseline" : "candidate"}
                        </span>
                      </td>
                    </tr>
                  );
                }
                return (
                  <tr key={tag}>
                    <td>{tag}</td>
                    <DeltaCell
                      baseline={bs.count}
                      candidate={cs.count}
                      better="none"
                      format={fmtNum}
                    />
                    <DeltaCell
                      baseline={bs.errors}
                      candidate={cs.errors}
                      better="lower"
                      format={fmtNum}
                    />
                    <DeltaCell
                      baseline={bs.p50}
                      candidate={cs.p50}
                      better="lower"
                      format={fmtMs}
                    />
                    <DeltaCell
                      baseline={bs.p95}
                      candidate={cs.p95}
                      better="lower"
                      format={fmtMs}
                    />
                    <DeltaCell
                      baseline={bs.p99}
                      candidate={cs.p99}
                      better="lower"
                      format={fmtMs}
                    />
                  </tr>
                );
              })}
              {steps.length === 0 && (
                <tr>
                  <td colSpan={6} className="empty">
                    Neither run recorded any steps.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      {statuses.length > 0 && (
        <div className="pad">
          <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>
            Status codes
          </div>
          <table className="data-table">
            <thead>
              <tr>
                <th>Status</th>
                <th>Baseline</th>
                <th>Candidate</th>
              </tr>
            </thead>
            <tbody>
              {statuses.map(({ sc, baseline: bc, candidate: cc }) => {
                const { label, cls } = describeStatus(sc);
                return (
                  <tr key={`${sc.protocol}-${sc.code}`}>
                    <td>
                      <span className={`status-pill ${cls}`}>{label}</span>
                    </td>
                    <td>{fmtNum(bc)}</td>
                    <td>{fmtNum(cc)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {(b.thresholds.length > 0 || c.thresholds.length > 0) && (
        <div className="pad">
          <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>
            Thresholds
          </div>
          <table className="data-table">
            <thead>
              <tr>
                <th>Threshold</th>
                <th>Baseline</th>
                <th>Candidate</th>
              </tr>
            </thead>
            <tbody>
              {Array.from(
                new Set([
                  ...b.thresholds.map((t) => t.description),
                  ...c.thresholds.map((t) => t.description),
                ]),
              ).map((description) => {
                const bt = b.thresholds.find((t) => t.description === description);
                const ct = c.thresholds.find((t) => t.description === description);
                const pill = (t?: { passed: boolean }) =>
                  t ? (
                    <span className={`status-pill ${t.passed ? "ok" : "err"}`}>
                      {t.passed ? "Passed" : "Failed"}
                    </span>
                  ) : (
                    <span className="hint">not set</span>
                  );
                return (
                  <tr key={description}>
                    <td>{description}</td>
                    <td>{pill(bt)}</td>
                    <td>{pill(ct)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/** Shown when a comparison is asked for but only one run is picked. */
export function ComparePrompt() {
  return (
    <EmptyState title="Pick two runs">
      Choose a second run from the list to compare it against the first.
    </EmptyState>
  );
}
