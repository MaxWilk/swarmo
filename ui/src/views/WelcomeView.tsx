import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import { Icons, Modal } from "../components/ui";
import { reportError } from "../stores/toast";
import { useWorkspace } from "../stores/workspace";

/**
 * Shown until a workspace is open. A workspace is just a directory of JSON
 * files, so the only decisions here are "which directory" and "what name".
 */
export function WelcomeView() {
  const { open: openWs, create, loading } = useWorkspace();
  const [recent, setRecent] = useState<string[]>([]);
  const [creating, setCreating] = useState<string | null>(null);
  const [newName, setNewName] = useState("");

  useEffect(() => {
    api.workspaceRecent().then(setRecent).catch(() => setRecent([]));
  }, []);

  const chooseExisting = async () => {
    try {
      const dir = await open({ directory: true, title: "Open a Swarmo workspace" });
      if (typeof dir === "string") await openWs(dir);
    } catch (e) {
      reportError("Could not open that folder", e);
    }
  };

  const chooseNew = async () => {
    try {
      const dir = await open({
        directory: true,
        title: "Choose a folder for the new workspace",
      });
      if (typeof dir !== "string") return;
      setCreating(dir);
      setNewName(dir.split(/[\\/]/).filter(Boolean).pop() ?? "My API workspace");
    } catch (e) {
      reportError("Could not choose that folder", e);
    }
  };

  const confirmCreate = async () => {
    if (!creating || !newName.trim()) return;
    const dir = creating;
    setCreating(null);
    try {
      await create(dir, newName.trim());
    } catch (e) {
      reportError("Could not create the workspace", e);
    }
  };

  const openRecent = async (path: string) => {
    try {
      await openWs(path);
    } catch (e) {
      reportError("Could not open that workspace", e);
    }
  };

  return (
    <div className="main">
      <div className="empty" style={{ gap: 20 }}>
        <div style={{ textAlign: "center" }}>
          <h2 style={{ fontSize: 22, color: "var(--text)" }}>Swarmo</h2>
          <p style={{ marginTop: 6 }}>
            An API client with a load-test engine built in. A workspace is a
            plain folder of JSON files, so you can commit it to git.
          </p>
        </div>

        <div className="row">
          <button className="btn primary" onClick={chooseNew} disabled={loading}>
            <Icons.Plus /> New workspace
          </button>
          <button className="btn" onClick={chooseExisting} disabled={loading}>
            <Icons.Folder /> Open workspace
          </button>
        </div>

        {loading && <div className="spinner" />}

        {recent.length > 0 && (
          <div className="col" style={{ width: "100%", maxWidth: 520, gap: 2 }}>
            <div className="stat-label" style={{ marginBottom: 4 }}>
              Recent
            </div>
            {recent.map((path) => (
              <button
                key={path}
                className="tree-row"
                style={{ width: "100%", textAlign: "left", border: "none", background: "transparent" }}
                onClick={() => openRecent(path)}
                title={path}
              >
                <Icons.Folder className="tree-caret" />
                <span className="tree-name mono faint">{path}</span>
              </button>
            ))}
          </div>
        )}
      </div>

      {creating && (
        <Modal
          title="Name this workspace"
          onClose={() => setCreating(null)}
          footer={
            <>
              <button className="btn" onClick={() => setCreating(null)}>
                Cancel
              </button>
              <button
                className="btn primary"
                onClick={confirmCreate}
                disabled={!newName.trim()}
              >
                Create
              </button>
            </>
          }
        >
          <div className="field">
            <label>Name</label>
            <input
              className="input"
              autoFocus
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void confirmCreate();
              }}
            />
          </div>
          <div className="hint mono selectable">{creating}</div>
          <div className="hint">
            Swarmo will create collections, environments and loadtests folders
            here, plus a .gitignore that keeps run results and secrets out of
            version control.
          </div>
        </Modal>
      )}
    </div>
  );
}
