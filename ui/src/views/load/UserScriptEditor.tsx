import { useEffect, useState } from "react";
import * as api from "../../api";
import { Editor } from "../../components/Editor";
import { reportError, notify } from "../../stores/toast";

export function UserScriptEditor({
  nodeRef,
  onRun,
  onDirtyChange,
}: {
  nodeRef: string;
  onRun: () => void;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [text, setText] = useState("");
  const [original, setOriginal] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);

  const dirty = text !== original;

  useEffect(() => {
    onDirtyChange?.(dirty);
    return () => onDirtyChange?.(false);
  }, [dirty, onDirtyChange]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void (async () => {
      try {
        const t = await api.textGet(nodeRef);
        if (cancelled) return;
        setText(t);
        setOriginal(t);
      } catch (e) {
        reportError("Could not load this script", e);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeRef]);

  const save = async (): Promise<boolean> => {
    setSaving(true);
    try {
      await api.textSave(nodeRef, text);
      setOriginal(text);
      notify("Script saved");
      return true;
    } catch (e) {
      reportError("Could not save this script", e);
      return false;
    } finally {
      setSaving(false);
    }
  };

  // The run reads the script from disk, so unsaved edits would run the old
  // version and then be lost when the view moves to Runs.
  const runSaved = async () => {
    if (dirty && !(await save())) return;
    onRun();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text]);

  if (loading) {
    return (
      <div className="empty">
        <div className="spinner" />
      </div>
    );
  }

  return (
    <div className="col" style={{ height: "100%", minHeight: 0, gap: 0 }}>
      <div className="panel-header">
        <span className="grow">User script</span>
        {dirty && <span className="tab-dirty" title="Unsaved changes" />}
        <button className="btn sm" onClick={() => setHelpOpen((v) => !v)}>
          {helpOpen ? "Hide help" : "Show help"}
        </button>
        <button className="btn" onClick={() => void save()} disabled={!dirty || saving}>
          {saving ? "Saving…" : "Save"}
        </button>
        <button className="btn primary" onClick={() => void runSaved()} disabled={saving}>
          Run
        </button>
      </div>

      {helpOpen && (
        <div className="col pad hint" style={{ borderBottom: "1px solid var(--border)", gap: 6 }}>
          <div>
            <b>ctx.http</b>.get/post/put/patch/delete(url, {"{"}headers, params, json, body, tag{"}"}) — send a
            request from the virtual user.
          </div>
          <div>
            <b>ctx.check</b>(res, {"{"}name: r =&gt; bool{"}"}) — record a named pass/fail check against a response.
          </div>
          <div>
            <b>ctx.sleep</b>(minSec, maxSec?) — pause the virtual user, optionally for a random duration.
          </div>
          <div>
            <b>ctx.vars</b> — an object for per-virtual-user state that persists across iterations.
          </div>
          <div>
            <b>ctx.env</b>(key) — read a value from the selected environment.
          </div>
          <div>
            <b>ctx.vu.id</b> / <b>ctx.vu.iteration</b> — the current virtual user's id and iteration count.
          </div>
          <div>
            <b>ctx.group</b>(name, fn) — label a block of steps so their stats are tagged together.
          </div>
          <div>
            <b>options</b> — an exported object that configures <span className="mono">mode</span>,{" "}
            <span className="mono">stages</span>, <span className="mono">maxVus</span>,{" "}
            <span className="mono">userMix</span>, <span className="mono">thresholds</span>, and{" "}
            <span className="mono">environment</span> for this script.
          </div>
        </div>
      )}

      <div className="grow" style={{ minHeight: 0 }}>
        <Editor
          language="javascript"
          value={text}
          onChange={setText}
          className="flush"
          onSubmit={() => void save()}
        />
      </div>
    </div>
  );
}
