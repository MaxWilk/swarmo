/**
 * Keeping open tabs in step with a rename or a move.
 *
 * A tab holds three copies of a request's identity: the ref it lives at, the
 * label on the tab, and the `name` inside the edited definition. Renaming has
 * to move all three. Updating only the first two looks fine until the next
 * edit — which resets the label from `def.name` — or the next save, which
 * writes the stale name back over the file and undoes the rename on disk.
 *
 * Pure and separate from the store so this can be tested directly; it is the
 * kind of bug that only shows up a few interactions later.
 */
import type { Tab } from "./tabs";

export interface TabSlots {
  tabs: Tab[];
  activeRef: string | null;
}

/** Whether `ref` sits inside the container at `containerRef`. */
function isInside(ref: string, containerRef: string): boolean {
  return ref.startsWith(`${containerRef}/`);
}

/**
 * Move every open tab affected by a rename.
 *
 * Handles the request itself, and — when a folder or collection is renamed —
 * every request inside it, whose refs are paths that begin with the old one.
 * Without that, a tab left pointing into a folder that no longer exists fails
 * the next time it is saved.
 */
export function applyRename(
  state: TabSlots,
  oldRef: string,
  newRef: string,
  name: string,
): TabSlots {
  if (oldRef === newRef && !name) return state;

  const moveRef = (ref: string): string | null => {
    if (ref === oldRef) return newRef;
    if (isInside(ref, oldRef)) return newRef + ref.slice(oldRef.length);
    return null;
  };

  const tabs = state.tabs.map((t) => {
    const moved = moveRef(t.nodeRef);
    if (moved === null) return t;

    // Only the renamed thing itself takes the new name; a request inside a
    // renamed folder keeps its own name and only moves.
    const renamed = t.nodeRef === oldRef && name !== "";
    if (!renamed) return { ...t, nodeRef: moved } as Tab;

    // The name lives in the definition as well as on the tab. Leaving `saved`
    // behind would make the tab look dirty the moment it is renamed, and
    // leaving `def` behind would write the old name back on the next save.
    if (t.kind === "http") {
      return {
        ...t,
        nodeRef: moved,
        name,
        def: { ...t.def, name },
        saved: { ...t.saved, name },
      };
    }
    // gRPC and WebSocket tabs carry the name the same way.
    return {
      ...t,
      nodeRef: moved,
      name,
      def: { ...t.def, name },
      saved: { ...t.saved, name },
    } as Tab;
  });

  const activeRef =
    state.activeRef === null ? null : (moveRef(state.activeRef) ?? state.activeRef);

  return { tabs, activeRef };
}
