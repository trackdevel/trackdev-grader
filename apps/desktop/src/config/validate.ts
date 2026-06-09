import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";

import type { GradeOutput, GradeSpec, RawProject } from "../data/types";
import { freeVarsFromExpr } from "./expr";
import {
  projectKnownScope,
  studentKnownScope,
  taskKnownScope,
} from "./scopes";
import gradingSchema from "../../config/grading.schema.json";

export type ValidationResult =
  | { ok: true }
  | { ok: false; message: string; formula?: string };

type FormulaDef = { name: string; infix: string; expr: unknown };

const ajv = new Ajv2020({ allErrors: true, strict: false });
addFormats(ajv);
const validateSchema = ajv.compile(gradingSchema);

/** Canonical JSON for stable hash/compare. */
export function canonicalSpecJson(spec: GradeSpec): string {
  return JSON.stringify(spec, Object.keys(spec).sort());
}

export function validateSpec(spec: GradeSpec): ValidationResult {
  if (!validateSchema(spec)) {
    const detail = validateSchema.errors?.[0];
    const path = detail?.instancePath || detail?.schemaPath || "";
    return {
      ok: false,
      message: `Schema: ${detail?.message ?? "invalid spec"}${path ? ` at ${path}` : ""}`,
    };
  }

  const weightKeys = Object.keys(spec.weights ?? {});

  const taskErr = lintFormulaGroup(
    spec.formulas?.task ?? [],
    taskKnownScope(weightKeys),
    "task",
  );
  if (taskErr) return taskErr;

  const projectNames: string[] = [];
  const projectErr = lintFormulaGroupOrdered(
    spec.formulas?.project ?? [],
    projectKnownScope(weightKeys),
    "project",
    projectNames,
  );
  if (projectErr) return projectErr;

  const studentErr = lintFormulaGroupOrdered(
    spec.formulas?.student ?? [],
    studentKnownScope(weightKeys, projectNames),
    "student",
    [],
  );
  if (studentErr) return studentErr;

  return { ok: true };
}

function lintFormulaGroup(
  formulas: FormulaDef[],
  known: Set<string>,
  group: string,
): ValidationResult | null {
  const names = new Set<string>();
  for (const fd of formulas) {
    if (names.has(fd.name)) {
      return { ok: false, message: `Duplicate formula name '${fd.name}' in ${group}`, formula: fd.name };
    }
    names.add(fd.name);
    const vars = freeVarsFromExpr(fd.expr);
    for (const v of vars) {
      if (!known.has(v)) {
        return {
          ok: false,
          message: `Unknown variable '${v}' in ${group}.${fd.name}`,
          formula: fd.name,
        };
      }
    }
  }
  return null;
}

/** Forward-reference lint: each formula may only reference names defined earlier in the group. */
function lintFormulaGroupOrdered(
  formulas: FormulaDef[],
  baseKnown: Set<string>,
  group: string,
  definedOut: string[],
): ValidationResult | null {
  const known = new Set(baseKnown);
  const names = new Set<string>();
  for (const fd of formulas) {
    if (names.has(fd.name)) {
      return { ok: false, message: `Duplicate formula name '${fd.name}' in ${group}`, formula: fd.name };
    }
    const vars = freeVarsFromExpr(fd.expr);
    for (const v of vars) {
      if (!known.has(v)) {
        const cycleHint = names.has(v)
          ? " (forward reference or cycle)"
          : "";
        return {
          ok: false,
          message: `Unknown variable '${v}' in ${group}.${fd.name}${cycleHint}`,
          formula: fd.name,
        };
      }
    }
    names.add(fd.name);
    known.add(fd.name);
    definedOut.push(fd.name);
  }
  return null;
}

/** Dry-run grade on a probe project; surfaces EvalError from WASM. */
export async function dryRunSpec(
  spec: GradeSpec,
  probe: RawProject,
  gradeFn: (raw: RawProject, spec: GradeSpec) => Promise<GradeOutput>,
): Promise<ValidationResult> {
  const structural = validateSpec(spec);
  if (!structural.ok) return structural;
  try {
    await gradeFn(probe, spec);
    return { ok: true };
  } catch (e) {
    return {
      ok: false,
      message: e instanceof Error ? e.message : String(e),
    };
  }
}

export function isEditedSpec(spec: GradeSpec, bundledDefault: GradeSpec): boolean {
  return JSON.stringify(spec) !== JSON.stringify(bundledDefault);
}
