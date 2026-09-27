import { useEffect, useMemo, useState } from "react";
import * as api from "../../api";
import type { RunListEntry, RunState } from "../../api";
import {
  ConfirmModal,
  EmptyState,
  Icons,
  clockTime,
  fmtNum,
  fmtTime,
  groupByDay,
} from "../../components/ui";
import { useLoad } from "../../stores/load";
import { reportError, notify } from "../../stores/toast";
import { RunScreen } from "./RunScreen";
import { CompareScreen, ComparePrompt } from "./CompareScreen";

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

export function RunsView() {
  const {
    runs,
    activeRunIds,
    current,
    compare,
    live,
    refreshRuns,
    openRun,
    openCompare,
    viewLive,
    clearCurrent,
    runScope,
    setRunScope,
  } = useLoad();
  const [pendingDelete, setPendingDelete] = useState<RunListEntry | null>(null);
  const [query, setQuery] = useState("");
  // Non-null while picking runs to compare; holds the first pick.
  const [picking, setPicking] = useState<string[] | null>(null);

  // Escape leaves pick-two mode, so it is never a trap.
  useEffect(() => {
    if (picking === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPicking(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [picking]);

  const pick = (runId: string) => {
    const chosen = picking ?? [];
    if (chosen.includes(runId)) {
      setPicking(chosen.filter((id) => id !== runId));
      return;
    }
    const next = [...chosen, runId];
    if (next.length === 2) {
      openCompare(next[0], next[1]);
      setPicking(null);
      return;
    }
    setPicking(next);
  };

  useEffect(() => {
    void refreshRuns();
  }, [refreshRuns]);

  const sorted = useMemo(
    () => runs.slice().sort((a, b) => b.startedAt - a.startedAt),
    [runs],
  );

  // Scope first, then the text filter: the scope answers "which test", the
  // box answers "which run of it".
  const scoped = useMemo(() => {
    if (!runScope) return sorted;
    return sorted.filter((r) => r.scenarioId === runScope.scenarioId);
  }, [sorted, runScope]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return scoped;
    return scoped.filter(
      (r) =>
        (r.label ?? "").toLowerCase().includes(q) ||
        r.scenarioName.toLowerCase().includes(q) ||
        statePill(r.state).label.toLowerCase().includes(q),
    );
  }, [scoped, query]);

  // Already newest-first, which is what groupByDay assumes.
  const groups = useMemo(() => groupByDay(filtered, (r) => r.startedAt), [filtered]);

  const doDelete = async (entry: RunListEntry) => {
    try {
      await api.runDelete(entry.runId);
      // The run on screen no longer exists; leaving it up would let Export
      // and "Run again" fail against a directory that is gone.
      if (current?.runId === entry.runId) clearCurrent();
      await refreshRuns();
      notify("Run deleted");
    } catch (e) {
      reportError("Could not delete this run", e);
    }
  };

  return (
    <>
      <div className="sidebar">
        <div className="panel-header">
          <span className="grow">Runs</span>
          <button
            className={`btn sm ${picking !== null ? "primary" : "ghost"}`}
            title="Compare two runs"
            disabled={runs.length < 2}
            onClick={() => setPicking(picking === null ? [] : null)}
          >
            <Icons.Chart /> Compare
          </button>
        </div>
        {live && (
          <div
            className={`tree-row ${current?.runId === live.runId ? "selected" : ""}`}
            style={{
              height: "auto",
              padding: "8px 6px 8px 10px",
              alignItems: "flex-start",
              borderBottom: "1px solid var(--border)",
            }}
            onClick={() => viewLive()}
          >
            <div className="col grow" style={{ gap: 2 }}>
              <div className="row">
                <span className="tree-name" style={{ fontWeight: 600 }}>
                  {live.name}
                </span>
                <span className="status-pill neutral">
                  <span className="spinner" />
                  Running
                </span>
              </div>
              {/* A run in progress is not in the history yet, so without this
                  there would be no way back to it once you looked away. */}
              <div className="hint">In progress — click to watch</div>
            </div>
          </div>
        )}

        {runScope && (
          <div className="pad" style={{ paddingBottom: 0 }}>
            <div className="row scope-chip">
              <span className="muted nowrap">Runs of</span>
              <span className="grow" title={runScope.name}>
                {runScope.name}
              </span>
              <button
                className="btn ghost icon sm"
                title="Show every run again"
                onClick={() => setRunScope(null)}
              >
                <Icons.Close />
              </button>
            </div>
          </div>
        )}
        <div className="pad" style={{ paddingBottom: 0 }}>
          <input
            className="input"
            placeholder="Filter by name, scenario or state"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <div className="scroll">
          {runs.length === 0 && <div className="hint" style={{ padding: 10 }}>No runs yet.</div>}
          {runs.length > 0 && scoped.length === 0 && (
            <div className="col" style={{ padding: 10, gap: 6 }}>
              <div className="hint">No runs of this test yet.</div>
              <button
                className="btn sm ghost"
                style={{ alignSelf: "flex-start" }}
                onClick={() => setRunScope(null)}
              >
                Show all runs
              </button>
            </div>
          )}
          {scoped.length > 0 && filtered.length === 0 && (
            <div className="hint" style={{ padding: 10 }}>
              Nothing matches “{query}”.
            </div>
          )}
          {groups.map((group) => (
            <div key={group.day}>
              <div className="hint" style={{ padding: "8px 10px 2px", fontWeight: 600 }}>
                {group.day}
              </div>
              {group.items.map((run) => {
            const { cls, label } = statePill(run.state);
            return (
              <div
                key={run.runId}
                className={`tree-row ${current?.runId === run.runId ? "selected" : ""}`}
                style={{ height: "auto", padding: "8px 6px 8px 10px", alignItems: "flex-start" }}
                onClick={() => (picking !== null ? pick(run.runId) : void openRun(run.runId))}
              >
                {picking !== null && (
                  <input
                    className="checkbox"
                    type="checkbox"
                    style={{ marginTop: 3, marginRight: 6 }}
                    checked={picking.includes(run.runId)}
                    readOnly
                    tabIndex={-1}
                  />
                )}
                <div className="col grow" style={{ gap: 2 }}>
                  <div className="row">
                    <span className="tree-name" style={{ fontWeight: 600 }}>
                      {run.label || run.scenarioName}
                    </span>
                    {run.hasNotes && (
                      <span className="hint" title="This run has notes">
                        <Icons.Edit />
                      </span>
                    )}
                    <span className={`status-pill ${cls}`}>{label}</span>
                  </div>
                  {/* Keep the scenario visible once a run has its own name,
                      or the list stops saying what was actually run. */}
                  {run.label && <div className="hint">{run.scenarioName}</div>}
                  <div className="hint">
                    {clockTime(run.startedAt)} · {fmtNum(run.totalRequests)} requests
                  </div>
                </div>
                <span className="tree-actions">
                  <button
                    className="btn icon sm ghost"
                    // Deleting a live run only clears the directory its summary
                    // is about to be written into, so it would come straight
                    // back. The backend refuses too; this just explains why.
                    disabled={activeRunIds.includes(run.runId)}
                    title={
                      activeRunIds.includes(run.runId)
                        ? "Stop the run before deleting it."
                        : "Delete"
                    }
                    onClick={(e) => {
                      e.stopPropagation();
                      setPendingDelete(run);
                    }}
                  >
                    <Icons.Trash />
                  </button>
                </span>
              </div>
            );
              })}
            </div>
          ))}
        </div>
      </div>

      <div className="main">
        {compare ? (
          <CompareScreen />
        ) : picking !== null && picking.length > 0 ? (
          <ComparePrompt />
        ) : current ? (
          <RunScreen />
        ) : (
          <EmptyState title="No run selected">
            Runs appear here after you start one from the Load section.
          </EmptyState>
        )}
      </div>

      {pendingDelete && (
        <ConfirmModal
          title="Delete run"
          message={`Delete the run of "${pendingDelete.label || pendingDelete.scenarioName}" from ${fmtTime(pendingDelete.startedAt)}? This cannot be undone.`}
          onConfirm={() => void doDelete(pendingDelete)}
          onClose={() => setPendingDelete(null)}
        />
      )}
    </>
  );
}
