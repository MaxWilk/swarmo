import { useEffect, useState } from "react";
import * as api from "../../api";
import type { RequestDef } from "../../api";
import { MethodLabel, Modal } from "../../components/ui";
import { reportError } from "../../stores/toast";

/**
 * Paste a cURL command, see what it will become, import it.
 *
 * The preview matters more than it looks: a command copied from a browser is
 * long and easy to paste wrong, and unsupported options are dropped with a
 * warning rather than refused, so the user needs to see what actually survived
 * before it lands in their collection.
 */
export function CurlImportModal({
  parentRef,
  parentName,
  onClose,
  onImported,
}: {
  parentRef: string;
  parentName: string;
  onClose: () => void;
  onImported: (nodeRef: string) => void;
}) {
  const [text, setText] = useState("");
  const [preview, setPreview] = useState<{ def: RequestDef; warnings: string[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Re-parsed as the user types, debounced so a long paste is not parsed once
  // per keystroke.
  useEffect(() => {
    if (!text.trim()) {
      setPreview(null);
      setError(null);
      return;
    }
    const timer = setTimeout(() => {
      void (async () => {
        try {
          setPreview(await api.curlParse(text));
          setError(null);
        } catch (e) {
          setPreview(null);
          setError(e instanceof Error ? e.message : String(e));
        }
      })();
    }, 250);
    return () => clearTimeout(timer);
  }, [text]);

  const doImport = async () => {
    setBusy(true);
    try {
      onImported(await api.curlImport(parentRef, text));
      onClose();
    } catch (e) {
      reportError("Could not import that command", e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="Import from cURL"
      onClose={onClose}
      wide
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn primary"
            disabled={!preview || busy}
            onClick={() => void doImport()}
          >
            {busy ? "Importing…" : `Import into ${parentName}`}
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 10 }}>
        <div className="field">
          <label htmlFor="curl-text">Command</label>
          <textarea
            id="curl-text"
            className="textarea"
            rows={8}
            autoFocus
            placeholder={"curl 'https://api.example.com/v1/items' \\\n  -H 'accept: application/json'"}
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
          <div className="hint">
            Paste anything from a browser's “Copy as cURL”. Options that describe the
            transport rather than the request are ignored, and listed below.
          </div>
        </div>

        {error && <div className="status-pill err">{error}</div>}

        {preview && (
          <div className="col" style={{ gap: 8 }}>
            <div className="row" style={{ gap: 8 }}>
              <MethodLabel method={preview.def.method} />
              <span className="mono selectable grow" style={{ wordBreak: "break-all" }}>
                {preview.def.url}
              </span>
            </div>
            <div className="hint">
              Saved as <strong>{preview.def.name}</strong> ·{" "}
              {preview.def.headers.length} header
              {preview.def.headers.length === 1 ? "" : "s"} ·{" "}
              {preview.def.body.type === "none" ? "no body" : `${preview.def.body.type} body`}
            </div>
            {preview.warnings.length > 0 && (
              <ul className="col" style={{ gap: 4, margin: 0, paddingLeft: 18 }}>
                {preview.warnings.map((w) => (
                  <li key={w} className="hint">
                    {w}
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}
