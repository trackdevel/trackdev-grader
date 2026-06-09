import type { ProjectDiagnostics } from "../data/diagnostics";
import type { RawProject, RawTask } from "../data/types";

const LEVEL_ORDER = ["A", "B", "C", "D", "E"] as const;
const AI_DETECT_RISK = "HIGH";
const LOW_DECLARED_LEVELS = new Set(["A", "B"]);

export function maxDeclaredLevel(tasks: RawTask[], studentId: string): string | null {
  let best: number | null = null;
  for (const t of tasks) {
    if (t.assignee_id !== studentId || !t.declared || !t.ai_level) continue;
    const idx = LEVEL_ORDER.indexOf(t.ai_level as (typeof LEVEL_ORDER)[number]);
    if (idx >= 0) {
      best = best === null ? idx : Math.max(best, idx);
    }
  }
  return best === null ? null : LEVEL_ORDER[best];
}

export function studentHasHighAiDetect(
  diagnostics: ProjectDiagnostics,
  studentId: string,
): boolean {
  return diagnostics.aiDetect.some(
    (r) => r.student_id === studentId && r.risk_level === AI_DETECT_RISK,
  );
}

export function projectReviewGate(diagnostics: ProjectDiagnostics): string | null {
  return diagnostics.plagiarism ? "PLAGIARISM" : null;
}

export function studentReviewGate(
  raw: RawProject,
  diagnostics: ProjectDiagnostics,
  studentId: string,
  effectivePoints: number,
): string | null {
  const projectGate = projectReviewGate(diagnostics);
  if (projectGate) return projectGate;
  if (effectivePoints <= 0) return "NO_DELIVERY";
  if (
    studentHasHighAiDetect(diagnostics, studentId) &&
    (maxDeclaredLevel(raw.tasks, studentId) === null ||
      LOW_DECLARED_LEVELS.has(maxDeclaredLevel(raw.tasks, studentId) ?? ""))
  ) {
    return "AI_REVIEW";
  }
  return null;
}
