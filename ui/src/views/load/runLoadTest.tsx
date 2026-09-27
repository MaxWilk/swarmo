import { useState } from "react";
import * as api from "../../api";
import type { Preflight } from "../../api";
import { Modal } from "../../components/ui";
import { useLoad } from "../../stores/load";
import { reportError } from "../../stores/toast";

/** Formats a duration in seconds as a short human string. */
function fmtDuration(sec: number): string {
  if (sec >= 3600) return `${(sec / 3600).toFixed(1)} h`;
  if (sec >= 60) return `${Math.round(sec / 60)} min`;
  return `${sec} s`;
}

/**
 * Shared "start a load test" flow: preflight -> confirm target hosts -> approve -> run.
 * `launch` returns nothing; render `dialog` alongside the caller's tree.
 */
export function useRunLauncher(onNavigateToRuns: () => void) {
  const [ref, setRef] = useState<string | null>(null);
  const [preflight, setPreflight] = useState<Preflight | null>(null);
  const [starting, setStarting] = useState(false);

  const launch = (nodeRef: string) => {
    void (async () => {
      try {
        const pf = await api.loadPreflight(nodeRef);
        setRef(nodeRef);
        setPreflight(pf);
      } catch (e) {
        reportError("Could not preflight this scenario", e);
      }
    })();
  };

  const close = () => {
    setRef(null);
    setPreflight(null);
    setStarting(false);
  };

  const start = () => {
    if (!ref || !preflight) return;
    void (async () => {
      setStarting(true);
      try {
        await api.loadApproveHosts(preflight.hosts);
        // Approving the run approves the commands it named, which the dialog
        // showed. Nothing is approved that was not on screen.
        for (const command of preflight.unapprovedAuthCommands) {
          await api.authCommandApprove(command);
        }
        const runId = await api.loadRun(ref);
        useLoad.getState().beginRun(runId, preflight.name);
        close();
        onNavigateToRuns();
      } catch (e) {
        reportError("Could not start the run", e);
        setStarting(false);
      }
    })();
  };

  const dialog = preflight ? (
    <Modal
      title="Confirm run"
      onClose={close}
      footer={
        <>
          <button className="btn" onClick={close}>
            Cancel
          </button>
          <button className="btn primary" onClick={start} disabled={starting}>
            {starting ? "Starting…" : "Start run"}
          </button>
        </>
      }
    >
      <div className="col">
        <div className="kv-table">
          <table className="kv-table">
            <tbody>
              <tr>
                <td className="muted">Scenario</td>
                <td>{preflight.name}</td>
              </tr>
              <tr>
                <td className="muted">Mode</td>
                <td>{preflight.mode === "closed" ? "Closed (virtual users)" : "Open (arrivals per second)"}</td>
              </tr>
              <tr>
                <td className="muted">Duration</td>
                <td>{fmtDuration(preflight.durationSec)}</td>
              </tr>
              <tr>
                <td className="muted">Peak target</td>
                <td>{preflight.peakTarget}</td>
              </tr>
              <tr>
                <td className="muted">Max virtual users</td>
                <td>{preflight.maxVus}</td>
              </tr>
            </tbody>
          </table>
        </div>

        <div className="sep" />

        <div className="field">
          <label>Target hosts</label>
          <div className="col" style={{ gap: 4 }}>
            {preflight.hosts.map((h) => (
              <div key={h} className="row">
                <span className="mono">{h}</span>
                {preflight.unapprovedHosts.includes(h) ? (
                  <span className="status-pill warn">Not yet approved</span>
                ) : (
                  <span className="status-pill ok">Approved</span>
                )}
              </div>
            ))}
          </div>
        </div>

        {preflight.authCommands.length > 0 && (
          <div className="field">
            <label>Commands this run will execute</label>
            <div className="col" style={{ gap: 4 }}>
              {preflight.authCommands.map((c) => (
                <div key={c} className="row">
                  <span className="mono grow" style={{ wordBreak: "break-all" }}>
                    {c}
                  </span>
                  {preflight.unapprovedAuthCommands.includes(c) ? (
                    <span className="status-pill warn">Not yet approved</span>
                  ) : (
                    <span className="status-pill ok">Approved</span>
                  )}
                </div>
              ))}
            </div>
            <div className="hint">
              These run on your machine with your permissions, to produce auth tokens.
              Starting the run allows them in this workspace.
            </div>
          </div>
        )}

        <div className="hint" style={{ color: "var(--danger)" }}>
          Only send load to systems you own or have permission to test.
        </div>
        {preflight.unapprovedHosts.length === 0 && (
          <div className="hint">All target hosts are already approved.</div>
        )}
      </div>
    </Modal>
  ) : null;

  return { launch, dialog };
}
