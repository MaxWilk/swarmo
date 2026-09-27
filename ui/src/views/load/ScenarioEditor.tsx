import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import * as api from "../../api";
import { flattenBlasts, flattenStages, isBlastRepeat, isRepeat, scenarioRounds } from "../../api";
import type {
  Blast,
  BlastItem,
  BlastRepeat,
  Capture,
  CaptureSource,
  LoadScenario,
  LoadStep,
  RepeatBlock,
  Stage,
  StopCondition,
  StageItem,
  Threshold,
  ThresholdOp,
  TreeNode,
} from "../../api";
import { Icons, Modal, MethodLabel } from "../../components/ui";
import { Editor } from "../../components/Editor";
import { GeneratorHelp } from "../../components/GeneratorHelp";
import { useWorkspace, walkTree, findNode, findNodeById } from "../../stores/workspace";
import { reportError, notify } from "../../stores/toast";

type Tab = "form" | "json";

const DURATION_METRICS = new Set(["http_req_duration"]);

function metricValueUnit(metric: string): "ms" | "value" {
  return DURATION_METRICS.has(metric) ? "ms" : "value";
}

export function ScenarioEditor({
  nodeRef,
  onRun,
  onDirtyChange,
}: {
  nodeRef: string;
  onRun: () => void;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [scenario, setScenario] = useState<LoadScenario | null>(null);
  const [dirty, setDirty] = useState(false);
  const [tab, setTab] = useState<Tab>("form");
  const [jsonText, setJsonText] = useState("");
  const [jsonError, setJsonError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const tree = useWorkspace((s) => s.tree);
  const environments = useWorkspace((s) => s.environments);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const s = await api.scenarioGet(nodeRef);
        if (cancelled) return;
        setScenario(s);
        setJsonText(JSON.stringify(s, null, 2));
        setJsonError(null);
        setDirty(false);
      } catch (e) {
        reportError("Could not load this scenario", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeRef]);

  useEffect(() => {
    onDirtyChange?.(dirty);
    return () => onDirtyChange?.(false);
  }, [dirty, onDirtyChange]);

  const update = (next: LoadScenario) => {
    setScenario(next);
    // The JSON is rewritten from the form, so any parse error it had is gone.
    setJsonText(JSON.stringify(next, null, 2));
    setJsonError(null);
    setDirty(true);
  };

  const onJsonChange = (text: string) => {
    setJsonText(text);
    try {
      const raw = JSON.parse(text) as unknown;
      if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
        throw new Error("A scenario is a JSON object.");
      }
      // The lists the form indexes into are optional on disk — the backend
      // defaults them — so they are defaulted here too, or the form would
      // throw on `.map` the moment it was shown.
      const parsed = raw as LoadScenario;
      setScenario({
        ...parsed,
        stages: Array.isArray(parsed.stages) ? parsed.stages : [],
        steps: Array.isArray(parsed.steps) ? parsed.steps : [],
        thresholds: Array.isArray(parsed.thresholds) ? parsed.thresholds : [],
      });
      setJsonError(null);
      setDirty(true);
    } catch (e) {
      setJsonError(e instanceof Error ? e.message : String(e));
    }
  };

  const save = async (): Promise<boolean> => {
    if (!scenario) return false;
    setSaving(true);
    try {
      await api.scenarioSave(nodeRef, scenario);
      setDirty(false);
      notify("Scenario saved");
      return true;
    } catch (e) {
      reportError("Could not save this scenario", e);
      return false;
    } finally {
      setSaving(false);
    }
  };

  // The run reads the scenario from disk, and the preflight dialog shows
  // what it read: running unsaved edits would show the old numbers, execute
  // the old plan, and then navigate away and lose the edits.
  const runSaved = async () => {
    if (jsonError) return;
    if (dirty && !(await save())) return;
    onRun();
  };

  if (!scenario) {
    return (
      <div className="empty">
        <div className="spinner" />
      </div>
    );
  }

  return (
    <div className="col" style={{ height: "100%", minHeight: 0, gap: 0 }}>
      <div className="panel-header">
        <span className="grow">{scenario.name}</span>
        {dirty && <span className="tab-dirty" title="Unsaved changes" />}
        <button
          className="btn"
          onClick={() => void save()}
          disabled={!dirty || saving || jsonError !== null}
          title={jsonError ? "Fix the JSON before saving" : undefined}
        >
          {saving ? "Saving…" : "Save"}
        </button>
        <button
          className="btn primary"
          onClick={() => void runSaved()}
          disabled={saving || jsonError !== null}
          title={jsonError ? "Fix the JSON before running" : undefined}
        >
          Run
        </button>
      </div>
      <div className="subtabs">
        <button className={`subtab ${tab === "form" ? "active" : ""}`} onClick={() => setTab("form")}>
          Form
        </button>
        <button className={`subtab ${tab === "json" ? "active" : ""}`} onClick={() => setTab("json")}>
          JSON
        </button>
      </div>

      {tab === "json" ? (
        <div className="col grow" style={{ minHeight: 0 }}>
          {jsonError && <div className="status-pill err" style={{ margin: "8px 12px" }}>{jsonError}</div>}
          <div className="grow" style={{ minHeight: 0 }}>
            <Editor language="json" value={jsonText} onChange={onJsonChange} className="flush" />
          </div>
        </div>
      ) : (
        <div className="scroll">
          <LoadProfileSection scenario={scenario} environments={environments} onChange={update} />
          <div className="sep" />
          {/* Only one of these drives a run; showing an editor for a list the
              run will ignore invites editing it to no effect. Each list is
              kept, so switching modes back restores it. */}
          {isFixedCount(scenario) ? (
            <>
              <RoundsSection scenario={scenario} onChange={update} />
              <div className="sep" />
            </>
          ) : (
            scenario.arrivalRatePerSec == null && (
              <>
                <StagesSection scenario={scenario} onChange={update} />
                <div className="sep" />
              </>
            )
          )}
          <StepsSection
            scenario={scenario}
            tree={tree}
            onChange={update}
            pickerOpen={pickerOpen}
            setPickerOpen={setPickerOpen}
          />
          <div className="sep" />
          <ThresholdsSection scenario={scenario} onChange={update} />
        </div>
      )}
    </div>
  );
}

// -- load profile -------------------------------------------------------------

function LoadProfileSection({
  scenario,
  environments,
  onChange,
}: {
  scenario: LoadScenario;
  environments: string[];
  onChange: (s: LoadScenario) => void;
}) {
  const rateMode: "stages" | "constant" | "fixed" =
    scenario.iterations != null || scenario.blasts?.length
      ? "fixed"
      : scenario.arrivalRatePerSec != null
        ? "constant"
        : "stages";
  const closedModeDisablesConstant = scenario.mode === "closed";

  const setMode = (mode: LoadScenario["mode"]) => {
    if (mode === "closed" && scenario.arrivalRatePerSec != null) {
      onChange({ ...scenario, mode, arrivalRatePerSec: null });
      notify(
        "Constant rate cleared",
        "Closed mode ramps a count of virtual users, so a fixed arrivals/sec rate no longer applies.",
      );
    } else {
      onChange({ ...scenario, mode });
    }
  };

  const setRateMode = (mode: "stages" | "constant" | "fixed") => {
    // Rate mode is read off the data, so leaving fixed-count mode has to
    // clear the rounds — but not silently, when there is a real list there.
    const discardRounds = () => {
      const n = scenarioRounds(scenario).length;
      const trivial = n <= 1 && !scenario.blasts?.some((b) => "blasts" in b);
      return trivial || window.confirm(`Discard the ${n} configured rounds?`);
    };
    if (mode === "stages") {
      if (!discardRounds()) return;
      onChange({ ...scenario, arrivalRatePerSec: null, iterations: null, blasts: [] });
      return;
    }
    if (mode === "fixed") {
      // Seeded as a round list, so there is one way to say this rather than
      // two that can disagree. An older scenario's loose iterations/concurrency
      // are carried into the first round and then cleared.
      const existing = scenarioRounds(scenario);
      onChange({
        ...scenario,
        arrivalRatePerSec: null,
        iterations: null,
        concurrency: null,
        blasts: existing.length
          ? existing
          : [{ iterations: 1000, concurrency: 16, gapSec: 0 }],
      });
      return;
    }
    const totalStageDuration = flattenStages(scenario.stages).reduce(
      (sum, s) => sum + s.durationSec,
      0,
    );
    if (!discardRounds()) return;
    onChange({
      ...scenario,
      iterations: null,
      blasts: [],
      arrivalRatePerSec: scenario.arrivalRatePerSec ?? 100,
      durationSec: scenario.durationSec ?? (totalStageDuration > 0 ? totalStageDuration : 60),
    });
  };

  return (
    <div className="col pad">
      <div className="row" style={{ fontWeight: 600 }}>Load profile</div>
      <div className="row wrap" style={{ alignItems: "flex-start" }}>
        {rateMode !== "fixed" && (
          <>
            <div className="field" style={{ minWidth: 180 }}>
              <label>Mode</label>
              <select
                className="select"
                value={scenario.mode}
                onChange={(e) => setMode(e.target.value as LoadScenario["mode"])}
              >
                <option value="closed">Closed</option>
                <option value="open">Open</option>
              </select>
              <div className="hint">
                {scenario.mode === "closed"
                  ? "Stages ramp the number of concurrent virtual users."
                  : "Stages ramp the number of new arrivals started per second."}
              </div>
            </div>
            <div className="field" style={{ minWidth: 140 }}>
              <label>Max virtual users</label>
              <input
                className="input"
                type="number"
                min={1}
                value={scenario.maxVus}
                onChange={(e) => onChange({ ...scenario, maxVus: Number(e.target.value) || 0 })}
              />
            </div>
          </>
        )}
        <div className="field" style={{ minWidth: 180 }}>
          <label>Environment</label>
          <select
            className="select"
            value={scenario.environment ?? ""}
            onChange={(e) => onChange({ ...scenario, environment: e.target.value || null })}
          >
            <option value="">Use active environment</option>
            {environments.map((env) => (
              <option key={env} value={env}>
                {env}
              </option>
            ))}
          </select>
        </div>
        <div className="field" style={{ minWidth: 140 }}>
          <label>Timeout (ms)</label>
          <input
            className="input"
            type="number"
            min={0}
            value={scenario.timeoutMs}
            onChange={(e) => onChange({ ...scenario, timeoutMs: Number(e.target.value) || 0 })}
          />
        </div>
        <div className="field" style={{ minWidth: 140, justifyContent: "flex-end" }}>
          <label className="row" style={{ textTransform: "none", fontWeight: 400 }}>
            <input
              className="checkbox"
              type="checkbox"
              checked={scenario.verifyTls}
              onChange={(e) => onChange({ ...scenario, verifyTls: e.target.checked })}
            />
            Verify TLS
          </label>
        </div>
      </div>

      <div className="row wrap" style={{ alignItems: "flex-start" }}>
        <div className="field" style={{ minWidth: 320 }}>
          <label>Rate</label>
          <div className="row">
            <label className="row" style={{ textTransform: "none", fontWeight: 400 }}>
              <input
                type="radio"
                name="rate-mode"
                checked={rateMode === "stages"}
                onChange={() => setRateMode("stages")}
              />
              Ramp through stages
            </label>
            <label
              className="row"
              style={{ textTransform: "none", fontWeight: 400 }}
              title={closedModeDisablesConstant ? "Only available in open mode" : undefined}
            >
              <input
                type="radio"
                name="rate-mode"
                checked={rateMode === "constant"}
                disabled={closedModeDisablesConstant}
                onChange={() => setRateMode("constant")}
              />
              Hold a constant rate
            </label>
            <label
              className="row"
              style={{ textTransform: "none", fontWeight: 400 }}
              title="Send an exact number of requests and measure how long it takes"
            >
              <input
                type="radio"
                name="rate-mode"
                checked={rateMode === "fixed"}
                onChange={() => setRateMode("fixed")}
              />
              Fixed number of iterations
            </label>
          </div>
          {closedModeDisablesConstant && (
            <div className="hint">
              A constant rate is open mode only — closed mode ramps a count of virtual users, not
              an arrivals rate.
            </div>
          )}
        </div>

        {rateMode === "fixed" && (
          <div className="field" style={{ minWidth: 240 }}>
            <div className="hint">
              The batch-benchmark shape: how long do these iterations take with this many in
              flight? Set the rounds below. Stages, mode and max VUs do not apply.
            </div>
          </div>
        )}

        {rateMode === "constant" && (
          <>
            <div className="field" style={{ minWidth: 160 }}>
              <label>Requests per second</label>
              <input
                className="input"
                type="number"
                min={0}
                value={scenario.arrivalRatePerSec ?? 0}
                onChange={(e) => onChange({ ...scenario, arrivalRatePerSec: Number(e.target.value) || 0 })}
              />
            </div>
            <div className="field" style={{ minWidth: 160 }}>
              <label>Duration (seconds)</label>
              <input
                className="input"
                type="number"
                min={0}
                value={scenario.durationSec ?? 0}
                onChange={(e) => onChange({ ...scenario, durationSec: Number(e.target.value) || 0 })}
              />
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// -- rounds ---------------------------------------------------------------

/** Whether this scenario is measured in requests rather than in time. */
function isFixedCount(s: LoadScenario): boolean {
  return s.iterations != null || !!s.blasts?.length;
}

/**
 * The rounds of a fixed-count run.
 *
 * Deliberately the same shape as the stages editor — a list of entries, any of
 * which can be a block run several times over — because it is the same idea
 * counted in requests instead of seconds, and someone who has set up a ramp
 * should not have to learn a second set of controls.
 */
function RoundsSection({
  scenario,
  onChange,
}: {
  scenario: LoadScenario;
  onChange: (s: LoadScenario) => void;
}) {
  const items = scenarioRounds(scenario);

  // Every edit writes the round list and clears the two loose fields, so an
  // older scenario migrates the first time it is touched and there is only
  // ever one place the answer lives.
  const setItems = (blasts: BlastItem[]) =>
    onChange({ ...scenario, blasts, iterations: null, concurrency: null });

  const replaceItem = (i: number, item: BlastItem) => {
    const next = items.slice();
    next[i] = item;
    setItems(next);
  };
  const removeItem = (i: number) => setItems(items.filter((_, j) => j !== i));
  const move = (from: number, to: number) => {
    if (from === to || to < 0 || to >= items.length) return;
    const next = items.slice();
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    setItems(next);
  };

  const flat = flattenBlasts(items);
  const totalRequests = flat.reduce((n, b) => n + b.iterations, 0);
  const totalGap = flat.slice(0, -1).reduce((n, b) => n + (b.gapSec ?? 0), 0);

  return (
    <div className="col pad">
      <div className="row" style={{ fontWeight: 600 }}>Rounds</div>
      <div className="hint">
        Each round sends its requests as fast as its workers manage, then waits before the
        next one starts. Repeating a round is how you see whether the second blast is quicker
        because a cache is warm, or slower because something did not recover.
      </div>
      <div className="hint">
        A round set to <strong>+</strong> adds to the previous round's count rather than
        setting it, which is what makes a repeated block climb.
      </div>

      <StopWhenEditor scenario={scenario} onChange={onChange} />

      <table className="kv-table stage-table">
        <thead>
          <tr>
            <th style={{ width: 28 }} />
            <th>Requests</th>
            <th>Concurrency</th>
            <th>Then wait (s)</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {items.map((item, i) =>
            isBlastRepeat(item) ? (
              <tr key={i}>
                <td>{reorder(i)}</td>
                <td colSpan={4}>
                  <BlastRepeatEditor
                    block={item}
                    onChange={(b) => replaceItem(i, b)}
                    onRemove={() => removeItem(i)}
                  />
                </td>
              </tr>
            ) : (
              <tr key={i}>
                <td>{reorder(i)}</td>
                <BlastFields
                  blast={item}
                  onChange={(b) => replaceItem(i, b)}
                  onRemove={() => removeItem(i)}
                />
              </tr>
            ),
          )}
          {items.length === 0 && (
            <tr>
              <td colSpan={5} className="empty">
                No rounds yet — add one below.
              </td>
            </tr>
          )}
        </tbody>
      </table>

      <div className="row" style={{ gap: 8, marginTop: 6 }}>
        <button
          className="btn sm"
          onClick={() => setItems([...items, { iterations: 1000, concurrency: 16, gapSec: 0 }])}
        >
          Add round
        </button>
        <button
          className="btn sm ghost"
          title="A block of rounds run several times over"
          onClick={() =>
            setItems([
              ...items,
              {
                times: 3,
                blasts: [{ iterations: 5000, concurrency: 64, gapSec: 5 }],
              },
            ])
          }
        >
          <Icons.Plus /> Add repeat
        </button>
        <span className="right muted">
          {flat.length} round{flat.length === 1 ? "" : "s"} · {totalRequests.toLocaleString()}{" "}
          request{totalRequests === 1 ? "" : "s"}
          {totalGap > 0 && ` · ${totalGap}s of gaps`}
        </span>
      </div>
    </div>
  );

  function reorder(i: number) {
    return (
      <div className="col" style={{ gap: 1 }}>
        <button
          className="btn ghost icon sm"
          title="Move up"
          disabled={i === 0}
          onClick={() => move(i, i - 1)}
        >
          ↑
        </button>
        <button
          className="btn ghost icon sm"
          title="Move down"
          disabled={i === items.length - 1}
          onClick={() => move(i, i + 1)}
        >
          ↓
        </button>
      </div>
    );
  }
}

function BlastFields({
  blast,
  onChange,
  onRemove,
}: {
  blast: Blast;
  onChange: (b: Blast) => void;
  onRemove: () => void;
}) {
  return (
    <>
      <td>
        <div className="row" style={{ gap: 4 }}>
          <button
            className={`btn icon sm ${blast.relative ? "primary" : "ghost"}`}
            title={
              blast.relative
                ? "Adds to the previous round's count"
                : "Sets the count outright — click to add to the previous round instead"
            }
            onClick={() => onChange({ ...blast, relative: !blast.relative })}
          >
            {blast.relative ? "+" : "="}
          </button>
          <input
            className="input grow"
            type="number"
            min={0}
            value={blast.iterations}
            onChange={(e) => onChange({ ...blast, iterations: Math.max(0, Number(e.target.value) || 0) })}
          />
        </div>
      </td>
      <td>
        <input
          className="input"
          type="number"
          min={1}
          value={blast.concurrency}
          onChange={(e) => onChange({ ...blast, concurrency: Math.max(1, Number(e.target.value) || 1) })}
        />
      </td>
      <td>
        <input
          className="input"
          type="number"
          min={0}
          value={blast.gapSec ?? 0}
          onChange={(e) => onChange({ ...blast, gapSec: Math.max(0, Number(e.target.value) || 0) })}
        />
      </td>
      <td>
        <button className="btn ghost icon sm" title="Remove" onClick={onRemove}>
          <Icons.Close />
        </button>
      </td>
    </>
  );
}

function BlastRepeatEditor({
  block,
  onChange,
  onRemove,
}: {
  block: BlastRepeat;
  onChange: (b: BlastRepeat) => void;
  onRemove: () => void;
}) {
  const setBlast = (i: number, b: Blast) => {
    const blasts = block.blasts.slice();
    blasts[i] = b;
    onChange({ ...block, blasts });
  };

  return (
    <div className="col repeat-block">
      <div className="row wrap">
        <span className="nowrap" style={{ fontWeight: 600 }}>Repeat</span>
        <input
          className="input"
          style={{ width: 70 }}
          type="number"
          min={1}
          value={block.times}
          onChange={(e) => onChange({ ...block, times: Math.max(1, Number(e.target.value) || 1) })}
        />
        <span className="nowrap muted">times</span>
        <span className="nowrap muted">· each pass ×</span>
        <input
          className="input"
          style={{ width: 70 }}
          type="number"
          min={0}
          step={0.1}
          value={block.iterationScale ?? 1}
          onChange={(e) => {
            const v = Number(e.target.value);
            onChange({ ...block, iterationScale: Number.isFinite(v) && v > 0 ? v : null });
          }}
          title="Multiply every count in the block by this, once per repetition. 2 doubles the batch each pass."
        />
        <button className="btn ghost icon sm right" title="Remove" onClick={onRemove}>
          <Icons.Close />
        </button>
      </div>
      <table className="kv-table">
        <tbody>
          {block.blasts.map((b, i) => (
            <tr key={i}>
              <BlastFields
                blast={b}
                onChange={(next) => setBlast(i, next)}
                onRemove={() =>
                  onChange({ ...block, blasts: block.blasts.filter((_, j) => j !== i) })
                }
              />
            </tr>
          ))}
        </tbody>
      </table>
      <div className="row">
        <button
          className="btn sm ghost"
          onClick={() =>
            onChange({
              ...block,
              blasts: [...block.blasts, { iterations: 1000, concurrency: 16, gapSec: 0 }],
            })
          }
        >
          <Icons.Plus /> Add round to block
        </button>
      </div>
    </div>
  );
}

// -- stages ---------------------------------------------------------------

/**
 * The shape a run will take, drawn from the stage list.
 *
 * Sized from the container rather than a fixed width, so it fills the panel at
 * any window size. `preserveAspectRatio="none"` is deliberately avoided —
 * stretching the viewBox would thicken vertical strokes and skew the text — so
 * the viewBox tracks the measured width instead.
 */
function StagePreview({ points, yLabel }: { points: [number, number][]; yLabel: string }) {
  const host = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(480);

  // Measured in a layout effect so the first paint is already the right
  // width; a ResizeObserver only has to catch later changes. Waiting for the
  // observer alone would render one frame at the fallback width.
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return;
    const measure = () => setWidth(el.clientWidth || 480);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const H = 132;
  const padL = 46;
  const padR = 10;
  const padT = 10;
  const padB = 26;
  const W = Math.max(240, width);

  const maxT = Math.max(1, points[points.length - 1]?.[0] ?? 1);
  const maxV = Math.max(1, ...points.map((p) => p[1]));
  const px = (t: number) => padL + (t / maxT) * (W - padL - padR);
  const py = (v: number) => H - padB - (v / maxV) * (H - padT - padB);

  const line = points.map(([t, v]) => `${px(t).toFixed(1)},${py(v).toFixed(1)}`).join(" ");
  // Closing the path back down to the baseline makes the ramp read as a
  // profile rather than a bare squiggle.
  const area = `${px(0).toFixed(1)},${py(0).toFixed(1)} ${line} ${px(maxT).toFixed(1)},${py(0).toFixed(1)}`;

  return (
    <div ref={host} style={{ width: "100%", marginTop: 8 }}>
      <svg viewBox={`0 0 ${W} ${H}`} width="100%" height={H} className="stage-chart">
        {[0, 0.5, 1].map((f) => (
          <g key={f}>
            <line className="grid" x1={padL} y1={py(maxV * f)} x2={W - padR} y2={py(maxV * f)} />
            <text className="tick" x={padL - 6} y={py(maxV * f) + 3.5} textAnchor="end">
              {maxV * f >= 10 ? Math.round(maxV * f) : (maxV * f).toFixed(1)}
            </text>
          </g>
        ))}
        <polygon className="area" points={area} />
        <polyline className="line" points={line} />
        {points.map(([t, v], i) => (
          <circle key={i} className="dot" cx={px(t)} cy={py(v)} r={2.5} />
        ))}
        {[0, maxT].map((t) => (
          <text
            key={t}
            className="tick"
            x={px(t)}
            y={H - padB + 14}
            textAnchor={t === 0 ? "start" : "end"}
          >
            {t}s
          </text>
        ))}
        <text className="axis" x={(padL + W - padR) / 2} y={H - 2} textAnchor="middle">
          Elapsed (s)
        </text>
        <text className="axis" transform={`translate(11,${(padT + H - padB) / 2}) rotate(-90)`} textAnchor="middle">
          {yLabel}
        </text>
      </svg>
    </div>
  );
}

function StagesSection({
  scenario,
  onChange,
}: {
  scenario: LoadScenario;
  onChange: (s: LoadScenario) => void;
}) {
  const targetLabel = scenario.mode === "closed" ? "Virtual users" : "Arrivals / sec";
  const constantRateActive = scenario.arrivalRatePerSec != null;

  const setStages = (stages: StageItem[]) => onChange({ ...scenario, stages });

  // The chart shows what will actually run, so repeats are unrolled and
  // relative targets resolved before plotting.
  const points = useMemo(() => {
    let t = 0;
    const pts: [number, number][] = [[0, 0]];
    for (const s of flattenStages(scenario.stages)) {
      t += s.durationSec;
      pts.push([t, s.target]);
    }
    return pts;
  }, [scenario.stages]);

  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dragOver, setDragOver] = useState<number | null>(null);
  // Rows are only draggable while their handle is held, or dragging would
  // hijack text selection inside the number inputs.
  const [handleHeld, setHandleHeld] = useState<number | null>(null);

  useEffect(() => {
    if (handleHeld === null) return;
    const release = () => setHandleHeld(null);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    return () => {
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
    };
  }, [handleHeld]);

  // Row identities, parallel to the item list and moved in lockstep with it.
  const ids = useRef<string[]>([]);
  const nextId = useRef(0);
  while (ids.current.length < scenario.stages.length) {
    ids.current.push(`stage-${nextId.current++}`);
  }
  if (ids.current.length > scenario.stages.length) {
    ids.current.length = scenario.stages.length;
  }

  const setItemsWithIds = (items: StageItem[], nextIds: string[]) => {
    ids.current = nextIds;
    setStages(items);
  };

  const move = (from: number, to: number) => {
    if (from === to || to < 0 || to >= scenario.stages.length) return;
    const next = scenario.stages.slice();
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    const nextIds = ids.current.slice();
    const [movedId] = nextIds.splice(from, 1);
    nextIds.splice(to, 0, movedId);
    setItemsWithIds(next, nextIds);
  };

  const replaceItem = (i: number, item: StageItem) => {
    const next = scenario.stages.slice();
    next[i] = item;
    setStages(next);
  };

  const removeItem = (i: number) => {
    setItemsWithIds(
      scenario.stages.filter((_, j) => j !== i),
      ids.current.filter((_, j) => j !== i),
    );
  };

  const addItem = (item: StageItem) => {
    setItemsWithIds([...scenario.stages, item], [...ids.current, `stage-${nextId.current++}`]);
  };

  const endDrag = () => {
    setDragFrom(null);
    setDragOver(null);
    setHandleHeld(null);
  };

  const dragProps = (i: number) => ({
    draggable: handleHeld === i,
    onDragStart: (e: React.DragEvent) => {
      setDragFrom(i);
      e.dataTransfer.effectAllowed = "move";
      // Firefox refuses to start a drag without payload.
      e.dataTransfer.setData("text/plain", String(i));
    },
    onDragOver: (e: React.DragEvent) => {
      if (dragFrom === null) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      if (dragOver !== i) setDragOver(i);
    },
    onDrop: (e: React.DragEvent) => {
      e.preventDefault();
      if (dragFrom !== null) move(dragFrom, i);
      endDrag();
    },
    onDragEnd: endDrag,
    className:
      dragFrom === i
        ? "stage-row dragging"
        : dragOver === i && dragFrom !== null
          ? `stage-row drop-${dragFrom < i ? "after" : "before"}`
          : "stage-row",
  });

  const grip = (i: number) => (
    <td
      className="stage-grip"
      title="Drag to reorder, or focus and use the arrow keys"
      tabIndex={0}
      onPointerDown={() => setHandleHeld(i)}
      onPointerUp={() => setHandleHeld(null)}
      onBlur={() => setHandleHeld(null)}
      onKeyDown={(e) => {
        if (e.key === "ArrowUp") {
          e.preventDefault();
          move(i, i - 1);
        } else if (e.key === "ArrowDown") {
          e.preventDefault();
          move(i, i + 1);
        }
      }}
    >
      <Icons.Grip />
    </td>
  );

  const total = flattenStages(scenario.stages).reduce((n, s) => n + s.durationSec, 0);
  const peak = Math.max(0, ...flattenStages(scenario.stages).map((s) => s.target));

  return (
    <div className="col pad">
      <div className="row" style={{ fontWeight: 600 }}>Stages</div>
      <div className="hint">
        Each stage ramps linearly from the previous stage's target, starting at zero for the
        first stage. A zero-second stage skips the ramp and jumps straight to its target.
      </div>
      <div className="hint">
        A stage set to <strong>+</strong> adds to where the ramp has reached rather than
        setting it, which is what makes a repeated block climb: <em>+100 over 30s, then hold
        60s</em>, repeated, steps up 100 at a time.
      </div>
      {constantRateActive && (
        <div className="hint">
          A constant rate is set below, so stage targets are ignored for this run.
        </div>
      )}

      <StopWhenEditor scenario={scenario} onChange={onChange} />
      <div style={constantRateActive ? { opacity: 0.5, pointerEvents: "none" } : undefined}>
        <table className="kv-table stage-table">
          <thead>
            <tr>
              <th style={{ width: 28 }} />
              <th>Duration (s)</th>
              <th>{targetLabel}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {scenario.stages.map((item, i) =>
              isRepeat(item) ? (
                <tr key={ids.current[i]} {...dragProps(i)}>
                  {grip(i)}
                  <td colSpan={3}>
                    <RepeatEditor
                      block={item}
                      targetLabel={targetLabel}
                      onChange={(b) => replaceItem(i, b)}
                      onRemove={() => removeItem(i)}
                    />
                  </td>
                </tr>
              ) : (
                <tr key={ids.current[i]} {...dragProps(i)}>
                  {grip(i)}
                  <StageFields
                    stage={item}
                    onChange={(s) => replaceItem(i, s)}
                    onRemove={() => removeItem(i)}
                  />
                </tr>
              ),
            )}
          </tbody>
        </table>

        <div className="row" style={{ gap: 8, marginTop: 6 }}>
          <button
            className="btn sm"
            onClick={() => addItem({ durationSec: 30, target: 10 })}
          >
            Add stage
          </button>
          <button
            className="btn sm ghost"
            title="A block of stages run several times over"
            onClick={() =>
              addItem({
                times: 3,
                stages: [
                  { durationSec: 30, target: 100, relative: true },
                  { durationSec: 60, target: 0, relative: true },
                ],
              })
            }
          >
            <Icons.Plus /> Add repeat
          </button>
        </div>

        {scenario.stages.length > 0 && (
          <>
            <StagePreview points={points} yLabel={targetLabel} />
            <div className="hint">
              {flattenStages(scenario.stages).length} stages once unrolled ·{" "}
              {total}s total · peaks at {peak} {targetLabel.toLowerCase()}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

/**
 * When to end the run early.
 *
 * Deliberately separate from thresholds: a threshold decides whether a run
 * passed, while this decides when it has learned what it was for. Ramping up
 * to find a breaking point means reaching the break is the result, not a
 * failure — and carrying on past it only hammers something already struggling.
 */
function StopWhenEditor({
  scenario,
  onChange,
}: {
  scenario: LoadScenario;
  onChange: (s: LoadScenario) => void;
}) {
  const stop = scenario.stopWhen ?? null;
  const set = (next: StopCondition | null) => onChange({ ...scenario, stopWhen: next });

  if (!stop) {
    return (
      <button
        className="btn sm ghost"
        style={{ alignSelf: "flex-start" }}
        onClick={() =>
          set({ metric: "errorRate", above: 0.05, below: null, forIntervals: 2 })
        }
      >
        Stop early when…
      </button>
    );
  }

  // Error rate is a share; latency is milliseconds. Showing a raw 0.05 for
  // "5%" is the kind of thing that gets typed wrong.
  const isRate = stop.metric === "errorRate";
  // Rounded, because the share does not round-trip through floating point:
  // 7 -> 0.07 -> 7.000000000000001.
  const shown = isRate ? +((stop.above ?? 0) * 100).toFixed(6) : (stop.above ?? 0);

  return (
    <div className="col" style={{ gap: 6 }}>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <span style={{ fontWeight: 600 }}>Stop early when</span>
        <select
          className="select"
          style={{ width: 150 }}
          value={stop.metric}
          onChange={(e) => {
            const metric = e.target.value;
            // Sensible starting points per metric, since 5% and 500ms are not
            // interchangeable numbers. Both bounds are reset: the engine
            // fires on *either*, so a bound left over from the previous
            // metric would stop the run on a condition the UI does not show.
            if (metric === "rps") set({ ...stop, metric, above: null, below: 10 });
            else set({ ...stop, metric, above: metric === "errorRate" ? 0.05 : 1000, below: null });
          }}
        >
          <option value="errorRate">error rate</option>
          <option value="p95">p95 latency</option>
          <option value="p99">p99 latency</option>
          <option value="rps">throughput</option>
        </select>
        <span>{stop.metric === "rps" ? "falls below" : "goes above"}</span>
        <input
          className="input"
          type="number"
          min={0}
          style={{ width: 90 }}
          value={stop.metric === "rps" ? (stop.below ?? 0) : shown}
          onChange={(e) => {
            const v = Number(e.target.value) || 0;
            if (stop.metric === "rps") set({ ...stop, above: null, below: v });
            else set({ ...stop, below: null, above: isRate ? v / 100 : v });
          }}
        />
        <span className="hint">
          {isRate ? "%" : stop.metric === "rps" ? "req/s" : "ms"}
        </span>
        <span>for</span>
        <input
          className="input"
          type="number"
          min={1}
          style={{ width: 64 }}
          value={stop.forIntervals}
          onChange={(e) =>
            set({ ...stop, forIntervals: Math.max(1, Number(e.target.value) || 1) })
          }
        />
        <span className="hint">seconds running</span>
        <button className="btn icon sm danger" onClick={() => set(null)} title="Remove">
          ×
        </button>
      </div>
      <div className="hint">
        Measured over the last second, not the whole run, so a service that fails
        after a long clean stretch stops promptly. The run still passes or fails on
        its thresholds — stopping here just means it found what it was looking for.
      </div>
    </div>
  );
}

/** The duration, target and relative toggle shared by every stage row. */
function StageFields({
  stage,
  onChange,
  onRemove,
}: {
  stage: Stage;
  onChange: (s: Stage) => void;
  onRemove: () => void;
}) {
  return (
    <>
      <td>
        <input
          className="input"
          type="number"
          min={0}
          value={stage.durationSec}
          onChange={(e) => onChange({ ...stage, durationSec: Number(e.target.value) || 0 })}
        />
      </td>
      <td>
        <div className="row" style={{ gap: 4 }}>
          <button
            className="btn sm"
            style={{ width: 30, flex: "0 0 30px" }}
            title={
              stage.relative
                ? "Relative: added to where the ramp has reached"
                : "Absolute: the ramp goes to this value"
            }
            onClick={() => onChange({ ...stage, relative: !stage.relative })}
          >
            {stage.relative ? "+" : "="}
          </button>
          <input
            className="input"
            type="number"
            value={stage.target}
            onChange={(e) => onChange({ ...stage, target: Number(e.target.value) || 0 })}
          />
        </div>
      </td>
      <td className="row">
        <button className="btn icon sm danger" onClick={onRemove} title="Remove">
          ×
        </button>
      </td>
    </>
  );
}

/**
 * A block of stages run several times over.
 *
 * Nested inside the stage table rather than given its own screen, because the
 * whole point is to read the ramp top to bottom in one place.
 */
function RepeatEditor({
  block,
  targetLabel,
  onChange,
  onRemove,
}: {
  block: RepeatBlock;
  targetLabel: string;
  onChange: (b: RepeatBlock) => void;
  onRemove: () => void;
}) {
  const setStage = (i: number, s: Stage) => {
    const stages = block.stages.slice();
    stages[i] = s;
    onChange({ ...block, stages });
  };

  return (
    <div className="repeat-block">
      <div className="row" style={{ gap: 6 }}>
        <span style={{ fontWeight: 600 }}>Repeat</span>
        <input
          className="input"
          type="number"
          min={1}
          style={{ width: 64 }}
          value={block.times}
          onChange={(e) => onChange({ ...block, times: Math.max(1, Number(e.target.value) || 1) })}
        />
        <span>times</span>
        <span className="grow" />
        <span className="hint">each pass</span>
        <select
          className="select"
          style={{ width: 128 }}
          value={String(block.durationScale ?? 1)}
          onChange={(e) => {
            const v = Number(e.target.value);
            onChange({ ...block, durationScale: v === 1 ? null : v });
          }}
          title="Lengthen each repetition, to hold longer as the system nears its limit"
        >
          <option value="1">same length</option>
          <option value="1.25">25% longer</option>
          <option value="1.5">50% longer</option>
          <option value="2">twice as long</option>
          <option value="0.5">half as long</option>
        </select>
        <button className="btn icon sm danger" onClick={onRemove} title="Remove block">
          ×
        </button>
      </div>

      <table className="kv-table">
        <thead>
          <tr>
            <th>Duration (s)</th>
            <th>{targetLabel}</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {block.stages.map((s, i) => (
            <tr key={i}>
              <StageFields
                stage={s}
                onChange={(next) => setStage(i, next)}
                onRemove={() =>
                  onChange({ ...block, stages: block.stages.filter((_, j) => j !== i) })
                }
              />
            </tr>
          ))}
          {block.stages.length === 0 && (
            <tr>
              <td colSpan={3} className="hint">
                An empty block does nothing. Add a stage to it.
              </td>
            </tr>
          )}
        </tbody>
      </table>

      <button
        className="btn sm"
        style={{ alignSelf: "flex-start" }}
        onClick={() =>
          onChange({
            ...block,
            stages: [...block.stages, { durationSec: 30, target: 0, relative: true }],
          })
        }
      >
        Add stage to block
      </button>
    </div>
  );
}


// -- steps ------------------------------------------------------------------

function StepsSection({
  scenario,
  tree,
  onChange,
  pickerOpen,
  setPickerOpen,
}: {
  scenario: LoadScenario;
  tree: TreeNode[];
  onChange: (s: LoadScenario) => void;
  pickerOpen: boolean;
  setPickerOpen: (v: boolean) => void;
}) {
  const [expanded, setExpanded] = useState<Set<number>>(new Set());

  const setSteps = (steps: LoadStep[]) => onChange({ ...scenario, steps });

  const toggleExpanded = (i: number) => {
    const next = new Set(expanded);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    setExpanded(next);
  };

  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dragOver, setDragOver] = useState<number | null>(null);
  // Rows are draggable only while their grip is held, so dragging never
  // competes with selecting text in the inputs alongside it.
  const [handleHeld, setHandleHeld] = useState<number | null>(null);

  // Releasing the pointer anywhere disarms the row; without this, pressing a
  // grip and letting go elsewhere leaves the row draggable.
  useEffect(() => {
    if (handleHeld === null) return;
    const release = () => setHandleHeld(null);
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    return () => {
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
    };
  }, [handleHeld]);

  // Stable row identities, moved in lockstep with the steps. Keying by index
  // would make React reuse DOM by position, so an expanded captures table
  // would stay with the row number rather than the step that moved.
  const ids = useRef<string[]>([]);
  const nextId = useRef(0);
  while (ids.current.length < scenario.steps.length) {
    ids.current.push(`step-${nextId.current++}`);
  }
  if (ids.current.length > scenario.steps.length) {
    ids.current.length = scenario.steps.length;
  }

  const setStepsWithIds = (steps: LoadStep[], nextIds: string[]) => {
    ids.current = nextIds;
    setSteps(steps);
  };

  const moveStep = (from: number, to: number) => {
    if (from === to || to < 0 || to >= scenario.steps.length) return;
    const next = scenario.steps.slice();
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);

    const nextIds = ids.current.slice();
    const [movedId] = nextIds.splice(from, 1);
    nextIds.splice(to, 0, movedId);

    // Captures are keyed by row index, so they have to travel with the move
    // or the wrong step would appear expanded afterwards.
    const remapped = new Set<number>();
    for (const openIndex of expanded) {
      let target = openIndex;
      if (openIndex === from) target = to;
      else if (from < openIndex && openIndex <= to) target = openIndex - 1;
      else if (to <= openIndex && openIndex < from) target = openIndex + 1;
      remapped.add(target);
    }
    setExpanded(remapped);
    setStepsWithIds(next, nextIds);
  };

  const removeStep = (i: number) => {
    // Captures are keyed by row index; the rows after the removed one move
    // up, so their expanded state has to move with them (as in moveStep).
    const remapped = new Set<number>();
    for (const openIndex of expanded) {
      if (openIndex < i) remapped.add(openIndex);
      else if (openIndex > i) remapped.add(openIndex - 1);
    }
    setExpanded(remapped);
    setStepsWithIds(
      scenario.steps.filter((_, j) => j !== i),
      ids.current.filter((_, j) => j !== i),
    );
  };

  const endDrag = () => {
    setDragFrom(null);
    setDragOver(null);
    setHandleHeld(null);
  };

  return (
    <div className="col pad">
      <div className="row" style={{ fontWeight: 600 }}>Steps</div>
      <div className="col">
        {scenario.steps.map((step, i) => {
          // By ref first, then by id: a request that has been renamed or moved
          // is still this step's request, and saying "not found" for one that
          // is right there is worse than useless.
          const node =
            findNode(tree, step.requestRef) ??
            (step.requestId ? findNodeById(tree, step.requestId) : null);
          return (
            <div
              key={ids.current[i]}
              className={`col step-row ${
                dragFrom === i
                  ? "dragging"
                  : dragOver === i && dragFrom !== null
                    ? `drop-${dragFrom < i ? "after" : "before"}`
                    : ""
              }`}
              style={{ border: "1px solid var(--border)", borderRadius: "var(--radius)", padding: 8, gap: 6 }}
              draggable={handleHeld === i}
              onDragStart={(e) => {
                setDragFrom(i);
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", String(i));
              }}
              onDragOver={(e) => {
                if (dragFrom === null) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (dragOver !== i) setDragOver(i);
              }}
              onDrop={(e) => {
                e.preventDefault();
                if (dragFrom !== null) moveStep(dragFrom, i);
                endDrag();
              }}
              onDragEnd={endDrag}
            >
              <div className="row">
                <span
                  className="stage-grip"
                  title="Drag to reorder, or focus and use the arrow keys"
                  tabIndex={0}
                  onPointerDown={() => setHandleHeld(i)}
                  onPointerUp={() => setHandleHeld(null)}
                  onBlur={() => setHandleHeld(null)}
                  onKeyDown={(e) => {
                    if (e.key === "ArrowUp") {
                      e.preventDefault();
                      moveStep(i, i - 1);
                    } else if (e.key === "ArrowDown") {
                      e.preventDefault();
                      moveStep(i, i + 1);
                    }
                  }}
                >
                  <Icons.Grip />
                </span>
                <button
                  className={`btn icon sm ${step.parallel ? "primary" : "ghost"}`}
                  style={{ visibility: i === 0 ? "hidden" : undefined }}
                  title={
                    step.parallel
                      ? "Runs at the same time as the step above"
                      : "Runs after the step above — click to run them together"
                  }
                  onClick={() => {
                    const next = scenario.steps.slice();
                    next[i] = { ...step, parallel: !step.parallel };
                    setSteps(next);
                  }}
                >
                  {step.parallel ? "∥" : "→"}
                </button>
                <span className="nowrap muted">{i + 1}.</span>
                {node ? (
                  <>
                    {node.method && <MethodLabel method={node.method} />}
                    <span className="grow">{node.name}</span>
                  </>
                ) : (
                  <span className="status-pill err grow">{step.requestRef} — request not found</span>
                )}
                <input
                  className="input"
                  style={{ width: 120 }}
                  placeholder="Tag (optional)"
                  value={step.tag ?? ""}
                  onChange={(e) => {
                    const next = scenario.steps.slice();
                    next[i] = { ...step, tag: e.target.value || null };
                    setSteps(next);
                  }}
                />
                <input
                  className="input"
                  type="number"
                  min={0}
                  style={{ width: 80 }}
                  placeholder="Min ms"
                  value={step.thinkTimeMs?.[0] ?? ""}
                  onChange={(e) => {
                    const min = Number(e.target.value) || 0;
                    const max = step.thinkTimeMs?.[1] ?? min;
                    const next = scenario.steps.slice();
                    next[i] = { ...step, thinkTimeMs: [min, max] };
                    setSteps(next);
                  }}
                />
                <input
                  className="input"
                  type="number"
                  min={0}
                  style={{ width: 80 }}
                  placeholder="Max ms"
                  value={step.thinkTimeMs?.[1] ?? ""}
                  onChange={(e) => {
                    const max = Number(e.target.value) || 0;
                    const min = step.thinkTimeMs?.[0] ?? 0;
                    const next = scenario.steps.slice();
                    next[i] = { ...step, thinkTimeMs: [min, max] };
                    setSteps(next);
                  }}
                />
                <button className="btn sm" onClick={() => toggleExpanded(i)}>
                  Captures ({step.capture.length})
                </button>
                <button
                  className="btn icon sm danger"
                  onClick={() => removeStep(i)}
                  title="Remove"
                >
                  ×
                </button>
              </div>
              {expanded.has(i) && (
                <CapturesTable
                  captures={step.capture}
                  onChange={(capture) => {
                    const next = scenario.steps.slice();
                    next[i] = { ...step, capture };
                    setSteps(next);
                  }}
                />
              )}
            </div>
          );
        })}
      </div>
      <button className="btn sm" style={{ alignSelf: "flex-start" }} onClick={() => setPickerOpen(true)}>
        Add step
      </button>

      <GeneratorHelp />

      {pickerOpen && (
        <RequestPicker
          tree={tree}
          onPick={(ref) => {
            const picked = findNode(tree, ref);
            setStepsWithIds(
              [
                ...scenario.steps,
                { requestRef: ref, requestId: picked?.id ?? null, capture: [] },
              ],
              [...ids.current, `step-${nextId.current++}`],
            );
            setPickerOpen(false);
          }}
          onClose={() => setPickerOpen(false)}
        />
      )}
    </div>
  );
}

function CapturesTable({
  captures,
  onChange,
}: {
  captures: Capture[];
  onChange: (c: Capture[]) => void;
}) {
  return (
    <table className="kv-table">
      <thead>
        <tr>
          <th>From</th>
          <th>Path / header</th>
          <th>As</th>
          <th />
        </tr>
      </thead>
      <tbody>
        {captures.map((cap, i) => (
          <tr key={i}>
            <td>
              <select
                className="select"
                value={cap.from}
                onChange={(e) => {
                  const next = captures.slice();
                  next[i] = { ...cap, from: e.target.value as CaptureSource };
                  onChange(next);
                }}
              >
                <option value="body">Body (JSONPath)</option>
                <option value="header">Header</option>
              </select>
            </td>
            <td>
              {cap.from === "body" ? (
                <input
                  className="input"
                  placeholder="$.token"
                  value={cap.jsonPath ?? ""}
                  onChange={(e) => {
                    const next = captures.slice();
                    next[i] = { ...cap, jsonPath: e.target.value };
                    onChange(next);
                  }}
                />
              ) : (
                <input
                  className="input"
                  placeholder="X-Id"
                  value={cap.name ?? ""}
                  onChange={(e) => {
                    const next = captures.slice();
                    next[i] = { ...cap, name: e.target.value };
                    onChange(next);
                  }}
                />
              )}
            </td>
            <td>
              <input
                className="input"
                placeholder="variableName"
                value={cap.as}
                onChange={(e) => {
                  const next = captures.slice();
                  next[i] = { ...cap, as: e.target.value };
                  onChange(next);
                }}
              />
            </td>
            <td>
              <button className="btn icon sm danger" onClick={() => onChange(captures.filter((_, j) => j !== i))}>
                ×
              </button>
            </td>
          </tr>
        ))}
      </tbody>
      <tfoot>
        <tr>
          <td colSpan={4}>
            <button
              className="btn sm"
              onClick={() => onChange([...captures, { from: "body", jsonPath: "$.", as: "" }])}
            >
              Add capture
            </button>
          </td>
        </tr>
      </tfoot>
    </table>
  );
}

function RequestPicker({
  tree,
  onPick,
  onClose,
}: {
  tree: TreeNode[];
  onPick: (ref: string) => void;
  onClose: () => void;
}) {
  const requests: { node: TreeNode; depth: number }[] = [];
  walkTree(tree, (n, parents) => {
    if (n.kind === "request") requests.push({ node: n, depth: parents.length });
  });

  return (
    <Modal title="Add step" onClose={onClose}>
      <div className="col" style={{ maxHeight: 360, overflow: "auto" }}>
        {requests.length === 0 && <div className="empty">No requests in this workspace yet.</div>}
        {requests.map(({ node, depth }) => (
          <div
            key={node.nodeRef}
            className="tree-row"
            style={{ paddingLeft: depth * 14 + 4 }}
            onClick={() => onPick(node.nodeRef)}
          >
            {node.method && <MethodLabel method={node.method} />}
            <span className="tree-name">{node.name}</span>
          </div>
        ))}
      </div>
    </Modal>
  );
}

// -- thresholds ---------------------------------------------------------------

const METRICS = ["http_req_duration", "http_req_failed", "http_reqs", "checks"];
const STATS = ["p50", "p90", "p95", "p99", "avg", "max", "rate", "count"];
const OPS: ThresholdOp[] = ["<", "<=", ">", ">="];

function ThresholdsSection({
  scenario,
  onChange,
}: {
  scenario: LoadScenario;
  onChange: (s: LoadScenario) => void;
}) {
  const setThresholds = (thresholds: Threshold[]) => onChange({ ...scenario, thresholds });

  return (
    <div className="col pad">
      <div className="row" style={{ fontWeight: 600 }}>Thresholds</div>
      <table className="kv-table">
        <thead>
          <tr>
            <th>Metric</th>
            <th>Stat</th>
            <th>Op</th>
            <th>Value</th>
            <th>Abort on fail</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {scenario.thresholds.map((th, i) => {
            const unit = metricValueUnit(th.metric);
            return (
              <tr key={i}>
                <td>
                  <select
                    className="select"
                    value={th.metric}
                    onChange={(e) => {
                      const metric = e.target.value;
                      const nextUnit = metricValueUnit(metric);
                      const next = scenario.thresholds.slice();
                      next[i] = {
                        ...th,
                        metric,
                        valueMs: nextUnit === "ms" ? th.valueMs ?? th.value ?? 0 : null,
                        value: nextUnit === "value" ? th.value ?? th.valueMs ?? 0 : null,
                      };
                      setThresholds(next);
                    }}
                  >
                    {METRICS.map((m) => (
                      <option key={m} value={m}>
                        {m}
                      </option>
                    ))}
                  </select>
                </td>
                <td>
                  <select
                    className="select"
                    value={th.stat}
                    onChange={(e) => {
                      const next = scenario.thresholds.slice();
                      next[i] = { ...th, stat: e.target.value };
                      setThresholds(next);
                    }}
                  >
                    {STATS.map((s) => (
                      <option key={s} value={s}>
                        {s}
                      </option>
                    ))}
                  </select>
                </td>
                <td>
                  <select
                    className="select"
                    value={th.op}
                    onChange={(e) => {
                      const next = scenario.thresholds.slice();
                      next[i] = { ...th, op: e.target.value as ThresholdOp };
                      setThresholds(next);
                    }}
                  >
                    {OPS.map((op) => (
                      <option key={op} value={op}>
                        {op}
                      </option>
                    ))}
                  </select>
                </td>
                <td>
                  <input
                    className="input"
                    type="number"
                    value={unit === "ms" ? th.valueMs ?? 0 : th.value ?? 0}
                    onChange={(e) => {
                      const v = Number(e.target.value) || 0;
                      const next = scenario.thresholds.slice();
                      next[i] =
                        unit === "ms" ? { ...th, valueMs: v, value: null } : { ...th, value: v, valueMs: null };
                      setThresholds(next);
                    }}
                  />
                  <span className="hint">{unit === "ms" ? "ms" : ""}</span>
                </td>
                <td>
                  <input
                    className="checkbox"
                    type="checkbox"
                    checked={th.abortOnFail}
                    onChange={(e) => {
                      const next = scenario.thresholds.slice();
                      next[i] = { ...th, abortOnFail: e.target.checked };
                      setThresholds(next);
                    }}
                  />
                </td>
                <td>
                  <button
                    className="btn icon sm danger"
                    onClick={() => setThresholds(scenario.thresholds.filter((_, j) => j !== i))}
                  >
                    ×
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <button
        className="btn sm"
        style={{ alignSelf: "flex-start" }}
        onClick={() =>
          setThresholds([
            ...scenario.thresholds,
            { metric: "http_req_duration", stat: "p95", op: "<", valueMs: 500, value: null, abortOnFail: false },
          ])
        }
      >
        Add threshold
      </button>
    </div>
  );
}
