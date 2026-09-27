import { useEffect, useState } from "react";
import * as api from "../api";
import type { CookieRecord, Settings } from "../api";
import { Icons, fmtTime } from "../components/ui";
import { notify, reportError } from "../stores/toast";
import { isDirty, useTabs } from "../stores/tabs";
import { useWorkspace } from "../stores/workspace";

export function SettingsView() {
  const { settings, saveSettings, info, close } = useWorkspace();
  const [draft, setDraft] = useState<Settings>(settings);
  const [cookies, setCookies] = useState<CookieRecord[]>([]);

  useEffect(() => setDraft(settings), [settings]);

  const refreshCookies = () => {
    api.cookiesList().then(setCookies).catch(() => setCookies([]));
  };
  useEffect(refreshCookies, []);

  const dirty = JSON.stringify(draft) !== JSON.stringify(settings);

  const patch = (p: Partial<Settings>) => setDraft({ ...draft, ...p });

  return (
    <div className="main">
      <div className="panel-header">Settings</div>
      <div className="scroll pad">
        <div className="col" style={{ maxWidth: 620, gap: 22 }}>
          <section className="col">
            <div className="stat-label">Appearance</div>
            <div className="field">
              <label>Theme</label>
              <select
                className="select"
                value={draft.theme}
                onChange={(e) => patch({ theme: e.target.value as Settings["theme"] })}
              >
                <option value="system">Match the system</option>
                <option value="light">Light</option>
                <option value="dark">Dark</option>
              </select>
            </div>
          </section>

          <section className="col">
            <div className="stat-label">Network</div>
            <div className="field">
              <label>HTTP proxy</label>
              <input
                className="input mono"
                placeholder="http://127.0.0.1:8080"
                value={draft.proxy ?? ""}
                onChange={(e) => patch({ proxy: e.target.value || null })}
              />
              <div className="hint">
                Applied to every request the client sends. Load runs ignore it,
                so the proxy never becomes the bottleneck under load.
              </div>
            </div>
            <div className="field">
              <label>Default timeout (ms)</label>
              <input
                className="input"
                type="number"
                min={0}
                value={draft.defaultTimeoutMs}
                onChange={(e) =>
                  patch({ defaultTimeoutMs: Number(e.target.value) || 0 })
                }
              />
              <div className="hint">
                Used for new requests. Existing requests keep their own setting.
              </div>
            </div>
            <label className="row" style={{ cursor: "pointer" }}>
              <input
                type="checkbox"
                className="checkbox"
                checked={draft.defaultVerifyTls}
                onChange={(e) => patch({ defaultVerifyTls: e.target.checked })}
              />
              <span>Verify TLS certificates by default</span>
            </label>
          </section>

          <div className="row">
            <button
              className="btn primary"
              disabled={!dirty}
              onClick={() => saveSettings(draft)}
            >
              Save settings
            </button>
            {dirty && (
              <button className="btn ghost" onClick={() => setDraft(settings)}>
                Discard
              </button>
            )}
          </div>

          <div className="sep" />

          <section className="col">
            <div className="stat-label">Workspace</div>
            {info ? (
              <>
                <div className="row">
                  <span className="mono faint selectable grow">{info.path}</span>
                </div>
                <div className="row wrap">
                  <button
                    className="btn"
                    onClick={async () => {
                      try {
                        await api.approvedHostsClear();
                        notify("Host approvals cleared");
                      } catch (e) {
                        reportError("Could not clear host approvals", e);
                      }
                    }}
                  >
                    Reset load-test host approvals
                  </button>
                  <button
                    className="btn"
                    onClick={async () => {
                      try {
                        await api.runtimeVarsClear();
                        notify("Script variables cleared");
                      } catch (e) {
                        reportError("Could not clear script variables", e);
                      }
                    }}
                  >
                    Clear script-set variables
                  </button>
                  <button
                    className="btn danger"
                    onClick={() => {
                      // Closing the workspace closes every tab; unsaved edits
                      // get the same warning a single tab close gives.
                      const dirty = useTabs.getState().tabs.filter(isDirty);
                      if (
                        dirty.length > 0 &&
                        !window.confirm(
                          `${dirty.length} open tab(s) have unsaved changes. Close the workspace and discard them?`,
                        )
                      )
                        return;
                      void close();
                    }}
                  >
                    Close workspace
                  </button>
                </div>
                <div className="hint">
                  Approved hosts are the ones you have confirmed are safe to send
                  load to. Clearing them makes Swarmo ask again before the next run.
                </div>
              </>
            ) : (
              <div className="faint">No workspace is open.</div>
            )}
          </section>

          <div className="sep" />

          <section className="col">
            <div className="row">
              <div className="stat-label grow">Cookies</div>
              <button className="btn sm ghost" onClick={refreshCookies} title="Refresh">
                <Icons.History />
              </button>
              <button
                className="btn sm"
                onClick={async () => {
                  try {
                    await api.cookiesClear();
                    setCookies([]);
                    notify("Cookies cleared");
                  } catch (e) {
                    reportError("Could not clear cookies", e);
                  }
                }}
              >
                Clear
              </button>
            </div>
            {cookies.length === 0 ? (
              <div className="faint">
                No cookies yet. They are kept in memory only and never written to
                disk.
              </div>
            ) : (
              <table className="data-table text">
                <thead>
                  <tr>
                    <th>Domain</th>
                    <th style={{ textAlign: "left" }}>Cookie</th>
                  </tr>
                </thead>
                <tbody>
                  {cookies.map((c, i) => (
                    <tr key={i}>
                      <td className="mono">{c.domain}</td>
                      <td className="mono selectable" style={{ textAlign: "left" }}>
                        {c.raw}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          <div className="sep" />

          <section className="col">
            <div className="stat-label">About</div>
            <div className="faint">
              Swarmo {import.meta.env.MODE === "development" ? "(development build)" : ""}
              {" · "}
              {fmtTime(Date.now())}
            </div>
            <div className="hint">
              Swarmo works entirely on your machine. It has no account, no sync
              and no telemetry; the only network requests it makes are the ones
              you ask it to send.
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
