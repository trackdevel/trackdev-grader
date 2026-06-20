/**
 * Persistence for `SortableTable` column orderings. A table opts in by passing
 * an `id`; its `{ key, dir }` choice is then remembered in localStorage so it
 * survives navigating away and back — and across app restarts (the Tauri
 * WebView persists localStorage in the app-data dir).
 *
 * The read/write/resolve functions are pure (storage is injected) so they can
 * be unit-tested under the node test environment, where `localStorage` is
 * absent. `defaultSortStorage()` returns the real store when reachable, else
 * null (callers degrade to in-component state).
 */

export type SortDir = "asc" | "desc";
export type StoredSort = { key: string; dir: SortDir };

export interface SortStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

const PREFIX = "trackdev-grader.sort.";

export function parseStoredSort(raw: string | null): StoredSort | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as unknown;
    if (
      typeof v === "object" &&
      v !== null &&
      typeof (v as { key?: unknown }).key === "string" &&
      ((v as { dir?: unknown }).dir === "asc" || (v as { dir?: unknown }).dir === "desc")
    ) {
      return { key: (v as StoredSort).key, dir: (v as StoredSort).dir };
    }
  } catch {
    // Corrupt value — treat as absent.
  }
  return null;
}

export function readStoredSort(storage: SortStorage | null, id: string): StoredSort | null {
  if (!storage) return null;
  try {
    return parseStoredSort(storage.getItem(PREFIX + id));
  } catch {
    return null;
  }
}

export function writeStoredSort(storage: SortStorage | null, id: string, sort: StoredSort): void {
  if (!storage) return;
  try {
    storage.setItem(PREFIX + id, JSON.stringify(sort));
  } catch {
    // Quota or unavailable storage — remembering the sort is best-effort.
  }
}

/**
 * Pick the starting sort: a stored sort whose column still exists wins, else
 * the caller's `defaultSort`, else the first column ascending. The column-key
 * guard prevents a stale stored key (column removed/renamed) from silently
 * disabling sorting.
 */
export function resolveInitialSort(
  stored: StoredSort | null,
  defaultSort: StoredSort | undefined,
  validKeys: readonly string[],
): StoredSort {
  if (stored && validKeys.includes(stored.key)) return stored;
  if (defaultSort) return defaultSort;
  return { key: validKeys[0] ?? "", dir: "asc" };
}

/** The real localStorage when reachable (Tauri WebView / browser); null under node tests. */
export function defaultSortStorage(): SortStorage | null {
  try {
    if (typeof localStorage !== "undefined") return localStorage;
  } catch {
    // Access to localStorage can throw in sandboxed contexts.
  }
  return null;
}
