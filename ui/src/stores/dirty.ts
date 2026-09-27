/**
 * Whether an edited definition differs from its saved copy.
 *
 * Memoised on the identity of the (edited, saved) pair: the store replaces
 * `def` on every edit and `saved` on every save, so identity is a faithful
 * proxy for "something changed" — and it spares the tab strip two
 * serialisations of an 8 MB body for every open tab on every keystroke.
 *
 * Pure and separate from the store so it can be tested without the Tauri
 * bridge the store imports.
 */
const cache = new WeakMap<object, { saved: object; dirty: boolean }>();

export function isDirtyPair(edited: object, saved: object): boolean {
  const hit = cache.get(edited);
  if (hit && hit.saved === saved) return hit.dirty;
  const dirty = JSON.stringify(edited) !== JSON.stringify(saved);
  cache.set(edited, { saved, dirty });
  return dirty;
}
