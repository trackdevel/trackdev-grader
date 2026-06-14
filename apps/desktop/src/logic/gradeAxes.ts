import type { AxisGrade } from "../data/types";

/** Present axis score, or null when absent / missing. */
export function axisScore(axes: AxisGrade[], key: string): number | null {
  const a = axes.find((x) => x.key === key);
  if (!a?.present || a.score == null) return null;
  return a.score;
}

/** Grading v3: neutral 10 when quality axis is absent. */
export function qualityEff(axes: AxisGrade[]): number | null {
  if (!axes.some((a) => a.key === "work_base")) {
    return null;
  }
  const q = axes.find((a) => a.key === "quality");
  if (q?.present && q.score != null) return q.score;
  return 10;
}
