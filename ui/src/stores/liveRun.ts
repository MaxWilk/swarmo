/**
 * How a run's incoming events are filed.
 *
 * Deliberately free of any Tauri or API import so it can be unit tested
 * directly: the rule it encodes — that the run in progress and the run on
 * screen are different things — is the sort that breaks quietly.
 */
import type { RunAnnotation, RunSummary, Snapshot } from "../api/types";

/** A run being watched live, or replayed from disk. */
export interface LiveRun {
  runId: string;
  name: string;
  live: boolean;
  snapshots: Snapshot[];
  summary: RunSummary | null;
  /** The user's name and notes for this run. */
  annotation: RunAnnotation;
}

/** The two runs an event can be about: the one running, and the one shown. */
export interface RunSlots {
  live: LiveRun | null;
  current: LiveRun | null;
}

/**
 * Where a newly arrived snapshot belongs.
 *
 * A snapshot has to reach the live run whether or not it is being watched.
 * Dropping snapshots for an unwatched run leaves a hole in the timeline of the
 * run you come back to.
 */
export function applySnapshot(
  state: RunSlots,
  runId: string,
  snapshot: Snapshot,
): Partial<RunSlots> {
  const append = (r: LiveRun): LiveRun => ({ ...r, snapshots: [...r.snapshots, snapshot] });
  const patch: Partial<RunSlots> = {};
  if (state.live && state.live.runId === runId) patch.live = append(state.live);
  if (state.current && state.current.runId === runId) patch.current = append(state.current);
  return patch;
}

/** The same, for the summary that arrives when a run finishes. */
export function applyDone(
  state: RunSlots,
  runId: string,
  summary: RunSummary,
): Partial<RunSlots> {
  const finish = (r: LiveRun): LiveRun => ({ ...r, live: false, summary });
  const patch: Partial<RunSlots> = {};
  if (state.live && state.live.runId === runId) patch.live = finish(state.live);
  if (state.current && state.current.runId === runId) patch.current = finish(state.current);
  return patch;
}
