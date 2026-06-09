import referenceGrades from "@repo-fixtures/reference.grades.json";

import type { GradeOutput, GradeSpec } from "../data/types";
import { isEditedSpec } from "../config/validate";
import { loadBundledDefault } from "../config/load";

export type ParityState = "standard" | "edited" | "broken" | "unchecked";

export type ParityResult = {
  state: ParityState;
  message: string;
  maxDelta: number;
  offenderCount: number;
};

type RefEntry = {
  project: { project_id: number; final_grade: number; quality_grade: number; ai_factor: number };
  students: Array<{ student_id: string; final_grade: number }>;
};

const REFERENCE: RefEntry[] = referenceGrades as RefEntry[];

function halfUlp(decimals: number): number {
  return 0.5 * 10 ** -decimals;
}

export function checkParity(
  spec: GradeSpec,
  grades: Map<number, GradeOutput>,
  bundledDefault: GradeSpec = loadBundledDefault(),
): ParityResult {
  if (isEditedSpec(spec, bundledDefault)) {
    return {
      state: "edited",
      message:
        "● Spec edited — live what-if grades, not the standard bundled spec. Reset to restore defaults.",
      maxDelta: 0,
      offenderCount: 0,
    };
  }

  const tol = halfUlp(spec.meta.decimals ?? 2);
  let maxDelta = 0;
  let offenderCount = 0;
  let checked = 0;

  for (const ref of REFERENCE) {
    const pid = ref.project.project_id;
    const out = grades.get(pid);
    if (!out) continue;
    checked += 1;

    const projectBad =
      Math.abs(out.grades.project_final - ref.project.final_grade) > tol ||
      Math.abs(out.grades.quality_grade - ref.project.quality_grade) > tol;

    let studentBad = false;
    for (const expStu of ref.students) {
      const stu = out.grades.students.find((s) => s.student_id === expStu.student_id);
      const d = stu ? Math.abs(stu.student_final - expStu.final_grade) : tol + 1;
      maxDelta = Math.max(maxDelta, d);
      if (d > tol) studentBad = true;
    }
    const pDelta = Math.abs(out.grades.project_final - ref.project.final_grade);
    const qDelta = Math.abs(out.grades.quality_grade - ref.project.quality_grade);
    maxDelta = Math.max(maxDelta, pDelta, qDelta);

    if (projectBad || studentBad) offenderCount += 1;
  }

  if (checked === 0) {
    return {
      state: "unchecked",
      message:
        "✓ Standard spec — parity not checked (load reference.db projects 1–4 to verify).",
      maxDelta: 0,
      offenderCount: 0,
    };
  }

  if (offenderCount > 0) {
    return {
      state: "broken",
      message: `⚠ Parity broken at standard spec — ${offenderCount} value(s) exceed 0.5·10⁻${spec.meta.decimals ?? 2} (max Δ ${maxDelta.toExponential(2)}).`,
      maxDelta,
      offenderCount,
    };
  }

  return {
    state: "standard",
    message: `✓ Parity verified — grades match reference fixture within 0.5·10⁻${spec.meta.decimals ?? 2} (max Δ ${maxDelta.toExponential(2)}).`,
    maxDelta,
    offenderCount: 0,
  };
}
