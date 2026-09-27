import { useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import type { HttpTab } from "../../stores/tabs";
import { notify, reportError } from "../../stores/toast";
import { toCurl } from "../../lib/curl";
import { Editor, type EditorLanguage } from "../../components/Editor";
import { EmptyState, Icons, StatusPill, fmtBytes, fmtMs } from "../../components/ui";

type RespSubTab = "body" | "headers" | "tests" | "timing" | "request";

function mapLanguage(lang: string): EditorLanguage {
  switch (lang) {
    case "json":
      return "json";
    case "javascript":
    case "js":
      return "javascript";
    case "html":
      return "html";
    case "xml":
      return "xml";
    default:
      return "text";
  }
}

export function ResponsePane({ tab }: { tab: HttpTab }) {
  const [subTab, setSubTab] = useState<RespSubTab>("body");
  const [bodyView, setBodyView] = useState<"pretty" | "raw">("pretty");

  if (tab.sending) {
    return (
      <div className="empty">
        <div className="spinner" />
        <p>Sending…</p>
      </div>
    );
  }

  const result = tab.result;

  if (result?.error) {
    return (
      <EmptyState title="Request failed">
        <span className="selectable mono">{result.error}</span>
      </EmptyState>
    );
  }

  if (!result || !result.response) {
    return <EmptyState title="Send the request to see the response" />;
  }

  const response = result.response;

  return (
    <div className="col" style={{ flex: 1, minHeight: 0, gap: 0 }}>
      <div className="row pad" style={{ paddingBottom: 8, paddingTop: 8 }}>
        <StatusPill status={response.status} />
        <span className="muted">{response.statusText}</span>
        <span className="faint">{fmtMs(response.timings.totalMs)}</span>
        <span className="faint">{fmtBytes(response.bodySize)}</span>
        <span className="faint">{response.httpVersion}</span>
        <span className="faint mono nowrap" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
          {response.finalUrl}
        </span>
      </div>

      <div className="subtabs">
        <button className={`subtab ${subTab === "body" ? "active" : ""}`} onClick={() => setSubTab("body")}>
          Body
        </button>
        <button
          className={`subtab ${subTab === "headers" ? "active" : ""}`}
          onClick={() => setSubTab("headers")}
        >
          Headers
          <span className="count-badge">{response.headers.length}</span>
        </button>
        <button className={`subtab ${subTab === "tests" ? "active" : ""}`} onClick={() => setSubTab("tests")}>
          Tests
          {result.tests.length > 0 && <span className="count-badge">{result.tests.length}</span>}
        </button>
        <button
          className={`subtab ${subTab === "timing" ? "active" : ""}`}
          onClick={() => setSubTab("timing")}
        >
          Timing
        </button>
        <button
          className={`subtab ${subTab === "request" ? "active" : ""}`}
          onClick={() => setSubTab("request")}
        >
          Request
        </button>
      </div>

      <div className="scroll pad">
        {subTab === "body" && (
          <BodyTab bodyView={bodyView} setBodyView={setBodyView} body={response.body} />
        )}
        {subTab === "headers" && (
          <div className="col">
            <div className="row" style={{ justifyContent: "flex-end" }}>
              <button
                className="btn sm"
                title="Copy every header as text"
                onClick={() => {
                  void navigator.clipboard.writeText(
                    response.headers.map((h) => `${h.key}: ${h.value}`).join("\n"),
                  );
                  notify("Headers copied");
                }}
              >
                <Icons.Copy /> Copy
              </button>
            </div>
            <table className="data-table text">
              <thead>
                <tr>
                  <th>Key</th>
                  <th>Value</th>
                </tr>
              </thead>
              <tbody>
                {response.headers.map((h, i) => (
                  <tr key={i}>
                    <td className="selectable mono">{h.key}</td>
                    <td className="selectable mono">{h.value}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {subTab === "tests" && (
          <div className="col">
            {result.scriptError && (
              <div className="status-pill err block selectable mono">
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
        {subTab === "timing" && <TimingTab timings={response.timings} />}
        {subTab === "request" && <RequestTab sent={result.sent} />}
      </div>
    </div>
  );
}

function BodyTab({
  bodyView,
  setBodyView,
  body,
}: {
  bodyView: "pretty" | "raw";
  setBodyView: (v: "pretty" | "raw") => void;
  body: import("../../api").ExecResult["body"];
}) {
  if (body.kind === "empty") {
    return <div className="faint">No body</div>;
  }
  if (body.kind === "image") {
    return <img src={body.dataUrl} style={{ maxWidth: "100%" }} alt="Response body" />;
  }
  if (body.kind === "file") {
    return (
      <div className="col">
        <div className="mono selectable">{body.path}</div>
        <div className="faint">{fmtBytes(body.size)}</div>
        <button
          className="btn sm"
          style={{ alignSelf: "flex-start" }}
          onClick={() => {
            revealItemInDir(body.path).catch((e: unknown) =>
              reportError("Could not open the file location", e),
            );
          }}
        >
          Open
        </button>
      </div>
    );
  }
  if (body.kind === "binary") {
    return (
      <div className="col">
        <div className="faint">Binary body ({fmtBytes(body.size)}).</div>
      </div>
    );
  }

  const text = bodyView === "raw" ? body.raw : body.text;
  return (
    <div className="col" style={{ height: "100%", minHeight: 0 }}>
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <button
          className={`btn sm ${bodyView === "pretty" ? "primary" : ""}`}
          onClick={() => setBodyView("pretty")}
        >
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
      <Editor
        value={text}
        readOnly
        language={bodyView === "raw" ? "text" : mapLanguage(body.language)}
        className="flush"
      />
    </div>
  );
}

function TimingTab({ timings }: { timings: import("../../api").Timings }) {
  const total = Math.max(timings.totalMs, 1);
  const rows: [string, number][] = [
    ["TTFB", timings.ttfbMs],
    ["Download", timings.downloadMs],
    ["Total", timings.totalMs],
  ];
  return (
    <div className="col">
      {rows.map(([label, ms]) => (
        <div key={label} className="col" style={{ gap: 2 }}>
          <div className="row" style={{ justifyContent: "space-between" }}>
            <span className="muted">{label}</span>
            <span className="mono">{fmtMs(ms)}</span>
          </div>
          <div style={{ height: 6, background: "var(--bg-active)", borderRadius: 3 }}>
            <div
              style={{
                height: "100%",
                width: `${Math.min(100, (ms / total) * 100)}%`,
                background: "var(--accent)",
                borderRadius: 3,
              }}
            />
          </div>
        </div>
      ))}
    </div>
  );
}

function RequestTab({ sent }: { sent: import("../../api").SentRequest }) {
  const copyCurl = async () => {
    try {
      await navigator.clipboard.writeText(
        toCurl(sent.method, sent.url, sent.headers, sent.body),
      );
      notify("cURL command copied");
    } catch (e) {
      reportError("Could not copy the cURL command", e);
    }
  };

  return (
    <div className="col">
      <div className="row">
        <span className="hint grow">
          This reflects the request as sent: after scripts ran and variables were resolved.
        </span>
        <button
          className="btn sm ghost"
          title="Copy this exact request as a cURL command"
          onClick={() => void copyCurl()}
        >
          <Icons.Copy /> Copy as cURL
        </button>
      </div>
      <div className="row">
        <span className="mono">{sent.method}</span>
        <span className="mono selectable grow">{sent.url}</span>
      </div>
      <table className="data-table text">
        <thead>
          <tr>
            <th>Key</th>
            <th>Value</th>
          </tr>
        </thead>
        <tbody>
          {sent.headers.map(([k, v], i) => (
            <tr key={i}>
              <td className="selectable mono">{k}</td>
              <td className="selectable mono">{v}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {sent.body != null && <Editor value={sent.body} readOnly language="text" />}
    </div>
  );
}
