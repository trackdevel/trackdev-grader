/** Mirrors `grade_core` input structs (serde snake_case). */

import type { ProjectDiagnostics } from "./diagnostics";

export type LoadedDb = {
  path: string;
  projects: RawProject[];
  diagnostics: Map<number, ProjectDiagnostics>;
};

export type AxisInputs = {
  documentation_raw: number;
  doc_present: boolean;
  code_quality_raw: number;
  cc_pct: number;
  mutation_score: number;
  cq_present: boolean;
  survival_raw: number;
  surv_present: boolean;
  arch_crit_count: number;
  arch_warn_count: number;
  arch_present: boolean;
};

export type RawTask = {
  assignee_id: string;
  raw_points: number;
  ai_model: string | null;
  ai_level: string | null;
  declared: boolean;
};

export type RawStudent = {
  student_id: string;
  full_name: string;
};

export type FindingKind = "static_analysis" | "complexity";

export type CritFinding = {
  kind: FindingKind;
  category: string | null;
};

export type StudentFlag = {
  student_id: string;
  severity: string;
  source: string;
  /** Mirror of grade_core StudentFlag (T2.2): partitions behavioural vs hotspot. */
  flag_type: string;
  /** Per-student blame magnitude for the v4 percentile bands; null when absent. */
  weighted: number | null;
};

export type RepoMetrics = {
  repo_full_name: string;
  metrics: Record<string, number>;
};

export type RawProject = {
  project_id: number;
  name: string;
  team_size: number;
  axis: AxisInputs;
  inventory?: RepoMetrics[];
  tasks: RawTask[];
  students: RawStudent[];
  crit_findings: CritFinding[];
  student_flags: StudentFlag[];
};

export type StudentScope = {
  student_id: string;
  student_eff: number;
  ai_keep: number | null;
  contribution: number | null;
  student_critical_count: number;
};

export type ProjectScopes = {
  sum_raw: number;
  sum_eff: number;
  mean_raw: number;
  ai_factor: number;
  crit_sa_count: number;
  crit_security_count: number;
  crit_cx_count: number;
  penalty_on: number;
  students: StudentScope[];
};

export type StructuralMeta = {
  penalty_mode: string;
  decimals?: number;
  quantize_final?: number;
  final_outputs?: string[];
};

export type FormulaDef = {
  name: string;
  infix: string;
  expr: unknown;
};

export type ManualFieldDef = {
  name: string;
  value: number;
  description: string;
};

export type ManualFields = {
  defs: ManualFieldDef[];
  /** project_id (as string) → field name → value override. */
  values: Record<string, Record<string, number>>;
  /**
   * project_id (as string) → field name → free-text explanation of the value.
   * Display/audit metadata only; never read by the grading engine.
   */
  notes?: Record<string, Record<string, string>>;
};

/** A named global constant usable in any formula (mirror of grade_core). */
export type ConstantDef = {
  name: string;
  value: number;
  description: string;
};

export type MetricAnchor = {
  floor: number;
  ceiling: number;
};

export type GradeSpec = {
  meta: StructuralMeta;
  weights: Record<string, number>;
  /** Optional per-metric absolute anchors for hybrid cohort normalization. */
  anchors?: Record<string, MetricAnchor>;
  models: Record<string, number>;
  levels: Record<string, number>;
  formulas: {
    task: FormulaDef[];
    project: FormulaDef[];
    student: FormulaDef[];
  };
  manual_fields: ManualFields;
  /** Named global constants usable in any formula. */
  constants: ConstantDef[];
};

/** @deprecated use GradeSpec */
export type StructuralSpec = GradeSpec;

export type StructuralOutput = {
  scopes: ProjectScopes;
};

export type GradeOutput = {
  grades: ProjectGrades;
  trees: GradeTrees;
};

export type MetricBounds = {
  floor: number;
  ceiling: number;
  p10: number;
  p90: number;
  sample_count: number;
};

export type CohortBounds = {
  metrics: Record<string, MetricBounds>;
};

export type CohortProjectGrade = {
  project_id: number;
  output: GradeOutput;
  /** Hybrid-normalized 0–10 preview per raw metric (explainability). */
  normalized: Record<string, number>;
};

export type CohortGradeOutput = {
  bounds: CohortBounds;
  projects: CohortProjectGrade[];
};

export type ProjectGrades = {
  project_id: number;
  quality_grade: number;
  quality_penalized: number;
  project_penalty: number;
  ai_factor: number;
  project_final: number;
  team_size: number;
  axes: AxisGrade[];
  /** EXTRA_TECH aggregate (weighted extra-technologies units; mirror of grade_core). */
  extra_tech?: number;
  /** Per-signal breakdown of extra_tech (only signals with raw > 0). */
  extra_tech_components?: ExtraTechComponent[];
  students: StudentGrades[];
};

/** One contribution to the extra_tech aggregate (mirror of grade_core). */
export type ExtraTechComponent = {
  key: string;
  raw: number;
  weight: number;
  contribution: number;
};

export type AxisGrade = {
  key: string;
  raw: number | null;
  score: number | null;
  present: boolean;
};

/** One negative contribution to the code-quality penalty (mirror of grade_core). */
export type CodeQualityComponent = {
  /** "architecture" | "complexity" | "static_analysis" */
  dimension: string;
  blame: number;
  blame_per_point: number;
  /** "critical" | "warning" */
  tier: string;
  points: number;
};

export type StudentGrades = {
  student_id: string;
  raw_points: number;
  effective_points: number;
  ai_keep: number | null;
  contribution: number | null;
  base_grade: number;
  student_penalty: number;
  /** v4 cohort-percentile code-quality deduction (mirror of grade_core). */
  codequality_penalty: number;
  /** Per-signal breakdown of codequality_penalty; empty when no penalty. */
  codequality_components: CodeQualityComponent[];
  student_final: number;
};

export type GradeTrees = {
  project: NamedNode[];
  students: StudentTree[];
  tasks: TaskTree[];
};

export type NamedNode = {
  name: string;
  node: ExplainNode;
};

export type StudentTree = {
  student_id: string;
  formulas: NamedNode[];
};

export type TaskTree = {
  assignee_id: string;
  raw_points: number;
  keep: number;
  node: ExplainNode;
};

export type ExplainNode = {
  label: string;
  expr: string;
  value: number;
  children: ExplainNode[];
};

/** Minimal SQL executor — implemented by Tauri plugin-sql and better-sqlite3 in tests. */
export type SqlExecutor = {
  select<T>(sql: string, bind?: unknown[]): Promise<T[]>;
  queryRow<T>(sql: string, bind?: unknown[]): Promise<T | undefined>;
};
