/** Known variable sets for formula scope lint (Phase 4). */

/** Documented weight keys (v2 + legacy v1 names for lint hints). */
export const WEIGHT_KEYS = [
  "w_quality",
  "w_complexity",
  "w_size",
  "w_android",
  "w_spring",
  "w_mi",
  "w_arch",
  "w_density",
  "w_mutation",
  "w_doc",
  "w_cq",
  "w_surv",
  "ai_strength",
  "floor_keep",
  "undeclared_model_m",
  "undeclared_level_l",
  "student_penalty_cap",
  "crit_flag_points",
  "mi_floor",
  "mi_ceiling",
  "arch_norm",
  "density_ceiling",
  "inventory_count_ceiling",
  "inventory_depth_ceiling",
  "inventory_density_ceiling",
  "prod_loc_ceiling",
  "prod_stmt_ceiling",
  "quality_floor",
  "quality_blend",
  // EXTRA_TECH aggregate weights/cap (default 0-impact until referenced).
  "w_extra_dep",
  "w_fcm_spring",
  "w_fcm_android",
  "w_spec",
  "w_email",
  "w_graphics",
  "w_av",
  "extra_tech_cap",
] as const;

/** Injected by grade_cohort before project formulas (Grading v3). */
export const V2_AXIS_SCOPE = [
  "quality",
  "complexity",
  "size",
  "work_base",
  "quality_eff",
  "quality_multiplier",
  "quality_present",
  "complexity_present",
  "size_present",
  "work_base_present",
  // EXTRA_TECH aggregate injected before project formulas (mirror of grade.rs).
  "extra_tech",
] as const;

export const RAW_SCOPE = [
  "documentation_raw",
  "code_quality_raw",
  "cc_pct",
  "mutation_score",
  "survival_raw",
  "arch_crit_count",
  "arch_warn_count",
  "doc_present",
  "cq_present",
  "surv_present",
  "arch_present",
  "team_size",
] as const;

export const TASK_SCOPE = ["raw_points", "model_m", "level_l", "declared"] as const;

export const STRUCTURAL_SCOPE = [
  "sum_raw",
  "sum_eff",
  "mean_raw",
  "ai_factor",
  "crit_sa_count",
  "crit_security_count",
  "crit_cx_count",
  "penalty_on",
] as const;

export const STUDENT_STRUCTURAL = [
  "student_eff",
  "ai_keep",
  "contribution",
  "student_contribution",
  "student_critical_count",
  // v4: cohort-percentile code-quality penalty injected per student by
  // grade_cohort (mirror of grade.rs). Usable in student formulas.
  "codequality_penalty",
] as const;

export function taskKnownScope(
  weightKeys: string[],
  constantNames: string[] = [],
): Set<string> {
  return new Set([...weightKeys, ...constantNames, ...TASK_SCOPE]);
}

export function projectKnownScope(
  weightKeys: string[],
  manualNames: string[] = [],
  constantNames: string[] = [],
): Set<string> {
  return new Set([
    ...weightKeys,
    ...constantNames,
    ...RAW_SCOPE,
    ...STRUCTURAL_SCOPE,
    ...V2_AXIS_SCOPE,
    ...manualNames,
  ]);
}

export function studentKnownScope(
  weightKeys: string[],
  projectFormulaNames: string[],
  manualNames: string[] = [],
  constantNames: string[] = [],
): Set<string> {
  return new Set([
    ...weightKeys,
    ...constantNames,
    ...RAW_SCOPE,
    ...STRUCTURAL_SCOPE,
    ...V2_AXIS_SCOPE,
    ...STUDENT_STRUCTURAL,
    ...projectFormulaNames,
    ...manualNames,
  ]);
}
