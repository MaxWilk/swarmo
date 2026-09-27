import { create } from "zustand";
import * as api from "../api";
import type { Settings, TreeNode, WorkspaceInfo } from "../api";
import { reportError } from "./toast";

interface WorkspaceState {
  info: WorkspaceInfo | null;
  tree: TreeNode[];
  environments: string[];
  activeEnv: string | null;
  /** Every variable that resolves right now, for highlighting and hovers. */
  effectiveVars: Record<string, string>;
  settings: Settings;
  loading: boolean;
  booted: boolean;

  boot: () => Promise<void>;
  open: (path: string) => Promise<void>;
  create: (path: string, name: string) => Promise<void>;
  close: () => Promise<void>;
  refreshTree: () => Promise<void>;
  refreshEnvs: () => Promise<void>;
  setActiveEnv: (name: string | null) => Promise<void>;
  saveSettings: (s: Settings) => Promise<void>;
}

let booting = false;

const DEFAULT_SETTINGS: Settings = {
  theme: "system",
  proxy: null,
  defaultTimeoutMs: 30000,
  defaultVerifyTls: true,
  recentWorkspaces: [],
};

export const useWorkspace = create<WorkspaceState>((set, get) => ({
  info: null,
  tree: [],
  environments: [],
  activeEnv: null,
  effectiveVars: {},
  settings: DEFAULT_SETTINGS,
  loading: false,
  booted: false,

  boot: async () => {
    // React StrictMode mounts effects twice in development; booting twice
    // would double every startup toast.
    if (get().booted || booting) return;
    booting = true;
    try {
      const [settings, info] = await Promise.all([
        api.settingsGet(),
        api.workspaceInfo(),
      ]);
      set({ settings, booted: true });
      applyTheme(settings.theme);
      if (info) {
        set({
          info,
          environments: info.environments,
          activeEnv: info.activeEnvironment,
        });
        await Promise.all([get().refreshTree(), refreshVars(set)]);
      }
    } catch (e) {
      set({ booted: true });
      reportError("Could not start Swarmo", e);
    }
  },

  open: async (path) => {
    set({ loading: true });
    try {
      const info = await api.workspaceOpen(path);
      set({
        info,
        environments: info.environments,
        activeEnv: info.activeEnvironment,
      });
      await Promise.all([get().refreshTree(), refreshVars(set)]);
    } finally {
      set({ loading: false });
    }
  },

  create: async (path, name) => {
    set({ loading: true });
    try {
      const info = await api.workspaceCreate(path, name);
      set({
        info,
        environments: info.environments,
        activeEnv: info.activeEnvironment,
      });
      await Promise.all([get().refreshTree(), refreshVars(set)]);
    } finally {
      set({ loading: false });
    }
  },

  close: async () => {
    try {
      await api.workspaceClose();
      set({
        info: null,
        tree: [],
        environments: [],
        activeEnv: null,
        effectiveVars: {},
      });
    } catch (e) {
      reportError("Could not close the workspace", e);
    }
  },

  refreshTree: async () => {
    if (!get().info) return;
    try {
      set({ tree: await api.treeGet() });
    } catch (e) {
      reportError("Could not read the workspace", e);
    }
  },

  refreshEnvs: async () => {
    if (!get().info) return;
    try {
      const environments = await api.envList();
      set({ environments });
      await refreshVars(set);
    } catch (e) {
      reportError("Could not read environments", e);
    }
  },

  setActiveEnv: async (name) => {
    try {
      await api.envSetActive(name);
      set({ activeEnv: name });
      await refreshVars(set);
    } catch (e) {
      reportError("Could not switch environment", e);
    }
  },

  saveSettings: async (s) => {
    try {
      await api.settingsSave(s);
      set({ settings: s });
      applyTheme(s.theme);
    } catch (e) {
      reportError("Could not save settings", e);
    }
  },
}));

type Setter = (partial: Partial<WorkspaceState>) => void;

async function refreshVars(set: Setter) {
  try {
    set({ effectiveVars: await api.envEffective() });
  } catch {
    set({ effectiveVars: {} });
  }
}

/** Refresh the variable table after scripts may have set new values. */
export async function refreshEffectiveVars() {
  try {
    useWorkspace.setState({ effectiveVars: await api.envEffective() });
  } catch {
    /* leave the previous values in place */
  }
}

export function applyTheme(theme: Settings["theme"]) {
  const root = document.documentElement;
  if (theme === "system") {
    const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    root.setAttribute("data-theme", dark ? "dark" : "light");
  } else {
    root.setAttribute("data-theme", theme);
  }
}

// Follow the OS while the user is on "system".
window
  .matchMedia("(prefers-color-scheme: dark)")
  .addEventListener("change", () => {
    if (useWorkspace.getState().settings.theme === "system") applyTheme("system");
  });

/** Depth-first walk over the collection tree. */
export function walkTree(
  nodes: TreeNode[],
  fn: (n: TreeNode, parents: TreeNode[]) => void,
  parents: TreeNode[] = [],
) {
  for (const n of nodes) {
    fn(n, parents);
    if (n.children.length) walkTree(n.children, fn, [...parents, n]);
  }
}

/**
 * Find a request by its stable id, wherever it now sits in the tree.
 *
 * The counterpart to `findNode`: a ref says where something was, an id says
 * which thing it is, and a rename separates the two.
 */
export function findNodeById(nodes: TreeNode[], id: string): TreeNode | null {
  for (const n of nodes) {
    if (n.id === id) return n;
    const hit = findNodeById(n.children, id);
    if (hit) return hit;
  }
  return null;
}

export function findNode(nodes: TreeNode[], ref: string): TreeNode | null {
  let found: TreeNode | null = null;
  walkTree(nodes, (n) => {
    if (n.nodeRef === ref) found = n;
  });
  return found;
}
