import { useRef, useState } from "react";
import type { KeyValue, WsMessageDef, WsRequestDef, WsWait } from "../../api";
import { type WsSubTab, type WsTab, isDirty, useTabs } from "../../stores/tabs";
import { notify, reportError } from "../../stores/toast";
import { Editor, type EditorActions } from "../../components/Editor";
import { InsertVariable } from "../../components/InsertVariable";
import { KeyValueTable } from "../../components/KeyValueTable";
import { GeneratorHelp } from "../../components/GeneratorHelp";
import { AuthEditor } from "../../components/AuthEditor";
import { Icons, Split } from "../../components/ui";
import { WsResponsePane } from "./WsResponsePane";

/**
 * A WebSocket request is a session script: connect, then a list of messages
 * each with what to wait for afterwards. That list is what makes it a load
 * test as well as a debugging tool — the wait is where latency comes from.
 */
export function WsRequestEditor({ tab }: { tab: WsTab }) {
  const patchTab = useTabs((s) => s.patch);
  const setSubTab = useTabs((s) => s.setSubTab);
  const save = useTabs((s) => s.save);
  const send = useTabs((s) => s.send);
  const cancel = useTabs((s) => s.cancel);
  const dirty = isDirty(tab);

  const patchDef = (p: Partial<WsRequestDef>) => patchTab(tab.nodeRef, p);
  const urlInput = useRef<HTMLInputElement | null>(null);

  const insertIntoUrl = (token: string) => {
    const el = urlInput.current;
    const current = tab.def.url;
    const start = el?.selectionStart ?? current.length;
    const end = el?.selectionEnd ?? current.length;
    patchDef({ url: current.slice(0, start) + token + current.slice(end) });
  };

  const copyUrl = async () => {
    try {
      await navigator.clipboard.writeText(tab.def.url);
      notify("URL copied");
    } catch (e) {
      reportError("Could not copy the URL", e);
    }
  };

  const enabledHeaders = tab.def.headers.filter((h) => h.enabled).length;
  const enabledMessages = tab.def.messages.filter((m) => m.enabled).length;

  return (
    <Split vertical storageKey="ws-req-resp" initial={55}>
      <div className="col" style={{ flex: 1, minHeight: 0, gap: 0 }}>
        <div className="row" style={{ padding: 8 }}>
          <span className="method other" style={{ flex: "0 0 auto" }}>
            WS
          </span>
          <input
            ref={urlInput}
            className="input mono grow"
            placeholder="wss://example.com/live"
            value={tab.def.url}
            onChange={(e) => patchDef({ url: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.ctrlKey && !e.metaKey) void send(tab.nodeRef);
            }}
          />
          <InsertVariable compact onInsert={insertIntoUrl} title="Insert a generated value into the URL" />
          <button className="btn icon" title="Copy the URL" onClick={() => void copyUrl()}>
            <Icons.Copy />
          </button>
          {tab.sending ? (
            <button className="btn danger" onClick={() => void cancel(tab.nodeRef)}>
              <span className="spinner" />
              Cancel
            </button>
          ) : (
            <button className="btn primary" onClick={() => void send(tab.nodeRef)}>
              Run
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
          <Sub tab={tab} id="messages" label="Messages" count={enabledMessages} onClick={setSubTab} />
          <Sub tab={tab} id="headers" label="Headers" count={enabledHeaders} onClick={setSubTab} />
          <Sub tab={tab} id="auth" label="Auth" onClick={setSubTab} />
          <Sub tab={tab} id="settings" label="Settings" onClick={setSubTab} />
        </div>

        <div className="scroll pad">
          {tab.subTab === "messages" && (
            <MessagesEditor
              messages={tab.def.messages}
              onChange={(messages) => patchDef({ messages })}
            />
          )}
          {tab.subTab === "headers" && (
            <div className="col">
              <KeyValueTable
                rows={tab.def.headers}
                onChange={(rows: KeyValue[]) => patchDef({ headers: rows })}
                keyPlaceholder="Header"
              />
              <div className="field">
                <label>Subprotocols</label>
                <SubprotocolsInput
                  value={tab.def.subprotocols}
                  onChange={(subprotocols) => patchDef({ subprotocols })}
                />
                <div className="hint">
                  Offered as <span className="mono">Sec-WebSocket-Protocol</span>, in order.
                </div>
              </div>
            </div>
          )}
          {tab.subTab === "auth" && <AuthEditor auth={tab.def.auth} onChange={(auth) => patchDef({ auth })} />}
          {tab.subTab === "settings" && (
            <SettingsEditor settings={tab.def.settings} onChange={(settings) => patchDef({ settings })} />
          )}
        </div>
      </div>

      <WsResponsePane tab={tab} />
    </Split>
  );
}

/**
 * A comma-separated list, edited as text. The list is committed on every
 * keystroke, so a save shortcut never misses it; only the draft's text is
 * left alone until the user leaves the field, or normalising it would eat
 * the comma and space they just typed.
 */
function SubprotocolsInput({ value, onChange }: { value: string[]; onChange: (v: string[]) => void }) {
  const [draft, setDraft] = useState(value.join(", "));
  const parse = (text: string) =>
    text
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
  const change = (text: string) => {
    setDraft(text);
    const list = parse(text);
    if (list.join("\u0000") !== value.join("\u0000")) onChange(list);
  };
  return (
    <input
      className="input mono"
      placeholder="graphql-transport-ws, json"
      value={draft}
      onChange={(e) => change(e.target.value)}
      onBlur={() => setDraft(parse(draft).join(", "))}
      onKeyDown={(e) => {
        if (e.key === "Enter") (e.target as HTMLInputElement).blur();
      }}
    />
  );
}

function Sub({
  tab,
  id,
  label,
  count,
  onClick,
}: {
  tab: WsTab;
  id: WsSubTab;
  label: string;
  count?: number;
  onClick: (nodeRef: string, subTab: WsSubTab) => void;
}) {
  return (
    <button className={`subtab ${tab.subTab === id ? "active" : ""}`} onClick={() => onClick(tab.nodeRef, id)}>
      {label}
      {!!count && <span className="count-badge">{count}</span>}
    </button>
  );
}

// -- messages -------------------------------------------------------------------

function waitLabel(w: WsWait): string {
  switch (w.kind) {
    case "none":
      return "none";
    case "reply":
      return "reply";
    case "count":
      return "count";
    case "millis":
      return "millis";
  }
}

let nextRowId = 1;
const newRowId = () => `row-${nextRowId++}`;

function MessagesEditor({
  messages,
  onChange,
}: {
  messages: WsMessageDef[];
  onChange: (m: WsMessageDef[]) => void;
}) {
  // Messages have no id of their own, and each row owns an editor with undo
  // history and a cursor. Keyed by index, a move or remove would leave that
  // state attached to whatever message slid into the slot, so rows carry a
  // client-side id that moves with them.
  const rowIds = useRef<string[]>([]);
  while (rowIds.current.length < messages.length) rowIds.current.push(newRowId());
  if (rowIds.current.length > messages.length) rowIds.current.length = messages.length;

  const update = (i: number, p: Partial<WsMessageDef>) =>
    onChange(messages.map((m, j) => (j === i ? { ...m, ...p } : m)));
  const remove = (i: number) => {
    rowIds.current.splice(i, 1);
    onChange(messages.filter((_, j) => j !== i));
  };
  const move = (from: number, to: number) => {
    if (to < 0 || to >= messages.length) return;
    const next = messages.slice();
    const [m] = next.splice(from, 1);
    next.splice(to, 0, m);
    const [id] = rowIds.current.splice(from, 1);
    rowIds.current.splice(to, 0, id);
    onChange(next);
  };

  return (
    <div className="col" style={{ gap: 8 }}>
      <div className="hint">
        Sent in order. A message that <strong>waits</strong> is what gets a latency — send to
        first frame back. One that does not still counts in bytes. <em>Count</em> waits for that
        many frames; <em>Millis</em> collects whatever arrives for that long (for a feed that
        pushes on its own).
      </div>
      {messages.map((m, i) => (
        <div key={rowIds.current[i]} className="ws-message-row">
          <input
            type="checkbox"
            className="checkbox"
            checked={m.enabled}
            onChange={(e) => update(i, { enabled: e.target.checked })}
            title={m.enabled ? "Enabled" : "Disabled"}
          />
          <select
            className="select"
            value={m.kind}
            onChange={(e) => update(i, { kind: e.target.value as WsMessageDef["kind"] })}
          >
            <option value="text">Text</option>
            <option value="binary">Binary (base64)</option>
          </select>
          <MessageBody value={m.body} onChange={(body) => update(i, { body })} />
          <div className="col" style={{ gap: 4 }}>
            <select
              className="select"
              value={waitLabel(m.wait)}
              onChange={(e) => {
                const k = e.target.value;
                update(i, {
                  wait:
                    k === "reply"
                      ? { kind: "reply" }
                      : k === "count"
                        ? { kind: "count", count: 1 }
                        : k === "millis"
                          ? { kind: "millis", ms: 1000 }
                          : { kind: "none" },
                });
              }}
              title="What to wait for after sending"
            >
              <option value="none">Then continue</option>
              <option value="reply">Wait for a reply</option>
              <option value="count">Wait for N frames</option>
              <option value="millis">Collect for ms</option>
            </select>
            {m.wait.kind === "count" && (
              <NumberField
                value={m.wait.count}
                onCommit={(count) => update(i, { wait: { kind: "count", count } })}
              />
            )}
            {m.wait.kind === "millis" && (
              <NumberField value={m.wait.ms} onCommit={(ms) => update(i, { wait: { kind: "millis", ms } })} />
            )}
          </div>
          <div className="col" style={{ gap: 1 }}>
            <button className="btn ghost icon sm" title="Move up" disabled={i === 0} onClick={() => move(i, i - 1)}>
              ↑
            </button>
            <button
              className="btn ghost icon sm"
              title="Move down"
              disabled={i === messages.length - 1}
              onClick={() => move(i, i + 1)}
            >
              ↓
            </button>
            <button className="btn ghost icon sm" title="Remove" onClick={() => remove(i)}>
              <Icons.Close />
            </button>
          </div>
        </div>
      ))}
      <div className="row">
        <button
          className="btn sm"
          onClick={() =>
            onChange([...messages, { kind: "text", body: "{}", wait: { kind: "reply" }, enabled: true }])
          }
        >
          <Icons.Plus /> Add message
        </button>
      </div>
      <GeneratorHelp />
    </div>
  );
}

/**
 * A positive integer that can be cleared while retyping. Controlled and
 * clamped on every keystroke, backspacing to empty would snap straight back
 * to 1; here the field only reports a value once there is one.
 */
function NumberField({ value, onCommit }: { value: number; onCommit: (n: number) => void }) {
  return (
    <input
      className="input"
      type="number"
      min={1}
      defaultValue={value}
      onChange={(e) => {
        const n = Math.floor(Number(e.target.value));
        if (e.target.value !== "" && Number.isFinite(n) && n >= 1) onCommit(n);
      }}
      onBlur={(e) => {
        if (e.target.value === "" || Number(e.target.value) < 1) e.target.value = String(value);
      }}
    />
  );
}

/** A small editor per message, with the variable picker beside it. */
function MessageBody({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const actions = useRef<EditorActions | null>(null);
  return (
    <div className="col" style={{ gap: 4, minHeight: 0 }}>
      <div className="row" style={{ justifyContent: "flex-end" }}>
        <InsertVariable compact onInsert={(t) => actions.current?.insert(t)} />
      </div>
      <div style={{ height: 96 }}>
        <Editor value={value} language="json" showLineNumbers={false} onChange={onChange} actionsRef={actions} />
      </div>
    </div>
  );
}

// -- settings -------------------------------------------------------------------

function SettingsEditor({
  settings,
  onChange,
}: {
  settings: WsRequestDef["settings"];
  onChange: (s: WsRequestDef["settings"]) => void;
}) {
  return (
    <div className="col">
      <div className="field">
        <label>Connect timeout (ms)</label>
        <input
          type="number"
          className="input"
          value={settings.connectTimeoutMs}
          onChange={(e) => onChange({ ...settings, connectTimeoutMs: Number(e.target.value) || 0 })}
        />
      </div>
      <div className="field">
        <label>Wait timeout (ms)</label>
        <input
          type="number"
          className="input"
          value={settings.timeoutMs}
          onChange={(e) => onChange({ ...settings, timeoutMs: Number(e.target.value) || 0 })}
        />
        <div className="hint">
          How long one wait may take. A wait that runs out fails that message — not the whole
          session — so a slow reply reads as a slow reply.
        </div>
      </div>
      <label className="row">
        <input
          type="checkbox"
          className="checkbox"
          checked={settings.closeAfter}
          onChange={(e) => onChange({ ...settings, closeAfter: e.target.checked })}
        />
        Close cleanly after the last message
      </label>
      <label className="row">
        <input
          type="checkbox"
          className="checkbox"
          checked={settings.verifyTls}
          onChange={(e) => onChange({ ...settings, verifyTls: e.target.checked })}
        />
        Verify TLS certificates
      </label>
      <div className="hint">Verify TLS certificates only applies to wss:// URLs.</div>
      <div className="hint">
        Scripts do not run for a WebSocket session: there is no single response for a
        post-response script to see.
      </div>
    </div>
  );
}
