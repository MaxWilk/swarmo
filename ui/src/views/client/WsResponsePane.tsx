import { useState } from "react";
import type { WsFrame, WsResult } from "../../api";
import type { WsTab } from "../../stores/tabs";
import { EmptyState, fmtBytes, fmtMs } from "../../components/ui";

/**
 * What the session did: the numbers, then the frames in order.
 *
 * The numbers come first because they are what a load test of this request
 * will report — connect time, per-message latency — and seeing them here is
 * how you learn what the load test is going to measure.
 */
export function WsResponsePane({ tab }: { tab: WsTab }) {
  const [view, setView] = useState<"transcript" | "exchanges" | "request">("transcript");
  const result = tab.result;

  if (tab.sending) {
    return (
      <EmptyState title="Running the session…">
        <span className="spinner" />
      </EmptyState>
    );
  }
  if (!result) {
    return (
      <EmptyState title="No session yet">
        <span>Run to connect, send the messages in order, and see what comes back.</span>
      </EmptyState>
    );
  }
  if (!result.result) {
    return (
      <EmptyState title="The session did not start">
        <span className="selectable">{result.error ?? "Unknown error"}</span>
      </EmptyState>
    );
  }

  const r = result.result;
  const ok = r.connected && !r.error && r.exchanges.every((e) => !e.timedOut);

  return (
    <div className="col" style={{ flex: 1, minHeight: 0, gap: 0 }}>
      <div className="row pad wrap" style={{ paddingBottom: 8, paddingTop: 8, gap: 10 }}>
        <span className={`status-pill ${ok ? "ok" : "err"}`}>
          {r.connected ? (ok ? "Session OK" : "Session failed") : "Not connected"}
        </span>
        {r.connectMs != null && <span className="faint">connect {fmtMs(r.connectMs)}</span>}
        <span className="faint">{fmtMs(r.durationMs)} total</span>
        <span className="faint">
          {r.messagesSent} sent · {r.messagesReceived} received
        </span>
        <span className="faint">
          {fmtBytes(r.bytesOut)} out · {fmtBytes(r.bytesIn)} in
        </span>
        {r.closeCode != null && (
          <span className="faint" title={r.closeReason}>
            closed {r.closeCode}
          </span>
        )}
        {r.subprotocol && <span className="faint mono">{r.subprotocol}</span>}
      </div>
      {r.error && (
        <div className="pad" style={{ paddingTop: 0 }}>
          <span className="status-pill err selectable">{r.error}</span>
        </div>
      )}

      <div className="subtabs">
        <button className={`subtab ${view === "transcript" ? "active" : ""}`} onClick={() => setView("transcript")}>
          Transcript <span className="count-badge">{r.frames.length}</span>
        </button>
        <button className={`subtab ${view === "exchanges" ? "active" : ""}`} onClick={() => setView("exchanges")}>
          Latency <span className="count-badge">{r.exchanges.length}</span>
        </button>
        <button className={`subtab ${view === "request" ? "active" : ""}`} onClick={() => setView("request")}>
          Request
        </button>
      </div>

      <div className="scroll">
        {view === "transcript" && <Transcript frames={r.frames} />}
        {view === "exchanges" && <Exchanges result={r} sentBodies={result.sent.messages.map((m) => m.body)} />}
        {view === "request" && (
          <div className="pad col selectable mono" style={{ fontSize: 12, gap: 6 }}>
            <div>{result.sent.url}</div>
            {result.sent.headers.map(([k, v], i) => (
              <div key={`${k}-${i}`}>
                {k}: {v}
              </div>
            ))}
            {result.sent.subprotocols.length > 0 && (
              <div>Sec-WebSocket-Protocol: {result.sent.subprotocols.join(", ")}</div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function Transcript({ frames }: { frames: WsFrame[] }) {
  if (frames.length === 0) {
    return <div className="hint" style={{ padding: 10 }}>No frames were exchanged.</div>;
  }
  return (
    <div className="col" style={{ gap: 0 }}>
      {frames.map((f, i) => (
        <div key={i} className={`ws-frame ${f.direction}`}>
          <span className="dir">{f.direction === "out" ? "→" : "←"}</span>
          <span className="at">{fmtMs(f.atMs)}</span>
          <span className="body selectable mono" title={`${f.kind}, ${fmtBytes(f.bytes)}`}>
            {f.body}
          </span>
        </div>
      ))}
    </div>
  );
}

function Exchanges({ result, sentBodies }: { result: WsResult; sentBodies: string[] }) {
  if (result.exchanges.length === 0) {
    return (
      <div className="hint" style={{ padding: 10 }}>
        No message waited for a reply, so there is nothing to time. Set a wait on a message to
        measure its round trip.
      </div>
    );
  }
  return (
    <div className="pad">
      <table className="data-table">
        <thead>
          <tr>
            <th>Message</th>
            <th>Latency</th>
            <th>Frames</th>
            <th>Out</th>
            <th>In</th>
            <th>Result</th>
          </tr>
        </thead>
        <tbody>
          {result.exchanges.map((e) => (
            <tr key={e.messageIndex}>
              <td className="mono" style={{ maxWidth: 260, overflow: "hidden", textOverflow: "ellipsis" }}>
                {sentBodies[e.messageIndex] ?? `#${e.messageIndex + 1}`}
              </td>
              <td>{e.latencyMs != null ? fmtMs(e.latencyMs) : "—"}</td>
              <td>{e.framesReceived}</td>
              <td>{fmtBytes(e.bytesOut)}</td>
              <td>{fmtBytes(e.bytesIn)}</td>
              <td>
                <span className={`status-pill ${e.timedOut ? "err" : "ok"}`}>
                  {e.timedOut ? "no reply" : "answered"}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
