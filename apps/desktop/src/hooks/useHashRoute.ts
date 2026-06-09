import { useCallback, useEffect, useState } from "react";

export type AppRoute =
  | { page: "students" }
  | { page: "projects" }
  | { page: "student"; projectId: number; studentId: string }
  | { page: "project"; projectId: number };

function parseHash(hash: string): AppRoute {
  const raw = hash.replace(/^#\/?/, "");
  const parts = raw.split("/").filter(Boolean);
  if (!parts.length || parts[0] === "students") return { page: "students" };
  if (parts[0] === "projects") return { page: "projects" };
  if (parts[0] === "student" && parts.length >= 3) {
    return {
      page: "student",
      projectId: Number(parts[1]),
      studentId: decodeURIComponent(parts[2]),
    };
  }
  if (parts[0] === "project" && parts.length >= 2) {
    return { page: "project", projectId: Number(parts[1]) };
  }
  return { page: "students" };
}

export function useHashRoute() {
  const [route, setRoute] = useState<AppRoute>(() =>
    typeof window !== "undefined" ? parseHash(window.location.hash) : { page: "students" },
  );

  useEffect(() => {
    const onHash = () => setRoute(parseHash(window.location.hash));
    window.addEventListener("hashchange", onHash);
    if (!window.location.hash || window.location.hash === "#") {
      window.location.hash = "#/students";
    }
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  const navigate = useCallback((hash: string) => {
    if (window.location.hash === hash) {
      setRoute(parseHash(hash));
    } else {
      window.location.hash = hash;
    }
  }, []);

  return { route, navigate };
}

export function projectHref(projectId: number): string {
  return `#/project/${projectId}`;
}

export function studentHref(projectId: number, studentId: string): string {
  return `#/student/${projectId}/${encodeURIComponent(studentId)}`;
}
