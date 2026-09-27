import { useRef } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../../api";
import type {
  Body,
  BodyType,
  ConsoleLine,
  KeyValue,
  MultipartKind,
  MultipartPart,
  RequestDef,
} from "../../api";
import { type HttpTab, type SubTab, isDirty, useTabs } from "../../stores/tabs";
import { notify, reportError } from "../../stores/toast";
import { Editor, type EditorActions } from "../../components/Editor";
import { InsertVariable } from "../../components/InsertVariable";
import { KeyValueTable } from "../../components/KeyValueTable";
import { GeneratorHelp } from "../../components/GeneratorHelp";
import { Icons, Split } from "../../components/ui";
import { AuthEditor } from "../../components/AuthEditor";
import { extractQuery, mergeParams } from "../../lib/url";
import { ResponsePane } from "./ResponsePane";

const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

export function RequestEditor({ tab }: { tab: HttpTab }) {
  const patchTab = useTabs((s) => s.patch);
  const setSubTab = useTabs((s) => s.setSubTab);
  const save = useTabs((s) => s.save);
  const send = useTabs((s) => s.send);
  const cancel = useTabs((s) => s.cancel);
  const dirty = isDirty(tab);

  const patchDef = (p: Partial<RequestDef>) => patchTab(tab.nodeRef, p);

  const urlInput = useRef<HTMLInputElement | null>(null);

  // Copies the URL as the server would see it, not the template on screen:
  // the address bar holds `{{baseUrl}}/x` and a separate Params table, and
  // neither of those is something you can paste into a browser.
  const copyUrl = async () => {
    // Resolution reads the request from disk, like a send does, so unsaved
    // edits have to reach it first.
    if (dirty && !(await save(tab.nodeRef))) return;
    try {
      const { url, unresolved } = await api.requestResolveUrl(tab.nodeRef);
      await navigator.clipboard.writeText(url);
      if (unresolved.length) {
        notify(
          "URL copied — with unresolved variables",
          `No value for ${unresolved.map((v) => `{{${v}}}`).join(", ")}; left as written.`,
        );
      } else {
        notify("URL copied");
      }
    } catch (e) {
      reportError("Could not copy the URL", e);
    }
  };
  const insertIntoUrl = (token: string) => {
    const el = urlInput.current;
    const current = tab.def.url;
    const start = el?.selectionStart ?? current.length;
    const end = el?.selectionEnd ?? current.length;
    patchDef({ url: current.slice(0, start) + token + current.slice(end) });
    requestAnimationFrame(() => {
      const node = urlInput.current;
      if (!node) return;
      node.focus();
      const caret = start + token.length;
      node.setSelectionRange(caret, caret);
    });
  };

  const enabledParams = tab.def.params.filter((p) => p.enabled).length;
  const enabledHeaders = tab.def.headers.filter((h) => h.enabled).length;
  const testCount = tab.result?.tests.length ?? 0;

  return (
    <Split vertical storageKey="req-resp" initial={55}>
      <div className="col" style={{ flex: 1, minHeight: 0, gap: 0 }}>
        <div className="row" style={{ padding: 8 }}>
          <select
            className="select"
            style={{ width: 110, flex: "0 0 110px" }}
            value={tab.def.method}
            onChange={(e) => patchDef({ method: e.target.value })}
          >
            {METHODS.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
          <input
            ref={urlInput}
            className="input mono grow"
            placeholder="https://example.com/path?query=value"
            value={tab.def.url}
            onChange={(e) => patchDef({ url: e.target.value })}
            onPaste={(e) => {
              // A pasted link usually arrives complete, with its query on it.
              // Splitting that into the params table is the thing you would
              // otherwise do by hand, one row at a time.
              const pasted = e.clipboardData.getData("text");
              const input = e.currentTarget;
              const next =
                tab.def.url.slice(0, input.selectionStart ?? 0) +
                pasted +
                tab.def.url.slice(input.selectionEnd ?? 0);

              const found = extractQuery(next);
              if (!found) return;

              e.preventDefault();
              const params = mergeParams(tab.def.params, found.params);
              patchDef({ url: found.url, params });
              notify(
                `${found.params.length} query parameter${found.params.length === 1 ? "" : "s"} moved into Params`,
              );
            }}
            onKeyDown={(e) => {
              // Plain Enter only: Ctrl/Cmd+Enter is the window-level send,
              // and firing both would send twice.
              if (e.key === "Enter" && !e.ctrlKey && !e.metaKey) void send(tab.nodeRef);
            }}
            // No extraction on blur: clicking Save blurs this field first, so a
            // silent rewrite here changes what reaches disk *after* the user
            // last looked at it. A query typed into the URL stays in the URL —
            // sending merges it with the Params table correctly either way.
          />
          <InsertVariable compact onInsert={insertIntoUrl} title="Insert a generated value into the URL" />
          <button
            className="btn icon"
            title="Copy the full URL — variables filled in, query params included"
            onClick={() => void copyUrl()}
          >
            <Icons.Copy />
          </button>
          {tab.sending ? (
            <button className="btn danger" onClick={() => void cancel(tab.nodeRef)}>
              <span className="spinner" />
              Cancel
            </button>
          ) : (
            <button className="btn primary" onClick={() => void send(tab.nodeRef)}>
              Send
            </button>
          )}
          <button className="btn" disabled={!dirty} onClick={() => void save(tab.nodeRef)}>
            Save
          </button>
        </div>

        {!!tab.result?.unresolved.length && (
          <div className="row" style={{ padding: "0 8px 8px" }}>
            <span className="status-pill warn">
              Unresolved: {tab.result.unresolved.map((v) => `{{${v}}}`).join(", ")}
            </span>
          </div>
        )}

        <div className="subtabs">
          <SubtabButton tab={tab} id="params" label="Params" count={enabledParams} onClick={setSubTab} />
          <SubtabButton tab={tab} id="headers" label="Headers" count={enabledHeaders} onClick={setSubTab} />
          <SubtabButton tab={tab} id="auth" label="Auth" onClick={setSubTab} />
          <SubtabButton tab={tab} id="body" label="Body" onClick={setSubTab} />
          <SubtabButton tab={tab} id="scripts" label="Scripts" count={testCount || undefined} onClick={setSubTab} />
          <SubtabButton tab={tab} id="settings" label="Settings" onClick={setSubTab} />
        </div>

        <div className="scroll pad">
          {tab.subTab === "params" && (
            <KeyValueTable
              rows={tab.def.params}
              onChange={(rows) => patchDef({ params: rows })}
              keyPlaceholder="Param"
            />
          )}
          {tab.subTab === "headers" && (
            <KeyValueTable rows={tab.def.headers} onChange={(rows) => patchDef({ headers: rows })} keyPlaceholder="Header" />
          )}
          {tab.subTab === "auth" && <AuthEditor auth={tab.def.auth} onChange={(auth) => patchDef({ auth })} />}
          {tab.subTab === "body" && (
            <div className="col" style={{ height: "100%", minHeight: 0 }}>
              <div className="grow" style={{ minHeight: 0 }}>
                <BodyEditor body={tab.def.body} onChange={(body) => patchDef({ body })} />
              </div>
              <GeneratorHelp />
            </div>
          )}
          {tab.subTab === "scripts" && (
            <ScriptsEditor
              preRequest={tab.def.scripts.preRequest}
              postResponse={tab.def.scripts.postResponse}
              console={tab.result?.console ?? []}
              onChange={(scripts) => patchDef({ scripts })}
            />
          )}
          {tab.subTab === "settings" && (
            <SettingsEditor settings={tab.def.settings} onChange={(settings) => patchDef({ settings })} />
          )}
        </div>
      </div>

      <ResponsePane tab={tab} />
    </Split>
  );
}

function SubtabButton({
  tab,
  id,
  label,
  count,
  onClick,
}: {
  tab: HttpTab;
  id: SubTab;
  label: string;
  count?: number;
  onClick: (nodeRef: string, subTab: SubTab) => void;
}) {
  return (
    <button
      className={`subtab ${tab.subTab === id ? "active" : ""}`}
      onClick={() => onClick(tab.nodeRef, id)}
    >
      {label}
      {!!count && <span className="count-badge">{count}</span>}
    </button>
  );
}

// -- auth ---------------------------------------------------------------------

// -- body -----------------------------------------------------------------------

/**
 * An editor with an "Insert variable" button above it.
 *
 * The button sits outside the editor because a CodeMirror surface has no room
 * for chrome, and next to the label so it reads as belonging to that field
 * rather than to the panel.
 */
function EditorWithVariables({
  label,
  value,
  language,
  onChange,
}: {
  label?: string;
  value: string;
  language: "json" | "text" | "javascript";
  onChange: (text: string) => void;
}) {
  const actions = useRef<EditorActions | null>(null);
  return (
    <div className="col" style={{ flex: 1, minHeight: 0, gap: 4 }}>
      <div className="row" style={{ justifyContent: "space-between", alignItems: "flex-end" }}>
        {label ? <label className="editor-label">{label}</label> : <span />}
        <InsertVariable onInsert={(token) => actions.current?.insert(token)} />
      </div>
      <div className="grow" style={{ minHeight: 0 }}>
        <Editor value={value} language={language} onChange={onChange} actionsRef={actions} />
      </div>
    </div>
  );
}

function BodyEditor({ body, onChange }: { body: Body; onChange: (body: Body) => void }) {
  const type: BodyType = body.type;

  const setType = (t: BodyType) => {
    switch (t) {
      case "none":
        onChange({ type: "none" });
        break;
      case "json":
        onChange({ type: "json", text: "" });
        break;
      case "text":
        onChange({ type: "text", text: "", contentType: "text/plain" });
        break;
      case "form":
        onChange({ type: "form", fields: [] });
        break;
      case "multipart":
        onChange({ type: "multipart", parts: [] });
        break;
      case "graphql":
        onChange({ type: "graphql", query: "", variables: "" });
        break;
      case "binary":
        onChange({ type: "binary", path: "" });
        break;
    }
  };

  return (
    <div className="col" style={{ height: "100%", minHeight: 0 }}>
      <div className="field">
        <label>Body type</label>
        <select className="select" value={type} onChange={(e) => setType(e.target.value as BodyType)}>
          <option value="none">None</option>
          <option value="json">JSON</option>
          <option value="text">Text</option>
          <option value="form">Form (urlencoded)</option>
          <option value="multipart">Multipart form</option>
          <option value="graphql">GraphQL</option>
          <option value="binary">Binary file</option>
        </select>
      </div>

      {body.type === "json" && (
        <EditorWithVariables
          value={body.text}
          language="json"
          onChange={(text) => onChange({ ...body, text })}
        />
      )}

      {body.type === "text" && (
        <div className="col" style={{ flex: 1, minHeight: 0 }}>
          <div className="field">
            <label>Content type</label>
            <input
              className="input mono"
              value={body.contentType ?? ""}
              placeholder="text/plain"
              onChange={(e) => onChange({ ...body, contentType: e.target.value })}
            />
          </div>
          <EditorWithVariables
            value={body.text}
            language="text"
            onChange={(text) => onChange({ ...body, text })}
          />
        </div>
      )}

      {body.type === "form" && (
        <KeyValueTable
          rows={body.fields}
          onChange={(fields: KeyValue[]) => onChange({ ...body, fields })}
        />
      )}

      {body.type === "multipart" && <MultipartEditor parts={body.parts} onChange={(parts) => onChange({ ...body, parts })} />}

      {body.type === "graphql" && (
        <div className="col" style={{ flex: 1, minHeight: 0 }}>
          <EditorWithVariables
            label="Query"
            value={body.query}
            language="javascript"
            onChange={(query) => onChange({ ...body, query })}
          />
          <EditorWithVariables
            label="Variables"
            value={body.variables}
            language="json"
            onChange={(variables) => onChange({ ...body, variables })}
          />
        </div>
      )}

      {body.type === "binary" && (
        <div className="field">
          <label>File path</label>
          <div className="row">
            <input
              className="input mono grow"
              value={body.path}
              onChange={(e) => onChange({ ...body, path: e.target.value })}
            />
            <button
              className="btn"
              onClick={() => {
                open({ multiple: false })
                  .then((path) => {
                    if (typeof path === "string") onChange({ ...body, path });
                  })
                  .catch((e: unknown) => reportError("Could not choose the file", e));
              }}
            >
              Choose…
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function MultipartEditor({
  parts,
  onChange,
}: {
  parts: MultipartPart[];
  onChange: (parts: MultipartPart[]) => void;
}) {
  const display: MultipartPart[] = [...parts];
  const last = display[display.length - 1];
  if (!last || last.key || last.value) {
    display.push({ key: "", kind: "text", value: "", enabled: true });
  }

  const commit = (next: MultipartPart[]) => {
    while (next.length && !next[next.length - 1].key && !next[next.length - 1].value) {
      next.pop();
    }
    onChange(next);
  };

  const update = (i: number, patch: Partial<MultipartPart>) =>
    commit(display.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  const remove = (i: number) => commit(display.filter((_, idx) => idx !== i));

  return (
    <table className="kv-table">
      <thead>
        <tr>
          <th style={{ width: 30 }} />
          <th style={{ width: "26%" }}>Key</th>
          <th style={{ width: 80 }}>Kind</th>
          <th>Value</th>
          <th style={{ width: 30 }} />
        </tr>
      </thead>
      <tbody>
        {display.map((row, i) => {
          const isBlank = !row.key && !row.value;
          return (
            <tr key={i} className={row.enabled ? "" : "disabled"}>
              <td style={{ textAlign: "center" }}>
                {!isBlank && (
                  <input
                    type="checkbox"
                    className="checkbox"
                    checked={row.enabled}
                    onChange={(e) => update(i, { enabled: e.target.checked })}
                  />
                )}
              </td>
              <td>
                <input className="input mono" value={row.key} placeholder="Key" onChange={(e) => update(i, { key: e.target.value })} />
              </td>
              <td>
                <select
                  className="select"
                  value={row.kind}
                  onChange={(e) => update(i, { kind: e.target.value as MultipartKind })}
                >
                  <option value="text">Text</option>
                  <option value="file">File</option>
                </select>
              </td>
              <td>
                {row.kind === "file" ? (
                  <div className="row">
                    <span className="mono grow" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                      {row.value || "No file chosen"}
                    </span>
                    <button
                      className="btn sm"
                      onClick={() => {
                        open({ multiple: false })
                          .then((path) => {
                            if (typeof path === "string") update(i, { value: path });
                          })
                          .catch((e: unknown) => reportError("Could not choose the file", e));
                      }}
                    >
                      Choose…
                    </button>
                  </div>
                ) : (
                  <input className="input mono" value={row.value} placeholder="Value" onChange={(e) => update(i, { value: e.target.value })} />
                )}
              </td>
              <td>
                {!isBlank && (
                  <button className="btn ghost icon sm" onClick={() => remove(i)} title="Remove">
                    ×
                  </button>
                )}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

// -- scripts ---------------------------------------------------------------------

function ScriptsEditor({
  preRequest,
  postResponse,
  console: lines,
  onChange,
}: {
  preRequest: string;
  postResponse: string;
  console: ConsoleLine[];
  onChange: (scripts: { preRequest: string; postResponse: string }) => void;
}) {
  return (
    <div className="col" style={{ height: "100%", minHeight: 0 }}>
      <div className="hint">
        Available APIs: sw.env.get/set, sw.test, sw.expect, sw.response, sw.request,
        sw.sendRequest. Postman-style pm.* scripts also work.
      </div>
      <Split vertical initial={50}>
        <div className="col" style={{ flex: 1, minHeight: 0 }}>
          <div className="field">
            <label>Pre-request script</label>
          </div>
          <Editor
            value={preRequest}
            language="javascript"
            onChange={(text) => onChange({ preRequest: text, postResponse })}
          />
        </div>
        <div className="col" style={{ flex: 1, minHeight: 0 }}>
          <div className="field">
            <label>Post-response script</label>
          </div>
          <Editor
            value={postResponse}
            language="javascript"
            onChange={(text) => onChange({ preRequest, postResponse: text })}
          />
        </div>
      </Split>
      {lines.length > 0 && (
        <div className="col" style={{ flex: "0 0 auto", maxHeight: 140, overflow: "auto" }}>
          {lines.map((line, i) => (
            <div
              key={i}
              className="mono selectable"
              style={{
                color:
                  line.level === "error"
                    ? "var(--danger)"
                    : line.level === "warn"
                      ? "var(--warn)"
                      : "var(--text-muted)",
              }}
            >
              {line.text}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// -- settings ---------------------------------------------------------------------

function SettingsEditor({
  settings,
  onChange,
}: {
  settings: RequestDef["settings"];
  onChange: (settings: RequestDef["settings"]) => void;
}) {
  return (
    <div className="col">
      <label className="row">
        <input
          type="checkbox"
          className="checkbox"
          checked={settings.followRedirects}
          onChange={(e) => onChange({ ...settings, followRedirects: e.target.checked })}
        />
        Follow redirects
      </label>
      <div className="field">
        <label>Timeout (ms)</label>
        <input
          type="number"
          className="input"
          value={settings.timeoutMs}
          onChange={(e) => onChange({ ...settings, timeoutMs: Number(e.target.value) })}
        />
      </div>
      <label className="row">
        <input
          type="checkbox"
          className="checkbox"
          checked={settings.verifyTls}
          onChange={(e) => onChange({ ...settings, verifyTls: e.target.checked })}
        />
        Verify TLS certificates
      </label>
    </div>
  );
}
