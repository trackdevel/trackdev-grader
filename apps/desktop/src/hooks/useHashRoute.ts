import { useCallback, useMemo, useSyncExternalStore } from "react";

/**
 * Hash routes, organised under three top-level tabs:
 *   #/students                          → student list
 *   #/students/<projectId>/<studentId>  → student detail
 *   #/projects                          → project list
 *   #/projects/<projectId>              → project detail
 *   #/formula                           → formula tree + custom fields
 *
 * Legacy aliases (#/student/…, #/project/…, #/formulas-and-custom-fields)
 * still parse so old links keep working.
 */
export type AppRoute =
  | { page: "students" }
  | { page: "projects" }
  | { page: "formula" }
  | { page: "student"; projectId: number; studentId: string }
  | { page: "project"; projectId: number };

export type TopTab = "students" | "projects" | "formula";

export function topTabOf(route: AppRoute): TopTab {
  switch (route.page) {
    case "student":
      return "students";
    case "project":
      return "projects";
    default:
      return route.page;
  }
}

export function parseHash(hash: string): AppRoute {
  const parts = hash
    .replace(/^#\/?/, "")
    .split("/")
    .filter(Boolean);
  const [head, a, b] = parts;
  switch (head) {
    case undefined:
    case "students":
    case "student": {
      if (a !== undefined && b !== undefined) {
        const projectId = Number(a);
        if (Number.isFinite(projectId)) {
          return { page: "student", projectId, studentId: decodeURIComponent(b) };
        }
      }
      return { page: "students" };
    }
    case "projects":
    case "project": {
      if (a !== undefined) {
        const projectId = Number(a);
        if (Number.isFinite(projectId)) return { page: "project", projectId };
      }
      return { page: "projects" };
    }
    case "formula":
    case "formulas-and-custom-fields":
      return { page: "formula" };
    default:
      return { page: "students" };
  }
}

function subscribe(onChange: () => void): () => void {
  window.addEventListener("hashchange", onChange);
  return () => window.removeEventListener("hashchange", onChange);
}

function getSnapshot(): string {
  return window.location.hash;
}

function getServerSnapshot(): string {
  return "";
}

export function useHashRoute(): { route: AppRoute; navigate: (hash: string) => void } {
  const hash = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
  const route = useMemo(() => parseHash(hash), [hash]);
  const navigate = useCallback((next: string) => {
    window.location.hash = next;
  }, []);
  return { route, navigate };
}

// ---- Navigation history (true back button) ----
//
// The WebView already keeps a full back/forward stack across hash changes, so
// `history.back()` returns to the *exact* previous page. The only thing we have
// to track ourselves is whether there is anywhere to go back to. We stamp each
// history entry with a monotonic depth in `history.state`: a fresh forward
// navigation (anchor click or `location.hash =`) lands unstamped, so we advance
// the depth; a back/forward navigation restores a previously stamped entry, so
// we adopt its depth. `canGoBack` is "are we past the entry we started on".

const NAV_STATE_KEY = "navDepth";

/** Pure transition used by both the live store and the tests. */
export function nextNavStep(
  prevDepth: number,
  stampedDepth: number | null,
): { depth: number; canGoBack: boolean } {
  const depth = stampedDepth == null ? prevDepth + 1 : stampedDepth;
  return { depth, canGoBack: depth > 0 };
}

let navDepth = 0;
let navCanGoBack = false;
let navInitialised = false;
const navListeners = new Set<() => void>();

function readStampedDepth(): number | null {
  const st = window.history.state as Record<string, unknown> | null;
  const d = st?.[NAV_STATE_KEY];
  return typeof d === "number" ? d : null;
}

function stampDepth(depth: number): void {
  const st = (window.history.state as Record<string, unknown> | null) ?? {};
  window.history.replaceState({ ...st, [NAV_STATE_KEY]: depth }, "");
}

function emitNav(): void {
  for (const listener of navListeners) listener();
}

function onNavHashChange(): void {
  const stamped = readStampedDepth();
  const step = nextNavStep(navDepth, stamped);
  navDepth = step.depth;
  navCanGoBack = step.canGoBack;
  // Only stamp on a fresh forward entry; back/forward already carry their stamp.
  if (stamped == null) stampDepth(navDepth);
  emitNav();
}

function ensureNavInit(): void {
  if (navInitialised) return;
  navInitialised = true;
  const stamped = readStampedDepth();
  navDepth = stamped ?? 0;
  navCanGoBack = navDepth > 0;
  if (stamped == null) stampDepth(navDepth);
  window.addEventListener("hashchange", onNavHashChange);
}

function subscribeNav(onChange: () => void): () => void {
  ensureNavInit();
  navListeners.add(onChange);
  return () => navListeners.delete(onChange);
}

function getNavSnapshot(): boolean {
  return navCanGoBack;
}

function getNavServerSnapshot(): boolean {
  return false;
}

export function useNavHistory(): {
  canGoBack: boolean;
  goBack: (fallbackHash?: string) => void;
} {
  const canGoBack = useSyncExternalStore(subscribeNav, getNavSnapshot, getNavServerSnapshot);
  const goBack = useCallback((fallbackHash?: string) => {
    if (navCanGoBack) {
      window.history.back();
    } else if (fallbackHash) {
      window.location.hash = fallbackHash;
    }
  }, []);
  return { canGoBack, goBack };
}

export function projectHref(projectId: number): string {
  return `#/projects/${projectId}`;
}

export function studentHref(projectId: number, studentId: string): string {
  return `#/students/${projectId}/${encodeURIComponent(studentId)}`;
}
