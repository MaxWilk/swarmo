import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useToasts } from "../stores/toast";
// Formatting helpers live in a JSX-free module so they can be unit tested;
// re-exported here because every view already imports them from "ui".
export * from "./format";

// -- icons -------------------------------------------------------------------

const ico = (d: ReactNode) => (props: { className?: string }) => (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    className={props.className}
    width="1em"
    height="1em"
    aria-hidden="true"
  >
    {d}
  </svg>
);

export const Icons = {
  Send: ico(<path d="m22 2-7 20-4-9-9-4Z" />),
  Chevron: ico(<path d="m9 18 6-6-6-6" />),
  ChevronDown: ico(<path d="m6 9 6 6 6-6" />),
  Close: ico(<><path d="M18 6 6 18" /><path d="m6 6 12 12" /></>),
  Plus: ico(<><path d="M12 5v14" /><path d="M5 12h14" /></>),
  Folder: ico(
    <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />,
  ),
  Collection: ico(
    <><path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H20v20H6.5a2.5 2.5 0 0 1 0-5H20" /></>,
  ),
  Trash: ico(
    <><path d="M3 6h18" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /></>,
  ),
  Search: ico(<><circle cx="11" cy="11" r="8" /><path d="m21 21-4.3-4.3" /></>),
  Play: ico(<polygon points="6 3 20 12 6 21 6 3" />),
  Stop: ico(<rect x="6" y="6" width="12" height="12" rx="2" />),
  Gauge: ico(
    <><path d="m12 14 4-4" /><path d="M3.34 19a10 10 0 1 1 17.32 0" /></>,
  ),
  Grip: ico(
    <>
      <circle cx="9" cy="6" r="1.4" fill="currentColor" /><circle cx="15" cy="6" r="1.4" fill="currentColor" />
      <circle cx="9" cy="12" r="1.4" fill="currentColor" /><circle cx="15" cy="12" r="1.4" fill="currentColor" />
      <circle cx="9" cy="18" r="1.4" fill="currentColor" /><circle cx="15" cy="18" r="1.4" fill="currentColor" />
    </>,
  ),
  Chart: ico(
    <><path d="M3 3v18h18" /><path d="M7 15v3" /><path d="M12 9v9" /><path d="M17 5v13" /></>,
  ),
  History: ico(
    <><path d="M3 12a9 9 0 1 0 3-6.7L3 8" /><path d="M3 3v5h5" /><path d="M12 7v5l3 2" /></>,
  ),
  Settings: ico(
    <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" /></>,
  ),
  Copy: ico(
    <><rect width="14" height="14" x="8" y="8" rx="2" /><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" /></>,
  ),
  Edit: ico(
    <><path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" /><path d="M18.5 2.5a2.12 2.12 0 0 1 3 3L12 15l-4 1 1-4Z" /></>,
  ),
  Import: ico(
    <><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" /><polyline points="7 10 12 15 17 10" /><path d="M12 15V3" /></>,
  ),
  Check: ico(<path d="M20 6 9 17l-5-5" />),
  Alert: ico(
    <><circle cx="12" cy="12" r="10" /><path d="M12 8v4" /><path d="M12 16h.01" /></>,
  ),
  Code: ico(<><polyline points="16 18 22 12 16 6" /><polyline points="8 6 2 12 8 18" /></>),
  File: ico(
    <><path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" /><path d="M14 2v5h6" /></>,
  ),
};

// -- toasts ------------------------------------------------------------------

export function Toasts() {
  const { toasts, dismiss } = useToasts();
  if (!toasts.length) return null;
  return (
    <div className="toasts">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`}>
          <div className="toast-body">
            <div className="toast-title">{t.title}</div>
            {t.message && <div className="toast-msg mono">{t.message}</div>}
          </div>
          <button
            className="tab-close"
            onClick={() => dismiss(t.id)}
            title="Dismiss"
          >
            <Icons.Close />
          </button>
        </div>
      ))}
    </div>
  );
}

// -- modal -------------------------------------------------------------------

interface ModalProps {
  title: string;
  children: ReactNode;
  onClose: () => void;
  footer?: ReactNode;
  wide?: boolean;
}

export function Modal({ title, children, onClose, footer, wide }: ModalProps) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className={`modal ${wide ? "wide" : ""}`} role="dialog">
        <div className="modal-header">{title}</div>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>
  );
}

/** A modal that asks for a single line of text. */
export function PromptModal({
  title,
  label,
  initial = "",
  confirmLabel = "Create",
  onConfirm,
  onClose,
}: {
  title: string;
  label: string;
  initial?: string;
  confirmLabel?: string;
  onConfirm: (value: string) => void;
  onClose: () => void;
}) {
  const [value, setValue] = useState(initial);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.select();
  }, []);

  const submit = () => {
    const v = value.trim();
    if (!v) return;
    onConfirm(v);
    // Dismissing is the modal's own job. Leaving it to each caller means the
    // one that forgets shows a dialog that stays open after it has already
    // done the thing — which reads as the action having failed.
    onClose();
  };

  return (
    <Modal
      title={title}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn primary" onClick={submit} disabled={!value.trim()}>
            {confirmLabel}
          </button>
        </>
      }
    >
      <div className="field">
        <label>{label}</label>
        <input
          ref={ref}
          className="input"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
          }}
        />
      </div>
    </Modal>
  );
}

/**
 * A yes/no gate before something that cannot be undone.
 *
 * The house rule for when to use one: **bulk or irreplaceable ⇒ confirm;
 * a single small item ⇒ just do it.** Clearing all history and deleting a run
 * confirm; deleting one history entry does not. Follow it rather than adding
 * a modal per instinct, or the app ends up asking twice for trivia and not at
 * all for the dangerous thing.
 */
export function ConfirmModal({
  title,
  message,
  confirmLabel = "Delete",
  danger = true,
  onConfirm,
  onClose,
}: {
  title: string;
  message: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Modal
      title={title}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className={`btn ${danger ? "danger" : "primary"}`}
            onClick={() => {
              onConfirm();
              onClose();
            }}
          >
            {confirmLabel}
          </button>
        </>
      }
    >
      <div>{message}</div>
    </Modal>
  );
}

// -- split pane --------------------------------------------------------------

export function Split({
  vertical = false,
  initial = 50,
  min = 15,
  max = 85,
  storageKey,
  children,
}: {
  vertical?: boolean;
  initial?: number;
  min?: number;
  max?: number;
  storageKey?: string;
  children: [ReactNode, ReactNode];
}) {
  const [pct, setPct] = useState(() => {
    if (!storageKey) return initial;
    const saved = Number(localStorage.getItem(`split:${storageKey}`));
    return Number.isFinite(saved) && saved >= min && saved <= max ? saved : initial;
  });
  const host = useRef<HTMLDivElement>(null);
  const dragging = useRef(false);

  const onMove = useCallback(
    (e: MouseEvent) => {
      if (!dragging.current || !host.current) return;
      const r = host.current.getBoundingClientRect();
      const raw = vertical
        ? ((e.clientY - r.top) / r.height) * 100
        : ((e.clientX - r.left) / r.width) * 100;
      const next = Math.max(min, Math.min(max, raw));
      setPct(next);
    },
    [vertical, min, max],
  );

  useEffect(() => {
    const up = () => {
      if (!dragging.current) return;
      dragging.current = false;
      document.body.style.cursor = "";
      if (storageKey) localStorage.setItem(`split:${storageKey}`, String(pct));
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", up);
    };
  }, [onMove, pct, storageKey]);

  const first = vertical ? { height: `${pct}%` } : { width: `${pct}%` };

  return (
    <div className={`split ${vertical ? "vertical" : ""}`} ref={host}>
      <div style={{ ...first, display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 }}>
        {children[0]}
      </div>
      <div
        className="splitter"
        onMouseDown={() => {
          dragging.current = true;
          document.body.style.cursor = vertical ? "row-resize" : "col-resize";
        }}
      />
      <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, minHeight: 0 }}>
        {children[1]}
      </div>
    </div>
  );
}

// -- misc --------------------------------------------------------------------

/**
 * A small menu hung off a button.
 *
 * Closes on outside click and on Escape, so it never strands the user with an
 * open menu they cannot dismiss with the keyboard. Items are ordinary buttons
 * so Tab order works without any roving-tabindex machinery.
 */
export function Menu({
  label,
  icon,
  items,
  title,
}: {
  label: string;
  icon?: ReactNode;
  title?: string;
  items: { label: string; hint?: string; onSelect: () => void }[];
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

  return (
    <div className="menu-host" ref={host}>
      <button className="btn sm" title={title} onClick={() => setOpen((v) => !v)}>
        {icon}
        {label}
        <Icons.ChevronDown />
      </button>
      {open && (
        <div className="menu-pop">
          {items.map((item) => (
            <button
              key={item.label}
              className="menu-item"
              onClick={() => {
                setOpen(false);
                item.onSelect();
              }}
            >
              <span>{item.label}</span>
              {item.hint && <span className="hint">{item.hint}</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function EmptyState({
  title,
  children,
  action,
}: {
  title: string;
  children?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="empty">
      <h2>{title}</h2>
      {children && <p>{children}</p>}
      {action}
    </div>
  );
}

export function StatusPill({ status }: { status: number }) {
  const cls =
    status === 0
      ? "err"
      : status < 300
        ? "ok"
        : status < 400
          ? "warn"
          : "err";
  return (
    <span className={`status-pill ${cls}`}>
      {status === 0 ? "Failed" : status}
    </span>
  );
}

export function MethodLabel({ method }: { method: string }) {
  const m = method.toLowerCase();
  const known = ["get", "post", "put", "patch", "delete", "head", "options"];
  return (
    <span className={`method ${known.includes(m) ? m : "other"}`}>
      {method.toUpperCase().slice(0, 6)}
    </span>
  );
}

