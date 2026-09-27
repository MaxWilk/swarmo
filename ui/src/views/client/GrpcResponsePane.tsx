import { useState } from "react";
import type { GrpcTab } from "../../stores/tabs";
import { Editor } from "../../components/Editor";
import { EmptyState, Icons, fmtBytes, fmtMs } from "../../components/ui";
import { grpcCodeName } from "../../api";

type GrpcRespSubTab = "response" | "metadata" | "tests" | "request";

/** The gRPC equivalent of StatusPill: code 0 is OK, anything else is a failure. */
export function GrpcStatusPill({ code }: { code: number }) {
  return (
    <span className={`status-pill ${code === 0 ? "ok" : "err"}`}>
      {code} {grpcCodeName(code)}
    </span>
  );
}

export function GrpcResponsePane({ tab }: { tab: GrpcTab }) {
  const [subTab, setSubTab] = useState<GrpcRespSubTab>("response");
  const [bodyView, setBodyView] = useState<"pretty" | "raw">("pretty");

  if (tab.sending) {
    return (
      <div className="empty">
        <div className="spinner" />
        <p>Calling…</p>
      </div>
    );
  }

  const result = tab.result;

  if (result?.error) {
    return (
      <EmptyState title="Call failed">
        <span className="selectable mono">{result.error}</span>
      </EmptyState>
    );
  }

  if (!result || !result.response) {
    return <EmptyState title="Call the method to see the response" />;
  }

  const response = result.response;
  const failed = response.code !== 0;

  return (
    <div className="col" style={{ flex: 1, minHeight: 0, gap: 0 }}>
      <div className="row pad" style={{ paddingBottom: 8, paddingTop: 8 }}>
        <GrpcStatusPill code={response.code} />
        <span className="faint">{fmtMs(response.durationMs)}</span>
        <span className="faint">{fmtBytes(response.responseBytes)}</span>
        {response.kind && response.kind !== "unary" && (
          // The figures a stream is judged by: how many arrived, and how long
          // the first took. The total duration also counts however long the
          // server chose to keep talking.
          <span className="faint nowrap" title={response.kind.replace("_", " ")}>
            {response.messageCount ?? 0} message{(response.messageCount ?? 0) === 1 ? "" : "s"}
            {response.firstMessageMs != null && ` · first after ${fmtMs(response.firstMessageMs)}`}
          </span>
        )}
        <span className="faint mono nowrap" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
          {result.sent.service}/{result.sent.method}
        </span>
      </div>

      {failed && (
        <div className="row" style={{ padding: "0 8px 8px" }}>
          <span className="selectable mono">{response.statusMessage}</span>
        </div>
      )}

      <div className="subtabs">
        <button className={`subtab ${subTab === "response" ? "active" : ""}`} onClick={() => setSubTab("response")}>
          Response
        </button>
        <button className={`subtab ${subTab === "metadata" ? "active" : ""}`} onClick={() => setSubTab("metadata")}>
          Metadata
          <span className="count-badge">{response.headers.length + response.trailers.length}</span>
        </button>
        <button className={`subtab ${subTab === "tests" ? "active" : ""}`} onClick={() => setSubTab("tests")}>
          Tests
          {result.tests.length > 0 && <span className="count-badge">{result.tests.length}</span>}
        </button>
        <button className={`subtab ${subTab === "request" ? "active" : ""}`} onClick={() => setSubTab("request")}>
          Request
        </button>
      </div>

      <div className="scroll pad">
        {subTab === "response" && (
          <ResponseTab bodyView={bodyView} setBodyView={setBodyView} failed={failed} response={response} />
        )}
        {subTab === "metadata" && (
          <table className="data-table text">
            <thead>
              <tr>
                <th>Kind</th>
                <th>Key</th>
                <th>Value</th>
              </tr>
            </thead>
            <tbody>
              {response.headers.map(([k, v], i) => (
                <tr key={`h${i}`}>
                  <td className="selectable">Header</td>
                  <td className="selectable mono">{k}</td>
                  <td className="selectable mono">{v}</td>
                </tr>
              ))}
              {response.trailers.map(([k, v], i) => (
                <tr key={`t${i}`}>
                  <td className="selectable">Trailer</td>
                  <td className="selectable mono">{k}</td>
                  <td className="selectable mono">{v}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {subTab === "tests" && (
          <div className="col">
            {result.scriptError && (
              <div className="status-pill err selectable mono" style={{ display: "block" }}>
                {result.scriptError}
              </div>
            )}
            {result.tests.length === 0 && !result.scriptError && (
              <EmptyState title="No tests. Add assertions in the Scripts tab." />
            )}
            {result.tests.map((t, i) => (
              <div key={i} className="row">
                <span style={{ color: t.passed ? "var(--ok)" : "var(--danger)" }}>
                  {t.passed ? <Icons.Check /> : <Icons.Alert />}
                </span>
                <span>{t.name}</span>
                {!t.passed && t.error && <span className="faint mono">{t.error}</span>}
              </div>
            ))}
          </div>
        )}
        {subTab === "request" && <RequestTab sent={result.sent} />}
      </div>
    </div>
  );
}

function ResponseTab({
  bodyView,
  setBodyView,
  failed,
  response,
}: {
  bodyView: "pretty" | "raw";
  setBodyView: (v: "pretty" | "raw") => void;
  failed: boolean;
  response: import("../../api").GrpcResult;
}) {
  if (failed) {
    return (
      <EmptyState title="No response message">
        <span className="selectable">
          A failed call carries no response message. Status: {response.statusMessage || grpcCodeName(response.code)}
        </span>
      </EmptyState>
    );
  }

  const text = bodyView === "raw" ? response.responseRawJson : response.responseJson;
  return (
    <div className="col" style={{ height: "100%", minHeight: 0 }}>
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <button className={`btn sm ${bodyView === "pretty" ? "primary" : ""}`} onClick={() => setBodyView("pretty")}>
          Pretty
        </button>
        <button className={`btn sm ${bodyView === "raw" ? "primary" : ""}`} onClick={() => setBodyView("raw")}>
          Raw
        </button>
        <button
          className="btn sm"
          onClick={() => {
            void navigator.clipboard.writeText(text);
          }}
        >
          Copy
        </button>
      </div>
      <Editor value={text} readOnly language="json" className="flush" />
    </div>
  );
}

function RequestTab({ sent }: { sent: import("../../api").SentGrpcRequest }) {
  return (
    <div className="col">
      <div className="hint">This reflects the call as sent: after scripts ran and variables were resolved.</div>
      <div className="row">
        <span className="mono selectable grow">{sent.address}</span>
        <span className="faint mono">{sent.service}/{sent.method}</span>
      </div>
      <table className="data-table text">
        <thead>
          <tr>
            <th>Key</th>
            <th>Value</th>
          </tr>
        </thead>
        <tbody>
          {sent.metadata.map(([k, v], i) => (
            <tr key={i}>
              <td className="selectable mono">{k}</td>
              <td className="selectable mono">{v}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <Editor value={sent.message} readOnly language="json" />
    </div>
  );
}
