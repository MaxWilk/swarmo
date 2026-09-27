import { create } from "zustand";
import * as api from "../api";
import type {
  LoadTestEntry,
  RunListEntry,
} from "../api";
import { reportError } from "./toast";
import { applyDone, applySnapshot, type LiveRun } from "./liveRun";

export type { LiveRun };

interface LoadState {
  /** Every load test, flat — for lookups by ref. */
  tests: LoadTestEntry[];
  /** The same tests nested by folder, for the sidebar. */
  testTree: LoadTestEntry[];
  runs: RunListEntry[];
  /** Runs the engine is still working on, so they cannot be deleted yet. */
  activeRunIds: string[];
  /** The run currently shown on the Run screen. */
  current: LiveRun | null;
  /**
   * The run the engine is still working on, if any.
   *
   * Kept apart from `current` so that looking at an old report does not stop
   * the running one being recorded — its snapshots arrive whether or not it is
   * the thing on screen — and so there is always a way back to it.
   */
  live: LiveRun | null;
  /** Two runs being compared, oldest first. Takes precedence over `current`. */
  compare: { baseline: string; candidate: string } | null;
  /**
   * Narrow the Runs list to one load test.
   *
   * Held by the scenario's stable id rather than its path, so the scope
   * survives the test being renamed or moved — which is the whole reason runs
   * are not nested under a folder tree in the first place. `name` is only for
   * the chip; it is not what matching uses.
   */
  runScope: { scenarioId: string; name: string } | null;
  listening: boolean;

  setRunScope: (scope: { scenarioId: string; name: string } | null) => void;
  refreshTests: () => Promise<void>;
  refreshRuns: () => Promise<void>;
  startListening: () => Promise<void>;
  beginRun: (runId: string, name: string) => void;
  openRun: (runId: string) => Promise<void>;
  /** Show the run in progress again. */
  viewLive: () => void;
  openCompare: (a: string, b: string) => void;
  closeCompare: () => void;
  annotateRun: (label: string | null, notes: string | null) => Promise<void>;
  clearCurrent: () => void;
  resetWorkspace: () => void;
}

/** Bumped whenever the shown run changes, so a stale openRun can tell. */
let openSeq = 0;

export const useLoad = create<LoadState>((set, get) => ({
  tests: [],
  testTree: [],
  runs: [],
  activeRunIds: [],
  current: null,
  live: null,
  compare: null,
  runScope: null,
  listening: false,

  refreshTests: async () => {
    try {
      // Both shapes in one round trip, so the sidebar and the lookups can
      // never disagree about what exists.
      const [tests, testTree] = await Promise.all([api.loadList(), api.loadTree()]);
      set({ tests, testTree });
    } catch (e) {
      reportError("Could not list load tests", e);
    }
  },

  refreshRuns: async () => {
    try {
      const [runs, activeRunIds] = await Promise.all([
        api.runsList(),
        api.loadActiveRuns(),
      ]);
      set({ runs, activeRunIds });
    } catch (e) {
      reportError("Could not list runs", e);
    }
  },

  startListening: async () => {
    if (get().listening) return;
    set({ listening: true });

    let stopSnapshots: (() => void) | null = null;
    try {
      stopSnapshots = await api.onLoadSnapshot(({ runId, snapshot }) => {
        const patch = applySnapshot(get(), runId, snapshot);
        if (Object.keys(patch).length > 0) set(patch);
      });

      await api.onLoadDone(async ({ runId, summary }) => {
        const patch = applyDone(get(), runId, summary);
        if (Object.keys(patch).length > 0) set(patch);

        await get().refreshRuns();
        // Only stop pinning it once it is listed, or it would be briefly
        // unreachable between finishing and appearing in the history.
        if (get().live?.runId === runId) set({ live: null });
      });
    } catch (e) {
      // If the second registration failed, remove the first before allowing a
      // retry; otherwise every retry would duplicate snapshot events.
      stopSnapshots?.();
      // Without these, live runs simply do not stream; the run still completes
      // and can be opened from the Runs list afterwards.
      set({ listening: false });
      reportError("Live run updates are unavailable", e);
    }
  },

  beginRun: (runId, name) => {
    const run: LiveRun = {
      runId,
      name,
      live: true,
      snapshots: [],
      summary: null,
      annotation: {},
    };
    openSeq++;
    set({ current: run, live: run, compare: null });
  },

  viewLive: () => {
    const live = get().live;
    if (live) {
      openSeq++;
      set({ current: live, compare: null });
    }
  },

  openRun: async (runId) => {
    // A slow load must not land on top of a run picked (or begun) since.
    const seq = ++openSeq;
    try {
      const [summary, snapshots, annotation] = await Promise.all([
        api.runGet(runId),
        api.runTimeline(runId),
        api.runAnnotationGet(runId),
      ]);
      if (seq !== openSeq) return;
      // An open comparison would otherwise keep covering the run just picked.
      set({
        compare: null,
        current: {
          runId,
          name: summary.scenarioName,
          live: false,
          snapshots,
          summary,
          annotation,
        },
      });
    } catch (e) {
      reportError("Could not open that run", e);
    }
  },

  annotateRun: async (label, notes) => {
    const cur = get().current;
    if (!cur) return;
    try {
      // The backend trims and drops blanks, so take back what it actually
      // saved rather than assuming the input survived unchanged.
      const annotation = await api.runAnnotationSet(cur.runId, label, notes);
      const now = get().current;
      if (now && now.runId === cur.runId) set({ current: { ...now, annotation } });
      await get().refreshRuns();
    } catch (e) {
      reportError("Could not save this run's name and notes", e);
    }
  },

  openCompare: (a, b) => {
    // Baseline is whichever ran first, so "Δ vs baseline" always reads as
    // change over time no matter which order they were picked in.
    const runs = get().runs;
    const at = (id: string) => runs.find((r) => r.runId === id)?.startedAt ?? 0;
    const [baseline, candidate] = at(a) <= at(b) ? [a, b] : [b, a];
    set({ compare: { baseline, candidate } });
  },

  closeCompare: () => set({ compare: null }),

  setRunScope: (runScope) => set({ runScope }),

  // The run in progress is deliberately not cleared: it is still running, and
  // the sidebar keeps offering the way back to it.
  clearCurrent: () => {
    openSeq++;
    set({ current: null, compare: null });
  },

  resetWorkspace: () =>
    set({
      tests: [],
      testTree: [],
      runs: [],
      activeRunIds: [],
      current: null,
      live: null,
      compare: null,
      runScope: null,
    }),
}));
