import { describe, expect, it } from "vitest";
import { axisScore, qualityEff } from "../src/logic/gradeAxes";
import type { AxisGrade } from "../src/data/types";

const axes: AxisGrade[] = [
  { key: "size", raw: null, score: 5, present: true },
  { key: "complexity", raw: null, score: 7.5, present: true },
  { key: "quality", raw: null, score: 8, present: true },
  { key: "work_base", raw: null, score: 6.5, present: true },
  { key: "quality_multiplier", raw: null, score: 0.85, present: true },
];

describe("gradeAxes", () => {
  it("axisScore returns present scores", () => {
    expect(axisScore(axes, "work_base")).toBe(6.5);
    expect(axisScore(axes, "quality_multiplier")).toBe(0.85);
  });

  it("axisScore returns null when absent", () => {
    expect(axisScore(axes, "missing")).toBeNull();
    expect(axisScore([{ key: "work_base", raw: null, score: 0, present: false }], "work_base")).toBeNull();
  });

  it("qualityEff uses quality when present", () => {
    expect(qualityEff(axes)).toBe(8);
  });

  it("qualityEff is neutral 10 when quality absent but v3 axes exist", () => {
    const noQ = axes.filter((a) => a.key !== "quality");
    expect(qualityEff(noQ)).toBe(10);
  });
});
