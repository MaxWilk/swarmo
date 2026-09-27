import { useRef } from "react";
import type { KeyValue } from "../api";
import { InsertVariable } from "./InsertVariable";
import { Icons } from "./ui";

/**
 * The editable key/value grid used for params, headers and form fields.
 * A blank trailing row is always present, so adding a pair needs no button.
 */
export function KeyValueTable({
  rows,
  onChange,
  keyPlaceholder = "Key",
  valuePlaceholder = "Value",
  unresolvedVars,
}: {
  rows: KeyValue[];
  onChange: (rows: KeyValue[]) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
  /** Variable names that failed to resolve, for inline highlighting. */
  unresolvedVars?: Set<string>;
}) {
  const display = [...rows];
  const last = display[display.length - 1];
  if (!last || last.key || last.value) {
    display.push({ key: "", value: "", enabled: true });
  }

  const commit = (next: KeyValue[]) => {
    // Drop trailing blank rows so they never reach disk.
    while (
      next.length &&
      !next[next.length - 1].key &&
      !next[next.length - 1].value
    ) {
      next.pop();
    }
    onChange(next);
  };

  const update = (i: number, patch: Partial<KeyValue>) => {
    const next = display.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    commit(next);
  };

  const remove = (i: number) => commit(display.filter((_, idx) => idx !== i));

  // Kept so a token can be dropped at the caret rather than always appended,
  // and so focus returns to the field the picker was opened from.
  const valueInputs = useRef<Map<number, HTMLInputElement | null>>(new Map());

  const insertIntoValue = (i: number, token: string) => {
    const el = valueInputs.current.get(i);
    const current = display[i]?.value ?? "";
    // A blurred input still reports its selection; appending is the sane
    // fallback when it does not.
    const start = el?.selectionStart ?? current.length;
    const end = el?.selectionEnd ?? current.length;
    update(i, { value: current.slice(0, start) + token + current.slice(end) });
    // After the value round-trips through the parent the input is re-rendered,
    // so the caret has to be restored on the next frame rather than now.
    requestAnimationFrame(() => {
      const node = valueInputs.current.get(i);
      if (!node) return;
      node.focus();
      const caret = start + token.length;
      node.setSelectionRange(caret, caret);
    });
  };

  const hasUnresolved = (text: string) =>
    !!unresolvedVars?.size &&
    [...unresolvedVars].some((v) => text.includes(`{{${v}}}`));

  return (
    <table className="kv-table">
      <thead>
        <tr>
          <th style={{ width: 30 }} />
          <th style={{ width: "38%" }}>{keyPlaceholder}</th>
          <th>{valuePlaceholder}</th>
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
                    title={row.enabled ? "Enabled" : "Disabled"}
                  />
                )}
              </td>
              <td>
                <input
                  className="input mono"
                  value={row.key}
                  placeholder={keyPlaceholder}
                  onChange={(e) => update(i, { key: e.target.value })}
                />
              </td>
              <td>
                <div className="kv-cell">
                  <input
                    ref={(el) => valueInputs.current.set(i, el)}
                    className={`input mono grow ${hasUnresolved(row.value) ? "unresolved" : ""}`}
                    value={row.value}
                    placeholder={valuePlaceholder}
                    onChange={(e) => update(i, { value: e.target.value })}
                  />
                  <InsertVariable
                    compact
                    onInsert={(token) => insertIntoValue(i, token)}
                    title="Insert a random or generated value"
                  />
                </div>
              </td>
              <td>
                {!isBlank && (
                  <button
                    className="btn ghost icon sm"
                    onClick={() => remove(i)}
                    title="Remove"
                  >
                    <Icons.Close />
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
