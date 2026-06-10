import type {
  CohortGradeOutput,
  GradeOutput,
  GradeSpec,
  RawProject,
  StructuralOutput,
  StructuralSpec,
} from "../data/types";

type WasmModule = {
  default: (input?: unknown) => Promise<unknown>;
  initSync?: (module: Buffer | WebAssembly.Module) => unknown;
  grade: (rawJson: string, specJson: string) => GradeOutput | StructuralOutput;
  grade_cohort: (projectsJson: string, specJson: string) => CohortGradeOutput;
  structural_scopes: (rawJson: string, specJson: string) => StructuralOutput;
  free_vars: (exprJson: string) => string[];
};

let wasm: WasmModule | null = null;

/** Load the grade_core WASM bundle once (browser / Vite dev server). */
export async function initEngine(): Promise<void> {
  if (wasm) return;
  const mod = (await import("../../pkg/grade_core_wasm.js")) as WasmModule;
  await mod.default();
  wasm = mod;
}

/** Vitest/Node: pass pre-read WASM bytes when `fetch(file://…)` is unavailable. */
export async function initEngineWithBytes(bytes: Buffer): Promise<void> {
  if (wasm) return;
  const mod = (await import("../../pkg/grade_core_wasm.js")) as WasmModule;
  if (!mod.initSync) {
    throw new Error("grade_core_wasm.js missing initSync — rebuild the pkg bundle");
  }
  mod.initSync(bytes);
  wasm = mod;
}

export function toRawProject(projection: RawProject): RawProject {
  return structuredClone(projection);
}

/** Full formula grade for one project (legacy path; prefer `recomputeCohort`). */
export async function recompute(
  raw: RawProject,
  spec: GradeSpec | StructuralSpec,
): Promise<GradeOutput | StructuralOutput> {
  if (!wasm) await initEngine();
  if (!wasm) throw new Error("WASM engine not initialized");
  return wasm.grade(JSON.stringify(raw), JSON.stringify(spec));
}

/** Cohort batch grade: shared hybrid bounds + per-project grades (Grading v2 Phase 2). */
export async function recomputeCohort(
  projects: RawProject[],
  spec: GradeSpec,
): Promise<CohortGradeOutput> {
  if (!wasm) await initEngine();
  if (!wasm) throw new Error("WASM engine not initialized");
  return wasm.grade_cohort(JSON.stringify(projects), JSON.stringify(spec));
}

/** Phase 2 structural path — scopes only, independent of formula staging. */
export async function recomputeStructural(
  raw: RawProject,
  spec: StructuralSpec,
): Promise<StructuralOutput> {
  if (!wasm) await initEngine();
  if (!wasm) throw new Error("WASM engine not initialized");
  return wasm.structural_scopes(JSON.stringify(raw), JSON.stringify(spec));
}

export async function recomputeStructuralProjects(
  projects: RawProject[],
  spec: StructuralSpec,
): Promise<StructuralOutput[]> {
  const out: StructuralOutput[] = [];
  for (const p of projects) {
    out.push(await recomputeStructural(p, spec));
  }
  return out;
}

export function extractFreeVars(exprJson: string): string[] {
  if (!wasm) throw new Error("WASM engine not initialized");
  return wasm.free_vars(exprJson);
}

export { clearLastGoodGrades, getLastGoodGrades, recomputeAll, recomputeFrom } from "./recompute";
export type { RecomputeFrom, RecomputeResult } from "./recompute";
export { recomputeCohort };
