import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  grpcListServices,
  grpcMessageTemplate,
  type ConsoleLine,
  type GrpcRequestDef,
  type KeyValue,
  type ProtoSource,
  type ProtoSourceKind,
  type ServiceInfo,
} from "../../api";
import { type GrpcSubTab, type GrpcTab, isDirty, useTabs } from "../../stores/tabs";
import { notify, reportError } from "../../stores/toast";
import { Editor, type EditorActions } from "../../components/Editor";
import { InsertVariable } from "../../components/InsertVariable";
import { KeyValueTable } from "../../components/KeyValueTable";
import { GeneratorHelp } from "../../components/GeneratorHelp";
import { ConfirmModal, Icons, Split } from "../../components/ui";
import { AuthEditor } from "../../components/AuthEditor";
import { GrpcResponsePane } from "./GrpcResponsePane";

export function GrpcRequestEditor({ tab }: { tab: GrpcTab }) {
  const patchTab = useTabs((s) => s.patch);
  const setSubTab = useTabs((s) => s.setSubTab);
  const save = useTabs((s) => s.save);
  const send = useTabs((s) => s.send);
  const cancel = useTabs((s) => s.cancel);
  const dirty = isDirty(tab);

  const messageActions = useRef<EditorActions | null>(null);
  const [services, setServices] = useState<ServiceInfo[] | null>(null);
  const [schemaLoading, setSchemaLoading] = useState(false);
  const [schemaError, setSchemaError] = useState<string | null>(null);
  const [confirmSkeleton, setConfirmSkeleton] = useState(false);

  // Merged into the store's current def rather than built from this
  // render's `tab.def`: `applySkeleton` awaits a proto compile, and anything
  // typed meanwhile must survive the write-back.
  const patchDef = (p: Partial<GrpcRequestDef>) => patchTab(tab.nodeRef, p);

  // Bumped per load, so a slow reflection call that finishes after a newer
  // one cannot overwrite its result.
  const schemaSeq = useRef(0);

  const loadSchema = async (refresh: boolean): Promise<ServiceInfo[] | null> => {
    const seq = ++schemaSeq.current;
    setSchemaLoading(true);
    setSchemaError(null);
    try {
      // Pass the editor's current state, so switching the proto source or the
      // address applies straight away rather than after a save.
      const list = await grpcListServices(tab.nodeRef, tab.def, refresh);
      if (seq !== schemaSeq.current) return null;
      setServices(list);
      return list;
    } catch (e) {
      if (seq !== schemaSeq.current) return null;
      setSchemaError(e instanceof Error ? e.message : String(e));
      setServices(null);
      return null;
    } finally {
      if (seq === schemaSeq.current) setSchemaLoading(false);
    }
  };

  // Reload whenever the request changes or its schema source does. Debounced,
  // because the address and file paths are typed a character at a time and each
  // keystroke would otherwise start a compile or a reflection call.
  const schemaKey = `${tab.nodeRef}|${tab.def.address}|${JSON.stringify(tab.def.protoSource)}`;
  useEffect(() => {
    const t = setTimeout(() => void loadSchema(false), 500);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [schemaKey]);

  const enabledMetadata = tab.def.metadata.filter((m) => m.enabled).length;
  const testCount = tab.result?.tests.length ?? 0;
  const authActive = tab.def.auth.type !== "inherit" && tab.def.auth.type !== "none";

  const currentService = services?.find((s) => s.name === tab.def.service) ?? null;
  const currentMethod = currentService?.methods.find((m) => m.name === tab.def.method) ?? null;
  // The request side of a client or bidirectional stream is a list of
  // messages, which the editor has to say: a lone object is accepted as a
  // list of one, but nobody would guess that an array is allowed.
  const methodSendsStream = !!currentMethod?.clientStreaming;
  const methodReceivesStream = !!currentMethod?.serverStreaming;

  const applySkeleton = async () => {
    if (!tab.def.service || !tab.def.method) return;
    try {
      const message = await grpcMessageTemplate(
        tab.nodeRef,
        tab.def,
        tab.def.service,
        tab.def.method,
      );
      patchDef({ message });
    } catch (e) {
      reportError("Could not generate a message skeleton", e);
    }
  };

  const requestSkeleton = () => {
    const msg = tab.def.message.trim();
    if (msg && msg !== "{}") {
      setConfirmSkeleton(true);
    } else {
      void applySkeleton();
    }
  };

  const selectMethod = (serviceName: string, methodName: string) => patchDef({ service: serviceName, method: methodName });

  return (
    <>
      <Split vertical storageKey="req-resp" initial={55}>
        <div className="col" style={{ flex: 1, minHeight: 0, gap: 0 }}>
          <div className="row" style={{ padding: 8 }}>
            <input
              className="input mono grow"
              placeholder="http://localhost:50051"
              value={tab.def.address}
              onChange={(e) => patchDef({ address: e.target.value })}
            />
            <select
              className="select"
              style={{ width: 180 }}
              value={tab.def.service}
              onChange={(e) => {
                const svc = services?.find((s) => s.name === e.target.value);
                selectMethod(e.target.value, svc?.methods[0]?.name ?? "");
              }}
            >
              <option value="">Select service…</option>
              {services?.map((s) => (
                <option key={s.name} value={s.name}>
                  {s.name}
                </option>
              ))}
            </select>
            <select
              className="select"
              style={{ width: 180 }}
              value={tab.def.method}
              disabled={!currentService}
              onChange={(e) => selectMethod(tab.def.service, e.target.value)}
            >
              <option value="">Select method…</option>
              {currentService?.methods.map((m) => {
                // Labelled by shape, so a streaming method is picked knowingly.
                const kind =
                  m.clientStreaming && m.serverStreaming
                    ? " (bidi stream)"
                    : m.clientStreaming
                      ? " (client stream)"
                      : m.serverStreaming
                        ? " (server stream)"
                        : "";
                return (
                  <option key={m.name} value={m.name}>
                    {m.name}
                    {kind}
                  </option>
                );
              })}
            </select>
            {schemaLoading && <span className="spinner" />}
            {tab.sending ? (
              <button className="btn danger" onClick={() => void cancel(tab.nodeRef)}>
                <span className="spinner" />
                Cancel
              </button>
            ) : (
              <button
                className="btn primary"
                disabled={!tab.def.service || !tab.def.method}
                onClick={() => void send(tab.nodeRef)}
              >
                Call
              </button>
            )}
            <button className="btn" disabled={!dirty} onClick={() => void save(tab.nodeRef)}>
              Save
            </button>
          </div>

          {schemaError && (
            <div className="row" style={{ padding: "0 8px 8px" }}>
              <span className="status-pill err selectable mono">{schemaError}</span>
              <button className="btn sm" onClick={() => void loadSchema(true)}>
                Retry
              </button>
            </div>
          )}

          {!!tab.result?.unresolved.length && (
            <div className="row" style={{ padding: "0 8px 8px" }}>
              <span className="status-pill warn">
                Unresolved: {tab.result.unresolved.map((v) => `{{${v}}}`).join(", ")}
              </span>
            </div>
          )}

          <div className="subtabs">
            <SubtabButton tab={tab} id="message" label="Message" onClick={setSubTab} />
            <SubtabButton tab={tab} id="metadata" label="Metadata" count={enabledMetadata} onClick={setSubTab} />
            <SubtabButton tab={tab} id="auth" label="Auth" count={authActive ? 1 : undefined} onClick={setSubTab} />
            <SubtabButton tab={tab} id="proto" label="Proto" onClick={setSubTab} />
            <SubtabButton tab={tab} id="scripts" label="Scripts" count={testCount || undefined} onClick={setSubTab} />
            <SubtabButton tab={tab} id="settings" label="Settings" onClick={setSubTab} />
          </div>

          <div className="scroll pad">
            {tab.subTab === "message" && (
              <div className="col" style={{ height: "100%", minHeight: 0 }}>
                <div className="row" style={{ justifyContent: "flex-end" }}>
                  <InsertVariable onInsert={(token) => messageActions.current?.insert(token)} />
                  <button className="btn sm" disabled={!tab.def.service || !tab.def.method} onClick={requestSkeleton}>
                    Generate skeleton
                  </button>
                </div>
                {(methodSendsStream || methodReceivesStream) && (
                  <div className="hint">
                    {methodSendsStream && (
                      <>
                        This method takes a <strong>stream</strong> of messages: write a JSON array
                        and each element is sent in turn (a single object is sent as a list of one).{" "}
                      </>
                    )}
                    {methodReceivesStream && (
                      <>
                        The reply is a <strong>stream</strong>; every message received is shown as
                        one JSON array, and a load test measures the whole stream as one sample.
                        Set a message cap in Settings for an unbounded feed.
                      </>
                    )}
                  </div>
                )}
                <div className="grow" style={{ minHeight: 0 }}>
                  <Editor
                    value={tab.def.message}
                    language="json"
                    onChange={(message) => patchDef({ message })}
                    actionsRef={messageActions}
                  />
                </div>
                <GeneratorHelp />
              </div>
            )}
            {tab.subTab === "metadata" && (
              <div className="col">
                <div className="hint">
                  Metadata keys are lowercased. A key ending in "-bin" must have a base64-encoded
                  value.
                </div>
                <KeyValueTable
                  rows={tab.def.metadata}
                  onChange={(rows: KeyValue[]) => patchDef({ metadata: rows })}
                  keyPlaceholder="Metadata key"
                  valuePlaceholder="Value"
                />
              </div>
            )}
            {tab.subTab === "auth" && (
              <AuthEditor auth={tab.def.auth} onChange={(auth) => patchDef({ auth })} />
            )}
            {tab.subTab === "proto" && (
              <ProtoEditor
                source={tab.def.protoSource}
                onChange={(protoSource) => patchDef({ protoSource })}
                onRefresh={() => void loadSchema(true)}
                onFetch={async () => {
                  const list = await loadSchema(true);
                  if (list) notify("Schema fetched", `Found ${list.length} service(s).`);
                }}
                refreshing={schemaLoading}
              />
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

        <GrpcResponsePane tab={tab} />
      </Split>

      {confirmSkeleton && (
        <ConfirmModal
          title="Replace message?"
          message="This will replace the current message with a generated skeleton for the selected method."
          confirmLabel="Generate"
          danger={false}
          onClose={() => setConfirmSkeleton(false)}
          onConfirm={() => void applySkeleton()}
        />
      )}
    </>
  );
}

function SubtabButton({
  tab,
  id,
  label,
  count,
  onClick,
}: {
  tab: GrpcTab;
  id: GrpcSubTab;
  label: string;
  count?: number;
  onClick: (nodeRef: string, subTab: GrpcSubTab) => void;
}) {
  return (
    <button className={`subtab ${tab.subTab === id ? "active" : ""}`} onClick={() => onClick(tab.nodeRef, id)}>
      {label}
      {!!count && <span className="count-badge">{count}</span>}
    </button>
  );
}

// -- auth ---------------------------------------------------------------------

// -- proto source --------------------------------------------------------------

function ProtoEditor({
  source,
  onChange,
  onRefresh,
  onFetch,
  refreshing,
}: {
  source: ProtoSource;
  onChange: (source: ProtoSource) => void;
  onRefresh: () => void;
  onFetch: () => void;
  refreshing: boolean;
}) {
  const setKind = (kind: ProtoSourceKind) => {
    switch (kind) {
      case "directory":
        onChange({ kind: "directory", root: "", entryFiles: [] });
        break;
      case "files":
        onChange({ kind: "files", files: [], includePaths: [] });
        break;
      case "reflection":
        onChange({ kind: "reflection" });
        break;
    }
  };

  return (
    <div className="col">
      <div className="row">
        <label className="row">
          <input
            type="radio"
            name="proto-source"
            checked={source.kind === "directory"}
            onChange={() => setKind("directory")}
          />
          Proto folder (recommended)
        </label>
        <label className="row">
          <input
            type="radio"
            name="proto-source"
            checked={source.kind === "files"}
            onChange={() => setKind("files")}
          />
          Individual files
        </label>
        <label className="row">
          <input
            type="radio"
            name="proto-source"
            checked={source.kind === "reflection"}
            onChange={() => setKind("reflection")}
          />
          Server reflection
        </label>
      </div>

      {source.kind === "directory" && <DirectoryEditor source={source} onChange={onChange} />}
      {source.kind === "files" && <FilesEditor source={source} onChange={onChange} />}
      {source.kind === "reflection" && (
        <div className="col">
          <div className="hint">
            The server must have the gRPC reflection service enabled for this to work.
          </div>
          <button className="btn" disabled={refreshing} onClick={onFetch} style={{ alignSelf: "flex-start" }}>
            Fetch schema
          </button>
        </div>
      )}

      <button className="btn sm" disabled={refreshing} onClick={onRefresh} style={{ alignSelf: "flex-start" }}>
        Refresh schema
      </button>
    </div>
  );
}

function DirectoryEditor({
  source,
  onChange,
}: {
  source: Extract<ProtoSource, { kind: "directory" }>;
  onChange: (source: ProtoSource) => void;
}) {
  const [entriesExpanded, setEntriesExpanded] = useState(source.entryFiles.length > 0);

  const setRoot = (root: string) => onChange({ ...source, root });
  const setEntryFiles = (entryFiles: string[]) => onChange({ ...source, entryFiles });

  const chooseRoot = async () => {
    try {
      const picked = await open({ directory: true });
      if (!picked || Array.isArray(picked)) return;
      setRoot(picked);
    } catch (e) {
      reportError("Could not choose the folder", e);
    }
  };

  return (
    <div className="col">
      <div className="field">
        <label>Root folder</label>
        <div className="row">
          <input className="input mono grow" value={source.root} onChange={(e) => setRoot(e.target.value)} />
          <button className="btn sm" onClick={() => void chooseRoot()}>
            Choose folder…
          </button>
        </div>
        <div className="hint">
          Point at the folder your service&rsquo;s .proto files live in. Swarmo scans it, reads the
          import statements and works out the import roots itself, so it does not matter which
          level of the tree you pick. Dependencies in a neighbouring folder are found too.
          Relative paths are workspace-relative.
        </div>
      </div>

      <div className="field">
        <button
          className="btn ghost sm"
          style={{ alignSelf: "flex-start" }}
          onClick={() => setEntriesExpanded(!entriesExpanded)}
        >
          {entriesExpanded ? <Icons.ChevronDown /> : <Icons.Chevron />}
          Entry files
          {source.entryFiles.length > 0 && <span className="count-badge">{source.entryFiles.length}</span>}
        </button>

        {entriesExpanded && (
          <div className="col">
            <div className="hint">
              Leave empty to compile everything. Naming entry points is only needed for very
              large trees; their imports are still pulled in.
            </div>
            {source.entryFiles.map((f, i) => (
              <div key={i} className="row">
                <input
                  className="input mono grow"
                  value={f}
                  onChange={(e) =>
                    setEntryFiles(source.entryFiles.map((x, idx) => (idx === i ? e.target.value : x)))
                  }
                />
                <button
                  className="btn ghost icon sm"
                  title="Remove"
                  onClick={() => setEntryFiles(source.entryFiles.filter((_, idx) => idx !== i))}
                >
                  <Icons.Close />
                </button>
              </div>
            ))}
            <button
              className="btn sm"
              style={{ alignSelf: "flex-start" }}
              onClick={() => setEntryFiles([...source.entryFiles, ""])}
            >
              Add entry file
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function FilesEditor({
  source,
  onChange,
}: {
  source: Extract<ProtoSource, { kind: "files" }>;
  onChange: (source: ProtoSource) => void;
}) {
  const setFiles = (files: string[]) => onChange({ ...source, files });
  const setIncludePaths = (includePaths: string[]) => onChange({ ...source, includePaths });

  const chooseFiles = async () => {
    try {
      const picked = await open({
        filters: [{ name: "Protocol buffers", extensions: ["proto"] }],
        multiple: true,
      });
      if (!picked) return;
      const paths = Array.isArray(picked) ? picked : [picked];
      setFiles([...source.files, ...paths]);
    } catch (e) {
      reportError("Could not choose the .proto file(s)", e);
    }
  };

  const chooseDir = async () => {
    try {
      const picked = await open({ directory: true });
      if (!picked || Array.isArray(picked)) return;
      setIncludePaths([...source.includePaths, picked]);
    } catch (e) {
      reportError("Could not choose the directory", e);
    }
  };

  return (
    <div className="col">
      <div className="field">
        <label>.proto files</label>
        {source.files.map((f, i) => (
          <div key={i} className="row">
            <input
              className="input mono grow"
              value={f}
              onChange={(e) => setFiles(source.files.map((x, idx) => (idx === i ? e.target.value : x)))}
            />
            <button
              className="btn ghost icon sm"
              title="Remove"
              onClick={() => setFiles(source.files.filter((_, idx) => idx !== i))}
            >
              <Icons.Close />
            </button>
          </div>
        ))}
        <div className="row">
          <button className="btn sm" onClick={() => void chooseFiles()}>
            Choose…
          </button>
          <button className="btn sm" onClick={() => setFiles([...source.files, ""])}>
            Add file
          </button>
        </div>
      </div>

      <div className="field">
        <label>Import paths</label>
        <div className="hint">
          Import paths are the -I roots used to resolve "import" statements in your .proto files.
          Relative paths are workspace-relative.
        </div>
        {source.includePaths.map((p, i) => (
          <div key={i} className="row">
            <input
              className="input mono grow"
              value={p}
              onChange={(e) =>
                setIncludePaths(source.includePaths.map((x, idx) => (idx === i ? e.target.value : x)))
              }
            />
            <button
              className="btn ghost icon sm"
              title="Remove"
              onClick={() => setIncludePaths(source.includePaths.filter((_, idx) => idx !== i))}
            >
              <Icons.Close />
            </button>
          </div>
        ))}
        <div className="row">
          <button className="btn sm" onClick={() => void chooseDir()}>
            Choose directory…
          </button>
          <button className="btn sm" onClick={() => setIncludePaths([...source.includePaths, ""])}>
            Add path
          </button>
        </div>
      </div>
    </div>
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
        Scripts use the same sw / pm API as HTTP requests. sw.response.status is the gRPC status
        code, so sw.expect(sw.response.status).toBe(0) asserts success. sw.response.json() parses
        the response message. Response trailers appear in sw.response.headers prefixed with
        "trailer-".
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

const BYTES_PER_MB = 1048576;

function SettingsEditor({
  settings,
  onChange,
}: {
  settings: GrpcRequestDef["settings"];
  onChange: (settings: GrpcRequestDef["settings"]) => void;
}) {
  // The field keeps its own text while being edited: an empty or partial
  // entry ("" while clearing, "0" on the way to "0.5") is not a value to
  // commit, and committing it used to cap responses at one byte.
  const [mbDraft, setMbDraft] = useState<string | null>(null);
  const setMaxResponseMb = (raw: string) => {
    setMbDraft(raw);
    const mb = Number(raw);
    if (raw.trim() === "" || !Number.isFinite(mb) || mb <= 0) return;
    onChange({ ...settings, maxResponseBytes: Math.round(mb * BYTES_PER_MB) });
  };

  return (
    <div className="col">
      <div className="field">
        <label>Timeout (ms)</label>
        <input
          type="number"
          className="input"
          value={settings.timeoutMs}
          onChange={(e) => onChange({ ...settings, timeoutMs: Number(e.target.value) })}
        />
      </div>
      <div className="field">
        <label>Max response size (MB)</label>
        <input
          type="number"
          step="0.1"
          min="0"
          className="input"
          value={mbDraft ?? settings.maxResponseBytes / BYTES_PER_MB}
          onChange={(e) => setMaxResponseMb(e.target.value)}
          onBlur={() => setMbDraft(null)}
        />
        <div className="hint">
          gRPC libraries usually cap responses at 4 MB. Swarmo allows 16 MB by default; raise it
          if you get a "message too large" error, for example with large tensors or embeddings.
        </div>
      </div>
      <div className="field">
        <label>Stop a stream after (messages)</label>
        <input
          type="number"
          min="1"
          className="input"
          placeholder="until the server ends it"
          value={settings.streamMaxMessages ?? ""}
          onChange={(e) => {
            const n = Number(e.target.value);
            onChange({
              ...settings,
              streamMaxMessages:
                e.target.value.trim() === "" || !Number.isFinite(n) || n < 1 ? null : Math.floor(n),
            });
          }}
        />
        <div className="hint">
          Only for server- and bidirectional-streaming methods. An unbounded feed would otherwise
          run every call — and every load-test iteration — to its deadline.
        </div>
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
      <div className="hint">Verify TLS certificates only applies to https:// addresses.</div>
    </div>
  );
}
