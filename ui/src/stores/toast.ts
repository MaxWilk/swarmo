import { create } from "zustand";

export type ToastKind = "error" | "success" | "info";

export interface Toast {
  id: string;
  kind: ToastKind;
  title: string;
  message?: string;
}

interface ToastState {
  toasts: Toast[];
  push: (t: Omit<Toast, "id">) => void;
  dismiss: (id: string) => void;
}

let seq = 0;

export const useToasts = create<ToastState>((set) => ({
  toasts: [],
  push: (t) => {
    const id = `t${++seq}`;
    set((s) => ({ toasts: [...s.toasts, { ...t, id }] }));
    // Errors stay until dismissed; the message usually needs reading.
    if (t.kind !== "error") {
      setTimeout(
        () => set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) })),
        4000,
      );
    }
  },
  dismiss: (id) => set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) })),
}));

/** Report a caught error to the user without swallowing it. */
export function reportError(title: string, e: unknown) {
  const message = e instanceof Error ? e.message : String(e);
  useToasts.getState().push({ kind: "error", title, message });
  console.error(title, e);
}

export function notify(title: string, message?: string) {
  useToasts.getState().push({ kind: "success", title, message });
}
