/**
 * A compact builder for `{{$generator}}` tokens.
 *
 * Typing these by hand means remembering argument order, quoting rules and
 * which transforms exist — and getting it wrong is silent, because an
 * unrecognised token is sent through as literal text. The picker builds the
 * token from labelled fields and previews the real expansion through the
 * backend, so what you see is what will actually be sent.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import * as api from "../api";
import { Icons } from "./ui";

import {
  GENERATORS,
  INNER_GENERATORS,
  NO_TRANSFORMS,
  buildSpec,
  initialArgs,
  type ArgValues,
  type GenSpec,
  type TransformValues,
} from "../lib/generatorToken";

/**
 * The picker body. Split out from the button so the popover can be dropped
 * into other layouts later without dragging the trigger along.
 */
function PickerBody({ onInsert, onClose }: { onInsert: (token: string) => void; onClose: () => void }) {
  const [genName, setGenName] = useState(GENERATORS[0].name);
  const gen = GENERATORS.find((g) => g.name === genName) ?? GENERATORS[0];
  const [args, setArgs] = useState<ArgValues>(() => initialArgs(GENERATORS[0]));
  const [transforms, setTransforms] = useState<TransformValues>(NO_TRANSFORMS);

  // The element of a repeat: its own generator, args and transforms.
  const [innerName, setInnerName] = useState("seq");
  const innerGen = INNER_GENERATORS.find((g) => g.name === innerName) ?? INNER_GENERATORS[0];
  const [innerArgs, setInnerArgs] = useState<ArgValues>(() =>
    initialArgs(INNER_GENERATORS.find((g) => g.name === "seq") ?? INNER_GENERATORS[0]),
  );
  const [innerTransforms, setInnerTransforms] = useState<TransformValues>(NO_TRANSFORMS);

  const selectGen = (name: string) => {
    const next = GENERATORS.find((g) => g.name === name);
    if (!next) return;
    setGenName(name);
    setArgs(initialArgs(next));
  };

  const selectInner = (name: string) => {
    const next = INNER_GENERATORS.find((g) => g.name === name);
    if (!next) return;
    setInnerName(name);
    setInnerArgs(initialArgs(next));
  };

  const innerSpec = buildSpec(innerGen, innerArgs, innerTransforms, "");
  const spec = buildSpec(gen, args, transforms, innerSpec);
  const token = `{{$${spec}}}`;

  // -- live preview ---------------------------------------------------------
  const [preview, setPreview] = useState<api.TokenPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    // Debounced: a big repeat is real work, and the token changes on every
    // keystroke in the argument fields.
    const timer = setTimeout(() => {
      api
        .tokenPreview(token)
        .then((p) => {
          if (cancelled) return;
          setPreview(p);
          setPreviewError(null);
        })
        .catch((e: unknown) => {
          if (cancelled) return;
          setPreview(null);
          setPreviewError(e instanceof Error ? e.message : String(e));
        });
    }, 180);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [token]);

  const unresolved = preview?.unresolved.length ? preview.unresolved : null;

  // Named `forGen` rather than `spec`: the enclosing scope already has a
  // `spec` holding the assembled token text, and two different meanings under
  // one name in one function is how the wrong one gets read later.
  const argFields = (
    forGen: GenSpec,
    values: ArgValues,
    set: (v: ArgValues) => void,
  ) =>
    forGen.args.map((a) => (
      <div className="field" key={a.key} style={{ minWidth: 96, flex: 1 }}>
        <label>{a.label}</label>
        <input
          className={`input ${a.kind === "number" ? "" : "mono"}`}
          type={a.kind === "number" ? "number" : "text"}
          value={values[a.key] ?? ""}
          placeholder={a.placeholder}
          onChange={(e) => set({ ...values, [a.key]: e.target.value })}
        />
        {a.hint && <div className="hint">{a.hint}</div>}
      </div>
    ));

  const transformFields = (values: TransformValues, set: (v: TransformValues) => void) => (
    <div className="row wrap" style={{ alignItems: "flex-start" }}>
      <div className="field" style={{ minWidth: 92 }}>
        <label>Pad to width</label>
        <input
          className="input"
          type="number"
          min={0}
          placeholder="off"
          value={values.padWidth}
          onChange={(e) => set({ ...values, padWidth: e.target.value })}
        />
      </div>
      <div className="field" style={{ minWidth: 150, flex: 1 }}>
        <label>Wrap in</label>
        <input
          className="input mono"
          placeholder="creative_{}"
          value={values.format}
          onChange={(e) => set({ ...values, format: e.target.value })}
        />
        <div className="hint">
          <span className="mono">{"{}"}</span> is where the value goes.
        </div>
      </div>
      <div className="field" style={{ minWidth: 92 }}>
        <label>Case</label>
        <select
          className="select"
          value={values.casing}
          onChange={(e) => set({ ...values, casing: e.target.value as TransformValues["casing"] })}
        >
          <option value="">Unchanged</option>
          <option value="upper">UPPERCASE</option>
          <option value="lower">lowercase</option>
        </select>
      </div>
      <div className="field" style={{ minWidth: 150 }}>
        <label>Encode</label>
        <select
          className="select"
          value={values.encode}
          onChange={(e) =>
            set({ ...values, encode: e.target.value as TransformValues["encode"] })
          }
        >
          <option value="">Not encoded</option>
          <option value="base64">Base64 (the text)</option>
          <option value="b64f32">Base64 (float32 vector)</option>
          <option value="b64f64">Base64 (float64 vector)</option>
          <option value="b64u128">Base64 (UInt128 keys)</option>
        </select>
        <div className="hint">
          A vector option packs the numbers as raw bytes first — what an embeddings API
          means by base64.
        </div>
      </div>
      <div className="field" style={{ minWidth: 150, justifyContent: "flex-end" }}>
        <label
          className="row"
          style={{ textTransform: "none", fontWeight: 400 }}
          title={
            "Wraps the value in double quotes, escaped. A list of strings in a " +
            "JSON body needs this, or the values arrive unquoted and the body " +
            "will not parse."
          }
        >
          <input
            className="checkbox"
            type="checkbox"
            checked={values.quote}
            onChange={(e) => set({ ...values, quote: e.target.checked })}
          />
          Quote as JSON string
        </label>
      </div>
    </div>
  );

  return (
    <div className="col" style={{ gap: 8 }}>
      <div className="row wrap" style={{ alignItems: "flex-start" }}>
        <div className="field" style={{ minWidth: 180, flex: 1 }}>
          <label>Value</label>
          <select className="select" value={genName} onChange={(e) => selectGen(e.target.value)}>
            {GENERATORS.map((g) => (
              <option key={g.name} value={g.name}>
                {g.label}
              </option>
            ))}
          </select>
          <div className="hint">{gen.desc}</div>
        </div>
        {argFields(gen, args, setArgs)}
      </div>

      {gen.takesInner && (
        <div className="col inset" style={{ gap: 6 }}>
          <div className="row wrap" style={{ alignItems: "flex-start" }}>
            <div className="field" style={{ minWidth: 170, flex: 1 }}>
              <label>Each value is</label>
              <select
                className="select"
                value={innerName}
                onChange={(e) => selectInner(e.target.value)}
              >
                {INNER_GENERATORS.map((g) => (
                  <option key={g.name} value={g.name}>
                    {g.label}
                  </option>
                ))}
              </select>
            </div>
            {argFields(innerGen, innerArgs, setInnerArgs)}
          </div>
          {transformFields(innerTransforms, setInnerTransforms)}
        </div>
      )}

      {!gen.takesInner && transformFields(transforms, setTransforms)}

      <div className="col" style={{ gap: 4 }}>
        <div className="field" style={{ gap: 4 }}>
          <label>Preview</label>
        </div>
        <div className="token-preview mono selectable">
          {previewError ? (
            <span style={{ color: "var(--danger)" }}>{previewError}</span>
          ) : preview ? (
            <>
              {preview.value || <span className="muted">(empty)</span>}
              {preview.truncated && (
                <span className="muted"> … {preview.length.toLocaleString()} characters</span>
              )}
            </>
          ) : (
            <span className="muted">…</span>
          )}
        </div>
        {unresolved && (
          <div className="status-pill warn">Not understood: {unresolved.join(", ")}</div>
        )}
      </div>

      <div className="row" style={{ justifyContent: "space-between" }}>
        <code className="mono muted token-source">{token}</code>
        <div className="row">
          <button className="btn sm" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn primary sm"
            disabled={!!unresolved}
            onClick={() => {
              onInsert(token);
              onClose();
            }}
          >
            Insert
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * The trigger button plus its popover.
 *
 * `align` decides which edge the popover hangs from; a picker on a narrow
 * table cell would otherwise open off the side of the window.
 */
export function InsertVariable({
  onInsert,
  align = "right",
  compact = false,
  title = "Insert a random or generated value",
}: {
  onInsert: (token: string) => void;
  align?: "left" | "right";
  compact?: boolean;
  title?: string;
}) {
  const [open, setOpen] = useState(false);
  const host = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (host.current && !host.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const close = useCallback(() => setOpen(false), []);

  return (
    <div className="menu-host" ref={host}>
      <button
        className={`btn ${compact ? "icon sm ghost" : "sm"}`}
        title={title}
        onClick={() => setOpen((v) => !v)}
      >
        {compact ? <span className="mono">{"{}"}</span> : <><Icons.Plus />Insert variable</>}
      </button>
      {open && (
        <div className={`menu-pop token-pop ${align === "left" ? "align-left" : ""}`}>
          <PickerBody onInsert={onInsert} onClose={close} />
        </div>
      )}
    </div>
  );
}
