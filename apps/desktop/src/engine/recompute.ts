import type { GradeOutput, GradeSpec, RawProject } from "../data/types";
import { initEngine, recompute } from "./index";

export type RecomputeFrom = "aggregate" | "project" | "student";

export type RecomputeResult = {
  grades: Map<number, GradeOutput>;
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
  await initEngine();
  try {
    const grades = new Map<number, GradeOutput>();
    for (const raw of projects) {
      const out = (await recompute(raw, spec)) as GradeOutput;
      if (!("grades" in out)) {
        throw new Error("expected full GradeOutput from WASM grade()");
      }
      grades.set(raw.project_id, out);
    }
    lastGoodGrades = grades;
    return { grades, error: null };
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return {
      grades: lastGoodGrades ?? new Map(),
      error: message,
    };
  }
}

/**
 * Staged recompute entry point. Phase 4 uses full `grade()` for all stages;
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
