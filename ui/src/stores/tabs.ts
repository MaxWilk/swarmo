import { create } from "zustand";
import * as api from "../api";
import type {
  GrpcRequestDef,
  GrpcSendResult,
  RequestDef,
  SendResult,
  WsRequestDef,
  WsSendResult,
} from "../api";
import { reportError } from "./toast";
import { isDirtyPair } from "./dirty";
import { applyRename } from "./tabRename";
import { refreshEffectiveVars } from "./workspace";

export type SubTab =
  | "params"
  | "headers"
  | "auth"
  | "body"
  | "scripts"
  | "settings";

export type GrpcSubTab = "message" | "metadata" | "auth" | "proto" | "scripts" | "settings";

export interface HttpTab {
  kind: "http";
  /** Workspace ref of the request; also the tab's identity. */
  nodeRef: string;
  name: string;
  /** The edited copy. `saved` is what is on disk. */
  def: RequestDef;
  saved: RequestDef;
  subTab: SubTab;
  result: SendResult | null;
  sending: boolean;
  execId: string | null;
}

export interface GrpcTab {
  kind: "grpc";
  nodeRef: string;
  name: string;
  def: GrpcRequestDef;
  saved: GrpcRequestDef;
  subTab: GrpcSubTab;
  result: GrpcSendResult | null;
  sending: boolean;
  execId: string | null;
}

export type WsSubTab = "messages" | "headers" | "auth" | "settings";

export interface WsTab {
  kind: "ws";
  nodeRef: string;
  name: string;
  def: WsRequestDef;
  saved: WsRequestDef;
  subTab: WsSubTab;
  result: WsSendResult | null;
  sending: boolean;
  execId: string | null;
}

export type Tab = HttpTab | GrpcTab | WsTab;
export const isWsTab = (t: Tab): t is WsTab => t.kind === "ws";

export const isHttpTab = (t: Tab): t is HttpTab => t.kind === "http";
export const isGrpcTab = (t: Tab): t is GrpcTab => t.kind === "grpc";

interface TabsState {
  tabs: Tab[];
  activeRef: string | null;

  open: (nodeRef: string) => Promise<void>;
  close: (nodeRef: string) => void;
  closeAll: () => void;
  activate: (nodeRef: string) => void;
  update: (nodeRef: string, def: RequestDef | GrpcRequestDef | WsRequestDef) => void;
  /**
   * Merge a partial change into the tab's *current* definition.
   *
   * Prefer this over `update` from anything asynchronous: a caller that
   * awaited a backend call is holding the `def` it saw before the await, and
   * replacing wholesale would revert every field edited meanwhile.
   */
  patch: (
    nodeRef: string,
    partial: Partial<RequestDef> | Partial<GrpcRequestDef> | Partial<WsRequestDef>,
  ) => void;
  setSubTab: (nodeRef: string, subTab: SubTab | GrpcSubTab | WsSubTab) => void;
  /** Save the editor snapshot, returning whether it reached disk. */
  save: (nodeRef: string) => Promise<boolean>;
  send: (nodeRef: string) => Promise<void>;
  cancel: (nodeRef: string) => Promise<void>;
  /** Reflect a rename/move that changed a request's ref. */
  /**
   * Follow a rename or a move.
   *
   * `name` is empty for a move, or when a container was renamed: the requests
   * inside it keep their own names and only change path.
   */
  rekey: (oldRef: string, newRef: string, name: string) => void;
}

// Opening is asynchronous. Track requests outside the serializable store so a
// double click cannot append the same tab twice.
const opening = new Set<string>();
let tabGeneration = 0;

export const isDirty = (t: Tab): boolean => isDirtyPair(t.def, t.saved);

export const useTabs = create<TabsState>((set, get) => ({
  tabs: [],
  activeRef: null,

  open: async (nodeRef) => {
    const existing = get().tabs.find((t) => t.nodeRef === nodeRef);
    if (existing) {
      set({ activeRef: nodeRef });
      return;
    }
    if (opening.has(nodeRef)) return;
    opening.add(nodeRef);
    const generation = tabGeneration;
    try {
      if (api.isWsRef(nodeRef)) {
        const def = await api.wsRequestGet(nodeRef);
        if (generation !== tabGeneration) return;
        const tab: WsTab = {
          kind: "ws",
          nodeRef,
          name: def.name,
          def,
          saved: structuredClone(def),
          subTab: "messages",
          result: null,
          sending: false,
          execId: null,
        };
        set((s) => ({ tabs: [...s.tabs, tab], activeRef: nodeRef }));
      } else if (api.isGrpcRef(nodeRef)) {
        const def = await api.grpcRequestGet(nodeRef);
        if (generation !== tabGeneration) return;
        const tab: GrpcTab = {
          kind: "grpc",
          nodeRef,
          name: def.name,
          def,
          saved: structuredClone(def),
          subTab: "message",
          result: null,
          sending: false,
          execId: null,
        };
        set((s) => ({ tabs: [...s.tabs, tab], activeRef: nodeRef }));
      } else {
        const def = await api.requestGet(nodeRef);
        if (generation !== tabGeneration) return;
        const tab: HttpTab = {
          kind: "http",
          nodeRef,
          name: def.name,
          def,
          saved: structuredClone(def),
          subTab: defaultSubTab(def),
          result: null,
          sending: false,
          execId: null,
        };
        set((s) => ({ tabs: [...s.tabs, tab], activeRef: nodeRef }));
      }
    } catch (e) {
      if (generation === tabGeneration) reportError("Could not open the request", e);
    } finally {
      opening.delete(nodeRef);
    }
  },

  close: (nodeRef) =>
    set((s) => {
      const tabs = s.tabs.filter((t) => t.nodeRef !== nodeRef);
      let activeRef = s.activeRef;
      if (activeRef === nodeRef) {
        const idx = s.tabs.findIndex((t) => t.nodeRef === nodeRef);
        activeRef =
          tabs[Math.min(idx, tabs.length - 1)]?.nodeRef ?? null;
      }
      return { tabs, activeRef };
    }),

  closeAll: () => {
    tabGeneration++;
    opening.clear();
    set({ tabs: [], activeRef: null });
  },

  activate: (nodeRef) => set({ activeRef: nodeRef }),

  update: (nodeRef, def) =>
    set((s) => ({
      tabs: s.tabs.map((t) => {
        if (t.nodeRef !== nodeRef) return t;
        if (t.kind === "http") return { ...t, def: def as RequestDef, name: def.name };
        if (t.kind === "ws") return { ...t, def: def as WsRequestDef, name: def.name };
        return { ...t, def: def as GrpcRequestDef, name: def.name };
      }),
    })),

  patch: (nodeRef, partial) =>
    set((s) => ({
      tabs: s.tabs.map((t) => {
        if (t.nodeRef !== nodeRef) return t;
        if (t.kind === "http") {
          const def = { ...t.def, ...(partial as Partial<RequestDef>) };
          return { ...t, def, name: def.name };
        }
        if (t.kind === "ws") {
          const def = { ...t.def, ...(partial as Partial<WsRequestDef>) };
          return { ...t, def, name: def.name };
        }
        const def = { ...t.def, ...(partial as Partial<GrpcRequestDef>) };
        return { ...t, def, name: def.name };
      }),
    })),

  setSubTab: (nodeRef, subTab) =>
    set((s) => ({
      tabs: s.tabs.map((t) => {
        if (t.nodeRef !== nodeRef) return t;
        if (t.kind === "http") return { ...t, subTab: subTab as SubTab };
        if (t.kind === "ws") return { ...t, subTab: subTab as WsSubTab };
        return { ...t, subTab: subTab as GrpcSubTab };
      }),
    })),

  save: async (nodeRef) => {
    const tab = get().tabs.find((t) => t.nodeRef === nodeRef);
    if (!tab) return false;
    // Keep the exact snapshot sent to the backend. If the user edits again
    // while the save is in flight, those newer edits must remain dirty.
    const saved = structuredClone(tab.def);
    try {
      if (tab.kind === "http") {
        await api.requestSave(nodeRef, saved as RequestDef);
      } else if (tab.kind === "ws") {
        await api.wsRequestSave(nodeRef, saved as WsRequestDef);
      } else {
        await api.grpcRequestSave(nodeRef, saved as GrpcRequestDef);
      }
      // Matched by stable id: a rename during the save changes the ref.
      const id = tab.def.id;
      set((s) => ({
        tabs: s.tabs.map((t) => {
          if (t.def.id !== id) return t;
          if (t.kind === "http") {
            return { ...t, saved: saved as RequestDef };
          }
          if (t.kind === "ws") {
            return { ...t, saved: saved as WsRequestDef };
          }
          return { ...t, saved: saved as GrpcRequestDef };
        }),
      }));
      return true;
    } catch (e) {
      reportError("Could not save the request", e);
      return false;
    }
  },

  send: async (nodeRef) => {
    const tab = get().tabs.find((t) => t.nodeRef === nodeRef);
    if (!tab || tab.sending) return;
    const generation = tabGeneration;
    // The tab's stable identity. A rename or move while the send is in
    // flight changes `nodeRef`; matching on that afterwards would find no
    // tab, leave `sending` stuck and drop the result.
    const id = tab.def.id;
    const byId = (t: Tab) => t.def.id === id;

    // Marked in flight *before* the save below is awaited, or a second send
    // arriving during that await would pass the guard and run too.
    const execId = crypto.randomUUID();
    set((s) => ({
      tabs: s.tabs.map((t) => (byId(t) ? { ...t, sending: true, execId } : t)),
    }));
    const clear = () =>
      set((s) => ({
        tabs: s.tabs.map((t) => (byId(t) ? { ...t, sending: false, execId: null } : t)),
      }));

    // The engine reads the request from disk, so save first.
    // Do not send stale contents when saving the editor failed.
    if (isDirty(tab) && !(await get().save(nodeRef))) {
      clear();
      return;
    }
    if (generation !== tabGeneration) return;

    // Read the ref afresh: it is what the backend keys on, and it may have
    // moved during the save.
    const currentRef = get().tabs.find(byId)?.nodeRef ?? nodeRef;

    try {
      if (tab.kind === "ws") {
        const result = await api.wsSend(currentRef, execId);
        set((s) => ({
          tabs: s.tabs.map((t) =>
            byId(t) && t.kind === "ws" ? { ...t, result, sending: false, execId: null } : t,
          ),
        }));
      } else if (tab.kind === "http") {
        const result = await api.requestSend(currentRef, execId);
        set((s) => ({
          tabs: s.tabs.map((t) =>
            byId(t) && t.kind === "http" ? { ...t, result, sending: false, execId: null } : t,
          ),
        }));
        if (Object.keys(result.variablesSet).length) void refreshEffectiveVars();
      } else {
        const result = await api.grpcSend(currentRef, execId);
        set((s) => ({
          tabs: s.tabs.map((t) =>
            byId(t) && t.kind === "grpc" ? { ...t, result, sending: false, execId: null } : t,
          ),
        }));
        if (Object.keys(result.variablesSet).length) void refreshEffectiveVars();
      }
    } catch (e) {
      clear();
      reportError(
        tab.kind === "grpc" ? "Call failed" : tab.kind === "ws" ? "Session failed" : "Request failed",
        e,
      );
    }
  },

  cancel: async (nodeRef) => {
    const tab = get().tabs.find((t) => t.nodeRef === nodeRef);
    if (!tab?.execId) return;
    try {
      await api.requestCancel(tab.execId);
    } catch (e) {
      reportError("Could not cancel the request", e);
    }
  },

  rekey: (oldRef, newRef, name) =>
    set((s) => applyRename({ tabs: s.tabs, activeRef: s.activeRef }, oldRef, newRef, name)),
}));

function defaultSubTab(def: RequestDef): SubTab {
  return def.body.type === "none" ? "params" : "body";
}

export function activeTab(): Tab | null {
  const { tabs, activeRef } = useTabs.getState();
  return tabs.find((t) => t.nodeRef === activeRef) ?? null;
}
