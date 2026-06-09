/** Known variable sets for formula scope lint (Phase 4). */

export const WEIGHT_KEYS = [
  "w_doc",
  "w_cq",
  "w_surv",
  "w_arch",
  "ai_strength",
  "floor_keep",
  "undeclared_model_m",
  "undeclared_level_l",
  "max_penalty_points",
  "student_penalty_cap",
  "crit_sa_points",
  "crit_cx_points",
  "crit_flag_points",
  "security_extra",
  "doc_max",
  "mi_floor",
  "mi_ceiling",
  "cc_penalty",
  "test_bonus",
  "test_cap",
  "surv_floor",
  "surv_ceiling",
  "k_crit",
  "k_warn",
  "arch_norm",
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
  "student_critical_count",
] as const;

export function taskKnownScope(weightKeys: string[]): Set<string> {
  return new Set([...weightKeys, ...TASK_SCOPE]);
}

export function projectKnownScope(weightKeys: string[]): Set<string> {
  return new Set([...weightKeys, ...RAW_SCOPE, ...STRUCTURAL_SCOPE]);
}

export function studentKnownScope(
  weightKeys: string[],
  projectFormulaNames: string[],
): Set<string> {
  return new Set([
    ...weightKeys,
    ...RAW_SCOPE,
    ...STRUCTURAL_SCOPE,
    ...STUDENT_STRUCTURAL,
    ...projectFormulaNames,
  ]);
}
