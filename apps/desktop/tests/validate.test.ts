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

describe("validateSpec — manual fields", () => {
  function withField(name: string, value = 0): GradeSpec {
    const spec = structuredClone(loadBundledDefault());
    spec.manual_fields = { defs: [{ name, value, description: "" }], values: {} };
    return spec;
  }

  it("accepts a defined manual field referenced in a student formula", () => {
    const spec = withField("oral_presentation");
    const base = spec.formulas.student.find((f) => f.name === "student_base")!;
    base.expr = {
      op: "mul",
      factors: [
        { op: "var", name: "student_eff" },
        { op: "var", name: "oral_presentation" },
      ],
    };
    expect(validateSpec(spec).ok).toBe(true);
  });

  it("accepts a defined manual field referenced in a project formula", () => {
    const spec = withField("team_bonus");
    spec.formulas.project[0].expr = { op: "var", name: "team_bonus" };
    expect(validateSpec(spec).ok).toBe(true);
  });

  it("rejects a manual field referenced in a task formula (project scope only)", () => {
    const spec = withField("oral_presentation");
    spec.formulas.task[0].expr = { op: "var", name: "oral_presentation" };
    const result = validateSpec(spec);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("oral_presentation");
  });

  it("rejects a field name that collides with a weight", () => {
    const result = validateSpec(withField("w_doc"));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("reserved");
  });

  it("rejects a field name that collides with a structural variable", () => {
    const result = validateSpec(withField("mean_raw"));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("reserved");
  });

  it("rejects a field name that collides with a formula output name", () => {
    const result = validateSpec(withField("quality_composite"));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("reserved");
  });

  it("rejects an invalid identifier", () => {
    // Caught by the schema `pattern` and/or the lint's identifier check.
    for (const bad of ["2bad", "has space", "with-dash", ""]) {
      const result = validateSpec(withField(bad));
      expect(result.ok).toBe(false);
      if (!result.ok) expect(result.message).toMatch(/identifier|pattern/);
    }
  });

  it("rejects duplicate field names", () => {
    const spec = structuredClone(loadBundledDefault());
    spec.manual_fields = {
      defs: [
        { name: "oral", value: 0, description: "" },
        { name: "oral", value: 1, description: "" },
      ],
      values: {},
    };
    const result = validateSpec(spec);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("Duplicate");
  });
});
