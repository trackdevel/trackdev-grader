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
    bad.formulas.project[0].expr = { op: "var", name: "student_final" };
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
    expect(roundTripped.weights.w_quality).toBe(spec.weights.w_quality);
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
    const result = validateSpec(withField("w_quality"));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("reserved");
  });

  it("rejects a field name that collides with a structural variable", () => {
    const result = validateSpec(withField("mean_raw"));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("reserved");
  });

  it("rejects a field name that collides with a formula output name", () => {
    const result = validateSpec(withField("project_final"));
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

  it("accepts per-project multiline explanation notes alongside values", () => {
    const spec = structuredClone(loadBundledDefault());
    spec.manual_fields = {
      defs: [{ name: "oral", value: 0, description: "" }],
      values: { "1": { oral: 5 } },
      notes: { "1": { oral: "Strong defense.\nSecond line of reasoning." } },
    };
    expect(validateSpec(spec).ok).toBe(true);
  });
});

describe("validateSpec — constants", () => {
  function withConstant(name: string, value = 1): GradeSpec {
    const spec = structuredClone(loadBundledDefault());
    spec.constants = [{ name, value, description: "" }];
    return spec;
  }

  it("accepts a constant referenced in a task formula", () => {
    const spec = withConstant("frontend_weight", 0.5);
    spec.formulas.task[0].expr = { op: "var", name: "frontend_weight" };
    expect(validateSpec(spec).ok).toBe(true);
  });

  it("accepts a constant referenced in project and student formulas", () => {
    const spec = withConstant("extra_credit", 1.1);
    spec.formulas.project[0].expr = { op: "var", name: "extra_credit" };
    const base = spec.formulas.student.find((f) => f.name === "student_base")!;
    base.expr = {
      op: "mul",
      factors: [
        { op: "var", name: "student_eff" },
        { op: "var", name: "extra_credit" },
      ],
    };
    expect(validateSpec(spec).ok).toBe(true);
  });

  it("rejects a constant name that collides with a weight", () => {
    const result = validateSpec(withConstant("w_quality"));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("reserved");
  });

  it("rejects a constant that collides with a manual field", () => {
    const spec = withConstant("shared_name");
    spec.manual_fields = {
      defs: [{ name: "shared_name", value: 0, description: "" }],
      values: {},
    };
    const result = validateSpec(spec);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("reserved");
  });

  it("rejects an invalid constant identifier", () => {
    for (const bad of ["2bad", "has space", "with-dash"]) {
      const result = validateSpec(withConstant(bad));
      expect(result.ok).toBe(false);
      if (!result.ok) expect(result.message).toMatch(/identifier|pattern/);
    }
  });

  it("rejects duplicate constant names", () => {
    const spec = structuredClone(loadBundledDefault());
    spec.constants = [
      { name: "k", value: 1, description: "" },
      { name: "k", value: 2, description: "" },
    ];
    const result = validateSpec(spec);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.message).toContain("Duplicate");
  });
});
