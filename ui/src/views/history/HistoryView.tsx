import { useEffect, useMemo, useState } from "react";
import * as api from "../../api";
import type { HistoryEntry } from "../../api";
import { describeStatus } from "../../api";
import { toCurl } from "../../lib/curl";
import {
  ConfirmModal,
  EmptyState,
  Icons,
  MethodLabel,
  clockTime,
  fmtBytes,
  fmtMs,
  groupByDay,
} from "../../components/ui";
import { Editor } from "../../components/Editor";
import { useTabs } from "../../stores/tabs";
import { notify, reportError } from "../../stores/toast";
import type { Section } from "../../App";

/**
 * Name the outcome of a send.
 *
 * A send that produced no response at all is its own case — there is no status
 * to name. Everything else defers to the shared status naming.
 */
function outcome(entry: HistoryEntry): { label: string; cls: string } {
  if (entry.error && entry.status === 0) return { label: "Failed", cls: "err" };
  return describeStatus({ protocol: entry.protocol, code: entry.status });
}

function Detail({
  entry,
  onNavigate,
  onDelete,
}: {
  entry: HistoryEntry;
  onNavigate: (s: Section) => void;
  onDelete: () => void;
}) {
  const open = useTabs((s) => s.open);
  const { label, cls } = outcome(entry);
  const isGrpc = entry.protocol === "grpc";
  const isWs = entry.protocol === "ws";

  // Where the request is now. It may have been renamed or moved since this
  // was sent, which the id sees through and the recorded path does not.
  const [currentRef, setCurrentRef] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    void api
      .requestLocate(entry.requestRef, entry.requestId)
      .then((found) => {
        if (!cancelled) setCurrentRef(found);
      })
      .catch(() => {
        if (!cancelled) setCurrentRef(null);
      });
    return () => {
      cancelled = true;
    };
  }, [entry.requestRef, entry.requestId]);
  const stillExists = currentRef != null;

  const openSource = async () => {
    if (!currentRef) return;
    await open(currentRef);
    onNavigate("client");
  };

  const copy = async (text: string, what: string) => {
    try {
      await navigator.clipboard.writeText(text);
      notify(`${what} copied`);
    } catch (e) {
      reportError(`Could not copy the ${what.toLowerCase()}`, e);
    }
  };

  return (
    <div className="scroll">
      <div className="panel-header">
        <MethodLabel method={entry.method} />
        <span className="grow">{entry.name}</span>
        <span className={`status-pill ${cls}`}>{label}</span>
        <span className="muted">{fmtMs(entry.durationMs)}</span>
      </div>

      <div className="col pad" style={{ gap: 10 }}>
        <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
          <button className="btn sm" onClick={() => void openSource()} disabled={!stillExists}>
            <Icons.Send /> Open request
          </button>
          <button className="btn sm ghost" onClick={() => void copy(entry.url, "URL")}>
            <Icons.Copy /> Copy URL
          </button>
          {entry.request.body && (
            <button
              className="btn sm ghost"
              onClick={() => void copy(entry.request.body ?? "", "Body")}
            >
              <Icons.Copy /> Copy body
            </button>
          )}
          {!isGrpc && !isWs && (
            <button
              className="btn sm ghost"
              title="Copy this exact request as a cURL command"
              onClick={() =>
                void copy(
                  toCurl(
                    entry.method,
                    entry.url,
                    entry.request.headers,
                    entry.request.body,
                    { bodyTruncated: entry.request.bodyTruncated },
                  ),
                  "cURL command",
                )
              }
            >
              <Icons.Copy /> Copy as cURL
            </button>
          )}
          <button className="btn sm ghost" onClick={onDelete}>
            <Icons.Trash /> Delete entry
          </button>
        </div>
        {!stillExists && (
          <div className="hint">
            The request this came from is no longer in the workspace, so it cannot be
            reopened. What was sent is still recorded below.
          </div>
        )}
      </div>

      <div className="col pad" style={{ gap: 6 }}>
        <div className="row" style={{ fontWeight: 600 }}>
          {isGrpc ? "Address and method" : "URL"}
        </div>
        <div className="mono selectable">
          {entry.url}
        </div>
        <div className="hint">
          Sent {new Date(entry.at).toLocaleString()} · {fmtMs(entry.durationMs)}
          {entry.responseBytes > 0 && ` · ${fmtBytes(entry.responseBytes)} received`}
        </div>
      </div>

      {entry.error && (
        <div className="col pad" style={{ gap: 6 }}>
          <div className="row" style={{ fontWeight: 600 }}>
            Error
          </div>
          <div className="status-pill err" style={{ alignSelf: "flex-start" }}>
            <Icons.Alert /> {entry.error}
          </div>
        </div>
      )}

      <div className="col pad" style={{ gap: 6 }}>
        <div className="row" style={{ fontWeight: 600 }}>
          {isGrpc ? "Metadata" : "Headers"}
        </div>
        {entry.request.headers.length === 0 ? (
          <div className="hint">None were sent.</div>
        ) : (
          <table className="data-table text">
            <thead>
              <tr>
                <th>Key</th>
                <th>Value</th>
              </tr>
            </thead>
            <tbody>
              {entry.request.headers.map(([k, v], i) => (
                <tr key={`${k}-${i}`}>
                  <td className="selectable mono">
                    {k}
                  </td>
                  <td className="selectable mono">
                    {v}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <div className="hint">
          These are the values that actually went out, after variables were resolved.
        </div>
      </div>

      <div className="col pad" style={{ gap: 6 }}>
        <div className="row" style={{ fontWeight: 600 }}>
          {isGrpc ? "Message" : isWs ? "Messages sent" : "Body"}
        </div>
        {entry.request.body ? (
          <>
            <Editor value={entry.request.body} readOnly language="text" />
            {entry.request.bodyTruncated && (
              <div className="hint">
                Only the first part of the body was kept, so history stays small.
              </div>
            )}
          </>
        ) : (
          <div className="hint">No body was sent.</div>
        )}
      </div>

      <div className="col pad" style={{ gap: 6 }}>
        <div className="row" style={{ fontWeight: 600, gap: 8 }}>
          Response
          <span className={`status-pill ${cls}`}>{label}</span>
        </div>
        {entry.response.headers.length > 0 && (
          <table className="data-table text">
            <thead>
              <tr>
                <th>Key</th>
                <th>Value</th>
              </tr>
            </thead>
            <tbody>
              {entry.response.headers.map(([k, v], i) => (
                <tr key={`${k}-${i}`}>
                  <td className="selectable mono">
                    {k}
                  </td>
                  <td className="selectable mono">
                    {v}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {entry.response.body ? (
          <>
            <Editor value={entry.response.body} readOnly language="text" />
            {entry.response.bodyTruncated && (
              <div className="hint">
                Only the first part of the response was kept, so history stays small.
              </div>
            )}
          </>
        ) : entry.responseBytes > 0 ? (
          <div className="hint">
            The response body was not captured — {fmtBytes(entry.responseBytes)} of
            binary or oversized content.
          </div>
        ) : entry.error ? (
          <div className="hint">The request never produced a response.</div>
        ) : (
          <div className="hint">The response had no body.</div>
        )}
      </div>
    </div>
  );
}

export function HistoryView({ onNavigate }: { onNavigate: (s: Section) => void }) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);

  const refresh = async () => {
    try {
      setEntries(await api.historyList());
    } catch (e) {
      reportError("Could not load request history", e);
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter(
      (e) =>
        e.name.toLowerCase().includes(q) ||
        e.url.toLowerCase().includes(q) ||
        e.method.toLowerCase().includes(q),
    );
  }, [entries, query]);

  // Entries arrive newest first, which is what groupByDay assumes.
  const groups = useMemo(() => groupByDay(filtered, (e) => e.at), [filtered]);

  const current = entries.find((e) => e.id === selected) ?? null;

  const clear = async () => {
    try {
      await api.historyClear();
      setSelected(null);
      await refresh();
      notify("History cleared");
    } catch (e) {
      reportError("Could not clear history", e);
    }
  };

  const remove = async (id: string) => {
    try {
      await api.historyDelete(id);
      if (selected === id) setSelected(null);
      await refresh();
    } catch (e) {
      reportError("Could not delete that entry", e);
    }
  };

  return (
    <>
      <div className="sidebar">
        <div className="panel-header">
          <span className="grow">History</span>
          <button
            className="btn icon sm ghost"
            title="Clear history"
            onClick={() => setConfirmClear(true)}
            disabled={entries.length === 0}
          >
            <Icons.Trash />
          </button>
        </div>
        <div className="pad" style={{ paddingBottom: 0 }}>
          <input
            className="input"
            placeholder="Filter by name, URL or method"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <div className="scroll">
          {entries.length === 0 && (
            <div className="hint" style={{ padding: 10 }}>
              Requests you send appear here.
            </div>
          )}
          {entries.length > 0 && filtered.length === 0 && (
            <div className="hint" style={{ padding: 10 }}>
              Nothing matches “{query}”.
            </div>
          )}
          {groups.map((group) => (
            <div key={group.day}>
              <div className="hint" style={{ padding: "8px 10px 2px", fontWeight: 600 }}>
                {group.day}
              </div>
              {group.items.map((entry) => {
                const { label, cls } = outcome(entry);
                return (
                  <div
                    key={entry.id}
                    className={`tree-row ${selected === entry.id ? "selected" : ""}`}
                    style={{ height: "auto", padding: "6px 6px 6px 10px", alignItems: "flex-start" }}
                    onClick={() => setSelected(entry.id)}
                  >
                    <div className="col grow" style={{ gap: 2, minWidth: 0 }}>
                      <div className="row" style={{ gap: 6 }}>
                        <MethodLabel method={entry.method} />
                        <span className="tree-name grow">{entry.name}</span>
                        <span className={`status-pill ${cls}`}>{label}</span>
                      </div>
                      <div className="hint">
                        {entry.url}
                      </div>
                      <div className="hint">
                        {clockTime(entry.at)} · {fmtMs(entry.durationMs)}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </div>

      <div className="main">
        {current ? (
          <Detail
            key={current.id}
            entry={current}
            onNavigate={onNavigate}
            onDelete={() => void remove(current.id)}
          />
        ) : (
          <EmptyState title="No request selected">
            Every request you send is recorded here with what was actually sent, so you
            can check later what went out and what came back.
          </EmptyState>
        )}
      </div>

      {confirmClear && (
        <ConfirmModal
          title="Clear history"
          message={`Delete all ${entries.length} recorded requests? This cannot be undone.`}
          onConfirm={() => void clear()}
          onClose={() => setConfirmClear(false)}
        />
      )}
    </>
  );
}
