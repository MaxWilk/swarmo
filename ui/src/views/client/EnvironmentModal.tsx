import { useEffect, useState } from "react";
import * as api from "../../api";
import type { EnvVariable, Environment } from "../../api";
import { useWorkspace } from "../../stores/workspace";
import { reportError } from "../../stores/toast";
import { ConfirmModal, Icons, Modal, PromptModal } from "../../components/ui";

export function EnvironmentModal({ onClose }: { onClose: () => void }) {
  const { environments } = useWorkspace();
  const [selected, setSelected] = useState<string | null>(environments[0] ?? null);
  const [env, setEnv] = useState<Environment | null>(null);
  const [loading, setLoading] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [showCreate, setShowCreate] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [pendingRename, setPendingRename] = useState<string | null>(null);

  useEffect(() => {
    if (!selected) {
      setEnv(null);
      return;
    }
    setLoading(true);
    api
      .envGet(selected)
      .then((e) => {
        setEnv(e);
        setDirty(false);
      })
      .catch((e: unknown) => reportError("Could not load the environment", e))
      .finally(() => setLoading(false));
  }, [selected]);

  const rows = env?.variables ?? [];
  const display: EnvVariable[] = [...rows];
  const last = display[display.length - 1];
  if (!last || last.key || last.value) {
    display.push({ key: "", value: "", secret: false, enabled: true });
  }

  function updateRow(i: number, patch: Partial<EnvVariable>) {
    if (!env) return;
    const next = display.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    while (next.length && !next[next.length - 1].key && !next[next.length - 1].value) {
      next.pop();
    }
    setEnv({ ...env, variables: next });
    setDirty(true);
  }

  function removeRow(i: number) {
    if (!env) return;
    const next = display.filter((_, idx) => idx !== i);
    setEnv({ ...env, variables: next });
    setDirty(true);
  }

  async function handleSave() {
    if (!env) return;
    try {
      // Drop the trailing blank row before saving.
      const variables = env.variables.filter((r) => r.key || r.value);
      const toSave: Environment = { ...env, variables };
      await api.envSave(toSave);
      await useWorkspace.getState().refreshEnvs();
      setEnv(toSave);
      setDirty(false);
    } catch (e) {
      reportError("Could not save the environment", e);
    }
  }

  // Same question as switching rows: anything that reloads or closes the
  // table would otherwise drop unsaved edits without a word.
  const confirmDiscard = () =>
    !dirty || window.confirm("Discard unsaved changes to this environment?");

  const close = () => {
    if (confirmDiscard()) onClose();
  };

  async function handleCreate(name: string) {
    setShowCreate(false);
    try {
      await api.envCreate(name);
      await useWorkspace.getState().refreshEnvs();
      // Created either way; only opening it would cost the edits.
      if (confirmDiscard()) setSelected(name);
    } catch (e) {
      reportError("Could not create the environment", e);
    }
  }

  async function handleRename(oldName: string, newName: string) {
    // Renaming the open environment reloads it under its new name.
    if (selected === oldName && !confirmDiscard()) return;
    try {
      await api.environmentRename(oldName, newName);
      await useWorkspace.getState().refreshEnvs();
      // Scenarios referencing the old name were updated too, so their editors
      // need to pick that up.
      await useWorkspace.getState().refreshTree();
      if (selected === oldName) setSelected(newName);
    } catch (e) {
      reportError("Could not rename the environment", e);
    }
  }

  async function handleDelete(name: string) {
    try {
      await api.envDelete(name);
      await useWorkspace.getState().refreshEnvs();
      if (selected === name) setSelected(null);
    } catch (e) {
      reportError("Could not delete the environment", e);
    }
  }

  return (
    <Modal
      title="Environments"
      onClose={close}
      wide
      footer={
        <>
          <button className="btn" onClick={close}>
            Close
          </button>
          <button className="btn primary" onClick={() => void handleSave()} disabled={!dirty || !env}>
            Save
          </button>
        </>
      }
    >
      <div className="row" style={{ alignItems: "stretch", minHeight: 320 }}>
        <div className="col" style={{ width: 180, flex: "0 0 180px", gap: 2 }}>
          <div className="row" style={{ justifyContent: "space-between" }}>
            <span className="muted">Environments</span>
            <button className="btn ghost icon sm" title="New environment" onClick={() => setShowCreate(true)}>
              <Icons.Plus />
            </button>
          </div>
          <div className="scroll">
            {environments.map((name) => (
              <div
                key={name}
                className={`tree-row ${selected === name ? "selected" : ""}`}
                onClick={() => {
                  if (name === selected) return;
                  // The load effect below resets the table, so unsaved
                  // edits would go without a word.
                  if (!confirmDiscard()) return;
                  setSelected(name);
                }}
              >
                <span className="tree-name">{name}</span>
                <button
                  className="btn ghost icon sm tree-actions"
                  style={{ display: "flex" }}
                  title="Rename environment"
                  onClick={(e) => {
                    e.stopPropagation();
                    setPendingRename(name);
                  }}
                >
                  <Icons.Edit />
                </button>
                <button
                  className="btn ghost icon sm tree-actions"
                  style={{ display: "flex" }}
                  title="Delete environment"
                  onClick={(e) => {
                    e.stopPropagation();
                    setPendingDelete(name);
                  }}
                >
                  <Icons.Trash />
                </button>
              </div>
            ))}
            {!environments.length && <div className="faint pad">No environments yet.</div>}
          </div>
        </div>

        <div className="col grow" style={{ gap: 6 }}>
          {loading ? (
            <div className="spinner" />
          ) : env ? (
            <>
              <table className="kv-table">
                <thead>
                  <tr>
                    <th style={{ width: 30 }} />
                    <th style={{ width: "28%" }}>Key</th>
                    <th>Value</th>
                    <th style={{ width: 60 }}>Secret</th>
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
                              onChange={(e) => updateRow(i, { enabled: e.target.checked })}
                            />
                          )}
                        </td>
                        <td>
                          <input
                            className="input mono"
                            value={row.key}
                            placeholder="Key"
                            onChange={(e) => updateRow(i, { key: e.target.value })}
                          />
                        </td>
                        <td>
                          <input
                            type={row.secret ? "password" : "text"}
                            className="input mono"
                            value={row.value}
                            placeholder="Value"
                            onChange={(e) => updateRow(i, { value: e.target.value })}
                          />
                        </td>
                        <td style={{ textAlign: "center" }}>
                          {!isBlank && (
                            <input
                              type="checkbox"
                              className="checkbox"
                              checked={row.secret}
                              onChange={(e) => updateRow(i, { secret: e.target.checked })}
                              title="Secret"
                            />
                          )}
                        </td>
                        <td>
                          {!isBlank && (
                            <button className="btn ghost icon sm" onClick={() => removeRow(i)} title="Remove">
                              <Icons.Close />
                            </button>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
              <div className="hint">
                Secret values are stored outside the committed files, in
                .swarmo/secrets.env.json.
              </div>
            </>
          ) : (
            <div className="empty">Select or create an environment.</div>
          )}
        </div>
      </div>

      {showCreate && (
        <PromptModal
          title="New environment"
          label="Name"
          onClose={() => setShowCreate(false)}
          onConfirm={(name) => void handleCreate(name)}
        />
      )}

      {pendingRename && (
        <PromptModal
          title="Rename environment"
          label="Name"
          initial={pendingRename}
          confirmLabel="Rename"
          onClose={() => setPendingRename(null)}
          onConfirm={(name) => void handleRename(pendingRename, name)}
        />
      )}

      {pendingDelete && (
        <ConfirmModal
          title={`Delete "${pendingDelete}"?`}
          message="This environment will be permanently deleted."
          onClose={() => setPendingDelete(null)}
          onConfirm={() => void handleDelete(pendingDelete)}
        />
      )}
    </Modal>
  );
}
