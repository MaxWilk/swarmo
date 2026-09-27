import { useState } from "react";
import * as api from "../api";
import type { Auth, AuthType } from "../api";
import { Icons } from "./ui";
import { reportError } from "../stores/toast";

/**
 * The auth section, shared by the HTTP and gRPC request editors.
 *
 * One component rather than one per protocol: the auth model is identical, and
 * two copies is how the two drift.
 */
export function AuthEditor({ auth, onChange }: { auth: Auth; onChange: (auth: Auth) => void }) {
  const type: AuthType = auth.type;

  const setType = (t: AuthType) => {
    switch (t) {
      case "inherit":
        onChange({ type: "inherit" });
        break;
      case "none":
        onChange({ type: "none" });
        break;
      case "basic":
        onChange({ type: "basic", username: "", password: "" });
        break;
      case "bearer":
        onChange({ type: "bearer", token: "" });
        break;
      case "apiKeyHeader":
        onChange({ type: "apiKeyHeader", headerName: "", value: "" });
        break;
      case "commandToken":
        onChange({
          type: "commandToken",
          command: "",
          headerName: "Authorization",
          prefix: "Bearer ",
        });
        break;
    }
  };

  return (
    <div className="col">
      <div className="field">
        <label>Type</label>
        <select
          className="select"
          value={type}
          onChange={(e) => setType(e.target.value as AuthType)}
        >
          <option value="inherit">Inherit from parent</option>
          <option value="none">No auth</option>
          <option value="basic">Basic auth</option>
          <option value="bearer">Bearer token</option>
          <option value="apiKeyHeader">API key header</option>
          <option value="commandToken">Token from a command</option>
        </select>
      </div>

      {type === "inherit" && (
        <div className="hint">
          Uses the auth configured on the parent folder or collection, if any.
        </div>
      )}

      {auth.type === "basic" && (
        <>
          <div className="field">
            <label>Username</label>
            <input
              className="input"
              value={auth.username}
              onChange={(e) => onChange({ ...auth, username: e.target.value })}
            />
          </div>
          <div className="field">
            <label>Password</label>
            <input
              type="password"
              className="input"
              value={auth.password}
              onChange={(e) => onChange({ ...auth, password: e.target.value })}
            />
          </div>
        </>
      )}

      {auth.type === "bearer" && (
        <div className="field">
          <label>Token</label>
          <input
            className="input mono"
            value={auth.token}
            onChange={(e) => onChange({ ...auth, token: e.target.value })}
          />
        </div>
      )}

      {auth.type === "apiKeyHeader" && (
        <>
          <div className="field">
            <label>Header name</label>
            <input
              className="input mono"
              value={auth.headerName}
              onChange={(e) => onChange({ ...auth, headerName: e.target.value })}
            />
          </div>
          <div className="field">
            <label>Value</label>
            <input
              className="input mono"
              value={auth.value}
              onChange={(e) => onChange({ ...auth, value: e.target.value })}
            />
          </div>
        </>
      )}

      {auth.type === "commandToken" && <CommandTokenFields auth={auth} onChange={onChange} />}
    </div>
  );
}

function CommandTokenFields({
  auth,
  onChange,
}: {
  auth: Extract<Auth, { type: "commandToken" }>;
  onChange: (auth: Auth) => void;
}) {
  const [advanced, setAdvanced] = useState(
    auth.headerName !== "Authorization" || auth.prefix !== "Bearer ",
  );
  const [result, setResult] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);

  const test = async () => {
    setTesting(true);
    setResult(null);
    try {
      const out = await api.authCommandTest(auth.command);
      const expiry =
        out.expiresInSec == null
          ? "no stated expiry"
          : `expires in ${Math.round(out.expiresInSec / 60)} min`;
      setResult(`${out.masked} · ${out.length} characters · ${expiry}`);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      // The approval gate speaks through the error; ask rather than just
      // reporting a failure the user cannot act on.
      if (message.startsWith(UNAPPROVED_PREFIX)) {
        const command = message.slice(UNAPPROVED_PREFIX.length);
        if (
          window.confirm(
            `Swarmo has not run this command in this workspace before:\n\n${command}\n\n` +
              `It will run on your machine with your permissions. Allow it?`,
          )
        ) {
          try {
            await api.authCommandApprove(command);
            // Awaited, so the retry owns the "Running…" state until it is
            // really done; returning early left the button re-enabled
            // while the command was still running.
            await test();
            return;
          } catch (approveError) {
            reportError("Could not save that approval", approveError);
          }
        }
      } else {
        setResult(message);
      }
    } finally {
      setTesting(false);
    }
  };

  return (
    <>
      <div className="field">
        <label>Command</label>
        <input
          className="input mono"
          placeholder="gcloud auth print-identity-token"
          value={auth.command}
          onChange={(e) => onChange({ ...auth, command: e.target.value })}
        />
        <div className="hint">
          Run on your machine to produce the token. The result is kept in memory only, is
          never written to the workspace, and is refreshed once if the server rejects it.
        </div>
      </div>

      <div className="row" style={{ gap: 8 }}>
        <button
          className="btn sm"
          disabled={!auth.command.trim() || testing}
          onClick={() => void test()}
        >
          {testing ? "Running…" : "Test"}
        </button>
        <button className="btn sm ghost" onClick={() => setAdvanced((v) => !v)}>
          {advanced ? "Hide" : "Advanced"}
        </button>
        {result && <span className="hint mono selectable">{result}</span>}
      </div>

      {advanced && (
        <>
          <div className="field">
            <label>Header name</label>
            <input
              className="input mono"
              value={auth.headerName}
              onChange={(e) => onChange({ ...auth, headerName: e.target.value })}
            />
          </div>
          <div className="field">
            <label>Prefix</label>
            <input
              className="input mono"
              value={auth.prefix}
              onChange={(e) => onChange({ ...auth, prefix: e.target.value })}
            />
            <div className="hint">
              Text placed before the token. Leave empty to send the token on its own.
            </div>
          </div>
        </>
      )}
    </>
  );
}

/** Matches `swarmo_app::state::UNAPPROVED_COMMAND_PREFIX`. */
const UNAPPROVED_PREFIX = "SWARMO_UNAPPROVED_COMMAND:";

/** True when an error is the backend asking for approval rather than failing. */
export function isUnapprovedCommandError(message: string): boolean {
  return message.startsWith(UNAPPROVED_PREFIX);
}

/** The command an approval-required error is about. */
export function unapprovedCommandOf(message: string): string {
  return message.slice(UNAPPROVED_PREFIX.length);
}

export { Icons };
