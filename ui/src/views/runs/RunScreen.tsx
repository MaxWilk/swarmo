import { useEffect, useMemo, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import uPlot from "uplot";
import * as api from "../../api";
import type {
  RoundStats,
  RunAnnotation,
  RunState,
  RunSummary,
  TagStats,
  ThresholdResult,
} from "../../api";
import { describeStatus } from "../../api";
import {
  Icons,
  Menu,
  fmtBytes,
  fmtDuration,
  fmtMs,
  fmtNum,
  fmtPct,
  fmtTime,
  fileStamp,
} from "../../components/ui";
import { useLoad } from "../../stores/load";
import { useRunLauncher } from "../load/runLoadTest";
import { notify, reportError } from "../../stores/toast";
import { Chart, cssVar, useThemeVersion } from "./Chart";
import { Histogram } from "./Histogram";

const DURATION_METRICS = new Set(["http_req_duration"]);

/**
 * A threshold's measured value, in the unit of its stat: a count is a plain
 * number, and http_reqs' rate is requests per second, while the other rates
 * (failures, failed checks) are shares.
 */
function fmtThresholdValue(th: ThresholdResult): string {
  if (DURATION_METRICS.has(th.metric)) return fmtMs(th.actual);
  if (th.stat === "count") return fmtNum(th.actual);
  if (th.metric === "http_reqs") return `${fmtNum(th.actual)} req/s`;
  return fmtPct(th.actual);
}

function statePill(state: RunState): { cls: string; label: string } {
  switch (state) {
    case "passed":
      return { cls: "ok", label: "Passed" };
    case "failed":
      return { cls: "err", label: "Failed" };
    case "errored":
      return { cls: "err", label: "Errored" };
    case "stopped":
      return { cls: "warn", label: "Stopped" };
    default:
      return { cls: "neutral", label: "Running" };
  }
}

type TagSortKey = keyof TagStats;

/**
 * Name and notes for a run.
 *
 * Editing is local until saved so a half-typed name never lands on disk, and
 * the `key={runId}` at the call site reseeds the fields when another run is
 * opened.
 */
/**
 * Each round of a fixed-count run, and the comparison between them.
 *
 * The per-round wall time is the number a repeated blast is run for, and the
 * summary line above the table answers the question directly rather than
 * leaving it to be read off eight rows: did it get faster or slower, and by
 * how much.
 */
function RoundsTable({ rounds }: { rounds: RoundStats[] }) {
  const walls = rounds.map((r) => r.wallSec);
  const mean = walls.reduce((a, b) => a + b, 0) / walls.length;
  const fastest = Math.min(...walls);
  const slowest = Math.max(...walls);
  const first = walls[0];
  const last = walls[walls.length - 1];
  const change = last - first;
  // Only worth calling a trend when there is more than one round to compare
  // and the difference is not just timing noise.
  const trend =
    rounds.length > 1 && first > 0 && Math.abs(change) / first > 0.05
      ? change < 0
        ? { text: `${fmtDuration(Math.abs(change))} faster by the last round`, cls: "ok" }
        : { text: `${fmtDuration(change)} slower by the last round`, cls: "err" }
      : null;

  return (
    <div className="pad col" style={{ gap: 6 }}>
      <div className="row" style={{ fontWeight: 600 }}>Rounds</div>
      <div className="row wrap" style={{ gap: 10 }}>
        <span className="muted">
          Mean {fmtDuration(mean)} · fastest {fmtDuration(fastest)} · slowest{" "}
          {fmtDuration(slowest)}
        </span>
        {trend && <span className={`status-pill ${trend.cls}`}>{trend.text}</span>}
      </div>
      <table className="data-table">
        <thead>
          <tr>
            <th>Round</th>
            <th>Requests</th>
            <th>Conc.</th>
            <th>Wall time</th>
            <th>Req/s</th>
            <th>Errors</th>
            <th>p50</th>
            <th>p95</th>
            <th>p99</th>
            <th>Max</th>
            <th>Then waited</th>
          </tr>
        </thead>
        <tbody>
          {rounds.map((r) => (
            <tr key={r.index}>
              <td>{r.index}</td>
              <td>{fmtNum(r.stats.count)}</td>
              <td>{fmtNum(r.concurrency)}</td>
              <td>{fmtDuration(r.wallSec)}</td>
              <td>{fmtNum(r.rps)}</td>
              <td className={r.stats.errors > 0 ? "err" : ""}>{fmtNum(r.stats.errors)}</td>
              <td>{fmtMs(r.stats.p50)}</td>
              <td>{fmtMs(r.stats.p95)}</td>
              <td>{fmtMs(r.stats.p99)}</td>
              <td>{fmtMs(r.stats.max)}</td>
              <td>{r.gapSec ? `${r.gapSec}s` : "—"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * The numbers that were only in the exported JSON.
 *
 * The tiles above answer "how is it going"; this answers "what exactly
 * happened" — the wall time a batch run is entirely about, the full latency
 * spread rather than p95 alone, and the counters that are invisible until they
 * are not zero.
 */
function RunFacts({ summary }: { summary: RunSummary }) {
  const o = summary.overall;
  const succeeded = summary.totalRequests - summary.totalErrors;

  // Exception counters: shown only when they have something to say, so the
  // table does not carry three permanent zeroes.
  const exceptions: [string, string][] = [];
  if (summary.droppedIterations > 0) {
    exceptions.push(["Dropped iterations", fmtNum(summary.droppedIterations)]);
  }
  if (summary.tokenRefreshes > 0) {
    exceptions.push(["Token refreshes", fmtNum(summary.tokenRefreshes)]);
  }
  if (summary.samplesDropped > 0) {
    exceptions.push(["Samples dropped", fmtNum(summary.samplesDropped)]);
  }

  const facts: [string, string][] = [
    ["Wall time", fmtDuration(summary.durationSec)],
    ["Started", fmtTime(summary.startedAt)],
    ["Finished", fmtTime(summary.endedAt)],
    ["Succeeded", fmtNum(succeeded)],
    ["Failed", fmtNum(summary.totalErrors)],
    ["Mean rate", `${fmtNum(summary.rps)} req/s`],
    ["Peak rate", `${fmtNum(summary.peakRps)} req/s`],
    ...exceptions,
  ];

  return (
    <div className="pad col" style={{ gap: 12 }}>
      <div>
        <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>Overall latency</div>
        <table className="data-table">
          <thead>
            <tr>
              <th>Min</th>
              <th>Mean</th>
              <th>p50</th>
              <th>p90</th>
              <th>p95</th>
              <th>p99</th>
              <th>p99.9</th>
              <th>Max</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>{fmtMs(o.min)}</td>
              <td>{fmtMs(o.avg)}</td>
              <td>{fmtMs(o.p50)}</td>
              <td>{fmtMs(o.p90)}</td>
              <td>{fmtMs(o.p95)}</td>
              <td>{fmtMs(o.p99)}</td>
              <td>{fmtMs(o.p999)}</td>
              <td>{fmtMs(o.max)}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div>
        <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>Run details</div>
        <table className="data-table facts-table">
          <tbody>
            {facts.map(([label, value]) => (
              <tr key={label}>
                <td>{label}</td>
                <td>{value}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function RunNotes({
  annotation,
  scenarioName,
  onSave,
}: {
  annotation: RunAnnotation;
  scenarioName: string;
  onSave: (label: string | null, notes: string | null) => Promise<void>;
}) {
  const [label, setLabel] = useState(annotation.label ?? "");
  const [notes, setNotes] = useState(annotation.notes ?? "");
  const [saving, setSaving] = useState(false);
  const [open, setOpen] = useState(Boolean(annotation.label || annotation.notes));

  const dirty = label !== (annotation.label ?? "") || notes !== (annotation.notes ?? "");

  const save = async () => {
    setSaving(true);
    try {
      await onSave(label || null, notes || null);
    } finally {
      setSaving(false);
    }
  };

  if (!open) {
    return (
      <div className="pad">
        <button className="btn sm ghost" onClick={() => setOpen(true)}>
          <Icons.Edit /> Name this run
        </button>
      </div>
    );
  }

  return (
    <div className="col pad" style={{ gap: 8 }}>
      <div className="field">
        <label htmlFor="run-label">Name</label>
        <input
          id="run-label"
          className="input"
          value={label}
          placeholder={scenarioName}
          onChange={(e) => setLabel(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && dirty) void save();
          }}
        />
      </div>
      <div className="field">
        <label htmlFor="run-notes">Notes</label>
        <textarea
          id="run-notes"
          className="textarea"
          rows={3}
          value={notes}
          placeholder="What was different about this run — config, server state, what you were testing."
          onChange={(e) => setNotes(e.target.value)}
        />
      </div>
      <div className="row" style={{ gap: 8 }}>
        <button className="btn sm primary" onClick={() => void save()} disabled={!dirty || saving}>
          {saving ? "Saving…" : "Save"}
        </button>
        {dirty ? (
          <span className="hint">Unsaved</span>
        ) : (
          label || notes ? <span className="hint">Saved</span> : null
        )}
      </div>
    </div>
  );
}

export function RunScreen() {
  const current = useLoad((s) => s.current);
  const annotateRun = useLoad((s) => s.annotateRun);
  const tests = useLoad((s) => s.tests);
  const refreshTests = useLoad((s) => s.refreshTests);
  // Already on the Runs section, so starting a run needs no navigation.
  const { launch, dialog } = useRunLauncher(() => {});

  // The scenario list is what says whether this run can be repeated; the Runs
  // section does not otherwise need it.
  useEffect(() => {
    if (tests.length === 0) void refreshTests();
  }, [tests.length, refreshTests]);

  // Where the scenario is *now*. A run records the path it used, but the
  // scenario may have been renamed since, so the id is what actually finds it.
  const [scenarioRef, setScenarioRef] = useState<string | null>(null);
  const summaryRef = current?.summary?.scenarioRef;
  const summaryId = current?.summary?.scenarioId;
  useEffect(() => {
    if (!summaryRef) {
      setScenarioRef(null);
      return;
    }
    let cancelled = false;
    void api
      .loadTestLocate(summaryRef, summaryId)
      .then((found) => {
        if (!cancelled) setScenarioRef(found);
      })
      .catch(() => {
        if (!cancelled) setScenarioRef(null);
      });
    return () => {
      cancelled = true;
    };
  }, [summaryRef, summaryId]);
  const [stopping, setStopping] = useState(false);
  const [sortKey, setSortKey] = useState<TagSortKey>("count");
  const [sortDir, setSortDir] = useState<1 | -1>(-1);
  const themeVersion = useThemeVersion();

  const snapshots = current?.snapshots ?? [];
  const last = snapshots[snapshots.length - 1];
  const summary = current?.summary ?? null;

  const rpsSeries = useMemo<uPlot.AlignedData>(() => {
    const xs = snapshots.map((s) => s.elapsedSec);
    const rps = snapshots.map((s) => s.rps);
    const err = snapshots.map((s) => s.errorRate * 100);
    return [xs, rps, err];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshots]);

  const latencySeries = useMemo<uPlot.AlignedData>(() => {
    const xs = snapshots.map((s) => s.elapsedSec);
    return [xs, snapshots.map((s) => s.p50), snapshots.map((s) => s.p95), snapshots.map((s) => s.p99)];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshots]);

  const vusSeries = useMemo<uPlot.AlignedData>(() => {
    const xs = snapshots.map((s) => s.elapsedSec);
    return [xs, snapshots.map((s) => s.activeVus), snapshots.map((s) => s.targetVus)];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshots]);

  const rpsSeriesDef = useMemo<uPlot.Series[]>(
    () => [
      {},
      { label: "Requests/sec", stroke: cssVar("--accent"), scale: "rps", width: 2 },
      { label: "Error rate %", stroke: cssVar("--danger"), scale: "err", width: 2 },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [themeVersion],
  );

  const latencySeriesDef = useMemo<uPlot.Series[]>(
    () => [
      {},
      { label: "p50", stroke: cssVar("--ok"), width: 2 },
      { label: "p95", stroke: cssVar("--warn"), width: 2 },
      { label: "p99", stroke: cssVar("--danger"), width: 2 },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [themeVersion],
  );

  const vusSeriesDef = useMemo<uPlot.Series[]>(
    () => [
      {},
      { label: "Active VUs", stroke: cssVar("--accent"), width: 2 },
      { label: "Target VUs", stroke: cssVar("--text-faint"), width: 2, dash: [4, 4] },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [themeVersion],
  );

  const perTag = useMemo(() => {
    const rows = last?.perTag ?? summary?.perTag ?? [];
    const copy = rows.slice();
    copy.sort((a, b) => {
      const av = a[sortKey];
      const bv = b[sortKey];
      if (av === bv) return 0;
      return av > bv ? sortDir : -sortDir;
    });
    return copy;
  }, [last, summary, sortKey, sortDir]);

  const checks = last?.checks ?? summary?.checks ?? [];
  const thresholds = last?.thresholdResults ?? summary?.thresholds ?? [];
  // Only the final summary carries the status breakdown; live snapshots do not.
  const statusCodes = summary?.statusCodes ?? [];
  const errorsByMessage = summary?.errorsByMessage ?? [];
  // Only the final summary carries the distribution; live snapshots do not.
  const latencyDistribution = summary?.latencyDistribution ?? [];
  const tokenRefreshes = summary?.tokenRefreshes ?? 0;
  const stoppedBecause = summary?.stoppedBecause ?? null;
  const annotation = current?.annotation ?? {};

  const samplesDropped = summary?.samplesDropped ?? last?.samplesDropped ?? 0;
  const droppedIterations = summary?.droppedIterations ?? last?.droppedIterations ?? 0;
  const vusSaturated = last?.vusSaturated ?? false;

  if (!current) return null;

  const { cls, label } = current.live ? { cls: "neutral", label: "Running" } : statePill(summary?.state ?? "running");
  const elapsedSec = last?.elapsedSec ?? summary?.durationSec ?? 0;

  const activeVus = last?.activeVus ?? 0;
  const targetVus = last?.targetVus ?? 0;
  // A snapshot's rates cover only its last interval, so once a run is over
  // the final (often partial) interval would stand in for the whole run.
  const rates = !current.live && summary ? summary : last ?? summary;
  const rps = rates?.rps ?? 0;
  const errorRate = last?.errorRate ?? summary?.errorRate ?? 0;
  const p95 = last?.p95 ?? summary?.overall?.p95 ?? 0;
  const totalRequests = last?.totalRequests ?? summary?.totalRequests ?? 0;
  const bytesIn = last?.bytesIn ?? summary?.bytesIn ?? 0;
  const bytesPerSec = rates?.bytesPerSec ?? 0;
  const bytesOut = last?.bytesOut ?? summary?.bytesOut ?? 0;
  const bytesOutPerSec = rates?.bytesOutPerSec ?? 0;
  const peakRps = summary?.peakRps ?? 0;
  // A run outlives the scenario it came from, so re-running is only offered
  // while that scenario is still there to run.
  const scenarioStillExists = scenarioRef != null;

  const toggleSort = (key: TagSortKey) => {
    if (sortKey === key) setSortDir((d) => (d === 1 ? -1 : 1));
    else {
      setSortKey(key);
      setSortDir(-1);
    }
  };

  // Write the run to a file the user picks, then offer to show it.
  const exportRun = async (format: "html" | "json") => {
    const base = (annotation.label || current.name).replace(/[^\w.-]+/g, "-");
    // Stamped with when the run started, so exports of different runs of the
    // same scenario do not all land on one filename and quietly overwrite.
    const stamp = fileStamp(summary?.startedAt ?? Date.now());
    try {
      const path = await save({
        defaultPath: stamp ? `${base}_${stamp}.${format}` : `${base}.${format}`,
        filters: [
          format === "html"
            ? { name: "HTML report", extensions: ["html"] }
            : { name: "JSON", extensions: ["json"] },
        ],
      });
      if (!path) return;
      await api.runExport(current.runId, path, format);
      notify("Report saved");
      // Showing it in the file manager is the usual next step when the point
      // of exporting was to send the file to someone.
      await revealItemInDir(path).catch(() => {});
    } catch (e) {
      reportError("Could not export this run", e);
    }
  };

  const stop = async () => {
    setStopping(true);
    try {
      await api.loadStop(current.runId);
    } catch (e) {
      reportError("Could not stop the run", e);
    } finally {
      setStopping(false);
    }
  };

  return (
    <div className="scroll">
      <div className="panel-header">
        <span className="grow">{annotation.label || current.name}</span>
        <span className={`status-pill ${cls}`}>
          {current.live && <span className="spinner" />}
          {label}
        </span>
        <span className="muted">
          {current.live ? `${fmtNum(elapsedSec)} s elapsed` : fmtDuration(elapsedSec)}
        </span>
        {!current.live && summary && (
          <button
            className="btn sm"
            disabled={!scenarioStillExists}
            title={
              scenarioStillExists
                ? "Run this scenario again"
                : "The scenario this run came from is no longer in the workspace."
            }
            onClick={() => scenarioRef && launch(scenarioRef)}
          >
            <Icons.Play /> Run again
          </button>
        )}
        {!current.live && (
          <Menu
            label="Export"
            icon={<Icons.File />}
            title="Save this run to a file"
            items={[
              {
                label: "HTML report",
                hint: "Self-contained; send to anyone",
                onSelect: () => void exportRun("html"),
              },
              {
                label: "JSON",
                hint: "Summary and timeline",
                onSelect: () => void exportRun("json"),
              },
            ]}
          />
        )}
        {current.live && (
          <button className="btn danger" onClick={() => void stop()} disabled={stopping}>
            {stopping ? "Stopping…" : "Stop"}
          </button>
        )}
      </div>

      <div className="stat-row">
        <div className="stat-tile">
          <div className="stat-label">Active VUs</div>
          <div className="stat-value">{fmtNum(activeVus)}</div>
          <div className="stat-sub">Target {fmtNum(targetVus)}</div>
        </div>
        <div className="stat-tile">
          <div className="stat-label">Requests / sec</div>
          <div className="stat-value">{fmtNum(rps)}</div>
        </div>
        <div className="stat-tile">
          <div className="stat-label">Error rate</div>
          <div className={`stat-value ${errorRate > 0 ? "err" : ""}`}>{fmtPct(errorRate)}</div>
        </div>
        <div className="stat-tile">
          <div className="stat-label">p95</div>
          <div className="stat-value">{fmtMs(p95)}</div>
        </div>
        <div className="stat-tile">
          <div className="stat-label">Total requests</div>
          <div className="stat-value">{fmtNum(totalRequests)}</div>
          {peakRps > 0 && <div className="stat-sub">Peak {fmtNum(peakRps)} req/s</div>}
        </div>
        <div className="stat-tile">
          <div className="stat-label">Data sent</div>
          <div className="stat-value">{fmtBytes(bytesOut)}</div>
          <div className="stat-sub">{fmtBytes(bytesOutPerSec)}/s</div>
        </div>
        <div className="stat-tile">
          <div className="stat-label">Data received</div>
          <div className="stat-value">{fmtBytes(bytesIn)}</div>
          <div className="stat-sub">{fmtBytes(bytesPerSec)}/s</div>
        </div>
      </div>

      {/* Directly under the tiles, before any of the detail: naming a run is
          something you do while looking at its headline numbers, and it is
          the same first item whatever kind of run this is. */}
      <RunNotes
        key={current.runId}
        annotation={annotation}
        scenarioName={current.name}
        onSave={annotateRun}
      />

      {summary && <RunFacts summary={summary} />}
      {!!summary?.rounds?.length && <RoundsTable rounds={summary.rounds} />}

      {stoppedBecause && (
        <div className="col pad" style={{ gap: 6 }}>
          <div className="status-pill ok" style={{ alignSelf: "flex-start" }}>
            <Icons.Check /> {stoppedBecause}
          </div>
          <div className="hint">
            The run ended on its stop condition rather than finishing its ramp —
            which is what a breaking-point run is for. Pass or fail still comes
            from the thresholds.
          </div>
        </div>
      )}

      {tokenRefreshes > 0 && (
        <div className="col pad" style={{ gap: 6 }}>
          <div className="status-pill neutral" style={{ alignSelf: "flex-start" }}>
            The auth token expired and was refreshed {fmtNum(tokenRefreshes)} time
            {tokenRefreshes === 1 ? "" : "s"} during this run. The requests that
            triggered a refresh include the time it took.
          </div>
        </div>
      )}

      {(samplesDropped > 0 || droppedIterations > 0 || vusSaturated) && (
        <div className="col pad" style={{ gap: 6 }}>
          {samplesDropped > 0 && (
            <div className="status-pill warn" style={{ alignSelf: "flex-start" }}>
              <Icons.Alert /> The load generator could not record every sample in time; {fmtNum(samplesDropped)}{" "}
              samples were dropped from the stats below.
            </div>
          )}
          {droppedIterations > 0 && (
            <div className="status-pill warn" style={{ alignSelf: "flex-start" }}>
              <Icons.Alert /> The load generator could not keep up with the target arrival rate; {fmtNum(droppedIterations)}{" "}
              iterations were dropped.
            </div>
          )}
          {vusSaturated && (
            <div className="status-pill warn" style={{ alignSelf: "flex-start" }}>
              <Icons.Alert /> The run hit its maximum virtual user limit, so it could not ramp up any further.
            </div>
          )}
        </div>
      )}

      <div className="col pad" style={{ gap: 16 }}>
        <Chart
          title="Requests/sec and error rate"
          data={rpsSeries}
          series={rpsSeriesDef}
          height={180}
          yLabels={{ rps: "Requests / sec", err: "Error rate (%)" }}
        />
        <Chart
          title="Latency (ms)"
          data={latencySeries}
          series={latencySeriesDef}
          height={180}
          yLabels={{ y: "Latency (ms)" }}
        />
        <Chart
          title="Virtual users"
          data={vusSeries}
          series={vusSeriesDef}
          height={180}
          yLabels={{ y: "Virtual users" }}
        />
        {latencyDistribution.length > 0 && (
          <Histogram buckets={latencyDistribution} p999={summary?.overall.p999 ?? 0} />
        )}
      </div>

      <div className="pad">
        <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>By tag</div>
        <div className="scroll" style={{ maxHeight: 320 }}>
          <table className="data-table">
            <thead>
              <tr>
                {(
                  [
                    ["tag", "Tag"],
                    ["count", "Count"],
                    ["errors", "Errors"],
                    ["min", "Min"],
                    ["p50", "p50"],
                    ["p90", "p90"],
                    ["p95", "p95"],
                    ["p99", "p99"],
                    ["p999", "p99.9"],
                    ["max", "Max"],
                    ["avg", "Avg"],
                    ["bytesIn", "Data"],
                  ] as [TagSortKey, string][]
                ).map(([key, label2]) => (
                  <th key={key} onClick={() => toggleSort(key)}>
                    {label2}
                    {sortKey === key ? (sortDir === 1 ? " ▲" : " ▼") : ""}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {perTag.map((tag) => (
                <tr key={tag.tag}>
                  <td>{tag.tag}</td>
                  <td>{fmtNum(tag.count)}</td>
                  <td>{fmtNum(tag.errors)}</td>
                  <td>{fmtMs(tag.min)}</td>
                  <td>{fmtMs(tag.p50)}</td>
                  <td>{fmtMs(tag.p90)}</td>
                  <td>{fmtMs(tag.p95)}</td>
                  <td>{fmtMs(tag.p99)}</td>
                  <td>{fmtMs(tag.p999)}</td>
                  <td>{fmtMs(tag.max)}</td>
                  <td>{fmtMs(tag.avg)}</td>
                  <td>{fmtBytes(tag.bytesIn)}</td>
                </tr>
              ))}
              {perTag.length === 0 && (
                <tr>
                  <td colSpan={12} className="empty">
                    No requests yet.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      {dialog}

      {statusCodes.length > 0 && (
        <div className="pad">
          <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>
            Status codes
          </div>
          <table className="data-table">
            <thead>
              <tr>
                <th>Status</th>
                <th>Count</th>
                <th>Share</th>
              </tr>
            </thead>
            <tbody>
              {statusCodes.map((sc) => {
                const { label, cls } = describeStatus(sc);
                const share = totalRequests > 0 ? sc.count / totalRequests : 0;
                return (
                  <tr key={`${sc.protocol}-${sc.code}`}>
                    <td>
                      <span className={`status-pill ${cls}`}>{label}</span>
                    </td>
                    <td>{fmtNum(sc.count)}</td>
                    <td>{fmtPct(share)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <div className="hint" style={{ marginTop: 6 }}>
            What the server actually replied. Anything other than gRPC 0 or a
            2xx is a failed request, and which code it is says why.
          </div>
        </div>
      )}

      {errorsByMessage.length > 0 && (
        <div className="pad">
          <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>
            Failures
          </div>
          <table className="data-table text">
            <thead>
              <tr>
                <th>Cause</th>
                <th>Count</th>
              </tr>
            </thead>
            <tbody>
              {errorsByMessage.map((e) => (
                <tr key={e.message}>
                  <td>{e.message}</td>
                  <td>{fmtNum(e.count)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="hint" style={{ marginTop: 6 }}>
            Requests that never produced a response, so they have no status
            code to explain them. This is where a refused connection, a
            timeout, or an exhausted socket pool shows up.
          </div>
        </div>
      )}

      {checks.length > 0 && (
        <div className="pad">
          <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>Checks</div>
          <table className="data-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Passes</th>
                <th>Fails</th>
              </tr>
            </thead>
            <tbody>
              {checks.map((c) => (
                <tr key={c.name}>
                  <td>{c.name}</td>
                  <td>{fmtNum(c.passes)}</td>
                  <td>{fmtNum(c.fails)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {thresholds.length > 0 && (
        <div className="pad">
          <div className="row" style={{ fontWeight: 600, marginBottom: 6 }}>Thresholds</div>
          <div className="col" style={{ gap: 6 }}>
            {thresholds.map((th: ThresholdResult, i) => (
              <div key={i} className="row">
                {th.passed ? (
                  <span style={{ color: "var(--ok)" }}>
                    <Icons.Check />
                  </span>
                ) : (
                  <span style={{ color: "var(--danger)" }}>
                    <Icons.Alert />
                  </span>
                )}
                <span className="grow">{th.description}</span>
                <span className="mono">{fmtThresholdValue(th)}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
