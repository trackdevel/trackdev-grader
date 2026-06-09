import { describe, expect, it } from "vitest";

import { freeVarsFromExpr } from "../src/config/expr";
import { loadBundledDefault, parseSpecJson } from "../src/config/load";
import { validateSpec } from "../src/config/validate";
import type { GradeSpec } from "../src/data/types";

describe("validateSpec", () => {
  it("accepts bundled default spec", () => {
    const spec = loadBundledDefault();
    const result = validateSpec(spec);
    expect(result.ok).toBe(true);
  });

  it("rejects unknown variable in task formula", () => {
    const spec = loadBundledDefault();
    const bad: GradeSpec = structuredClone(spec);
    bad.formulas.task[0].expr = { op: "var", name: "typo_model_m" };
    const result = validateSpec(bad);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.message).toContain("typo_model_m");
    }
  });

  it("rejects forward reference in project formulas", () => {
    const spec = loadBundledDefault();
    const bad: GradeSpec = structuredClone(spec);
    bad.formulas.project[0].expr = { op: "var", name: "quality_composite" };
    const result = validateSpec(bad);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.formula).toBe(bad.formulas.project[0].name);
    }
  });

  it("rejects forward reference in student formulas", () => {
    const spec = loadBundledDefault();
    const bad: GradeSpec = structuredClone(spec);
    bad.formulas.student[0].expr = { op: "var", name: "student_final" };
    const result = validateSpec(bad);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.message).toContain("student_final");
      expect(result.formula).toBe("student_penalty");
    }
  });

  it("round-trips bundled spec through JSON parse", () => {
    const spec = loadBundledDefault();
    const text = JSON.stringify(spec, null, 2);
    const roundTripped = parseSpecJson(text);
    expect(roundTripped.meta.decimals).toBe(spec.meta.decimals);
    expect(roundTripped.weights.w_doc).toBe(spec.weights.w_doc);
  });

  it("collects free vars from nested expr", () => {
    const vars = freeVarsFromExpr({
      op: "mul",
      factors: [
        { op: "var", name: "w_doc" },
        { op: "add", terms: [{ op: "var", name: "doc_axis" }] },
      ],
    });
    expect(vars.has("w_doc")).toBe(true);
    expect(vars.has("doc_axis")).toBe(true);
  });
});
