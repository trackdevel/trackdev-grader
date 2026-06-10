import type { CohortGradeOutput, GradeOutput, GradeSpec, RawProject } from "../data/types";

export type RecomputeFrom = "aggregate" | "project" | "student";

export type RecomputeResult = {
  grades: Map<number, GradeOutput>;
  cohort: CohortGradeOutput | null;
  error: string | null;
};

let lastGoodGrades: Map<number, GradeOutput> | null = null;

export function getLastGoodGrades(): Map<number, GradeOutput> | null {
  return lastGoodGrades;
}

/** Full recompute for all projects; retains last-good grades on failure. */
export async function recomputeAll(
  projects: RawProject[],
  spec: GradeSpec,
): Promise<RecomputeResult> {
  try {
    const { initEngine, recomputeCohort } = await import("./index");
    await initEngine();
    const cohort = await recomputeCohort(projects, spec);
    const grades = new Map<number, GradeOutput>();
    for (const entry of cohort.projects) {
      grades.set(entry.project_id, entry.output);
    }
    lastGoodGrades = grades;
    return { grades, cohort, error: null };
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return {
      grades: lastGoodGrades ?? new Map(),
      cohort: null,
      error: message,
    };
  }
}

/**
 * Staged recompute entry point. Uses `grade_cohort` for the full batch;
 * `from` is reserved for future partial re-evaluation optimizations.
 */
export async function recomputeFrom(
  projects: RawProject[],
  spec: GradeSpec,
  from: RecomputeFrom = "aggregate",
): Promise<RecomputeResult> {
  void from;
  return recomputeAll(projects, spec);
}

export function clearLastGoodGrades(): void {
  lastGoodGrades = null;
}
