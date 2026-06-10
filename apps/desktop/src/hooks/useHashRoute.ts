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

export function projectHref(projectId: number): string {
  return `#/projects/${projectId}`;
}

export function studentHref(projectId: number, studentId: string): string {
  return `#/students/${projectId}/${encodeURIComponent(studentId)}`;
}
