import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";

import type { GradeOutput, GradeSpec, RawProject } from "../data/types";
import { freeVarsFromExpr } from "./expr";
import {
  projectKnownScope,
  RAW_SCOPE,
  STRUCTURAL_SCOPE,
  STUDENT_STRUCTURAL,
  studentKnownScope,
  TASK_SCOPE,
  taskKnownScope,
  V2_AXIS_SCOPE,
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
  const constantNames = (spec.constants ?? []).map((c) => c.name);
  const manualNames = (spec.manual_fields?.defs ?? []).map((d) => d.name);

  const constErr = lintConstants(spec, weightKeys, manualNames);
  if (constErr) return constErr;

  const manualErr = lintManualFields(spec, weightKeys, constantNames);
  if (manualErr) return manualErr;

  const taskErr = lintFormulaGroup(
    spec.formulas?.task ?? [],
    taskKnownScope(weightKeys, constantNames),
    "task",
  );
  if (taskErr) return taskErr;

  const projectNames: string[] = [];
  const projectErr = lintFormulaGroupOrdered(
    spec.formulas?.project ?? [],
    projectKnownScope(weightKeys, manualNames, constantNames),
    "project",
    projectNames,
  );
  if (projectErr) return projectErr;

  const studentErr = lintFormulaGroupOrdered(
    spec.formulas?.student ?? [],
    studentKnownScope(weightKeys, projectNames, manualNames, constantNames),
    "student",
    [],
  );
  if (studentErr) return studentErr;

  return { ok: true };
}

const IDENTIFIER_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * Lint manual-field definitions: each name must be a valid identifier, unique,
 * and must not collide with a weight, a scope variable, or a formula name —
 * because field names are injected into project scope as formula variables.
 */
function lintManualFields(
  spec: GradeSpec,
  weightKeys: string[],
  constantNames: string[],
): ValidationResult | null {
  const defs = spec.manual_fields?.defs ?? [];
  if (defs.length === 0) return null;

  const formulaNames = [
    ...(spec.formulas?.task ?? []),
    ...(spec.formulas?.project ?? []),
    ...(spec.formulas?.student ?? []),
  ].map((f) => f.name);

  const reserved = new Set<string>([
    ...weightKeys,
    ...constantNames,
    ...RAW_SCOPE,
    ...STRUCTURAL_SCOPE,
    ...STUDENT_STRUCTURAL,
    ...TASK_SCOPE,
    ...formulaNames,
  ]);

  const seen = new Set<string>();
  for (const d of defs) {
    if (!IDENTIFIER_RE.test(d.name)) {
      return {
        ok: false,
        message: `Manual field name '${d.name}' is not a valid identifier (letters, digits, underscore; cannot start with a digit)`,
      };
    }
    if (seen.has(d.name)) {
      return { ok: false, message: `Duplicate manual field name '${d.name}'` };
    }
    if (reserved.has(d.name)) {
      return {
        ok: false,
        message: `Manual field name '${d.name}' is reserved (collides with a weight, constant, scope variable, or formula name)`,
      };
    }
    seen.add(d.name);
  }
  return null;
}

/**
 * Lint global constant definitions: each name must be a valid identifier,
 * unique, and must not collide with a weight, scope variable, manual field, or
 * formula name — constants are injected into every formula scope as variables.
 */
function lintConstants(
  spec: GradeSpec,
  weightKeys: string[],
  manualNames: string[],
): ValidationResult | null {
  const consts = spec.constants ?? [];
  if (consts.length === 0) return null;

  const formulaNames = [
    ...(spec.formulas?.task ?? []),
    ...(spec.formulas?.project ?? []),
    ...(spec.formulas?.student ?? []),
  ].map((f) => f.name);

  const reserved = new Set<string>([
    ...weightKeys,
    ...manualNames,
    ...RAW_SCOPE,
    ...STRUCTURAL_SCOPE,
    ...STUDENT_STRUCTURAL,
    ...TASK_SCOPE,
    ...V2_AXIS_SCOPE,
    ...formulaNames,
  ]);

  const seen = new Set<string>();
  for (const c of consts) {
    if (!IDENTIFIER_RE.test(c.name)) {
      return {
        ok: false,
        message: `Constant name '${c.name}' is not a valid identifier (letters, digits, underscore; cannot start with a digit)`,
      };
    }
    if (seen.has(c.name)) {
      return { ok: false, message: `Duplicate constant name '${c.name}'` };
    }
    if (reserved.has(c.name)) {
      return {
        ok: false,
        message: `Constant name '${c.name}' is reserved (collides with a weight, scope variable, manual field, or formula name)`,
      };
    }
    seen.add(c.name);
  }
  return null;
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
