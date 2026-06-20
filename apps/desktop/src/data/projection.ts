import type {
  AxisInputs,
  RawProject,
  RawStudent,
  RawTask,
  RepoMetrics,
  SqlExecutor,
  StudentFlag,
} from "./types";

function placeholders(n: number): string {
  return Array.from({ length: n }, () => "?").join(", ");
}

export async function sprintIdsUpToCurrent(
  db: SqlExecutor,
  projectId: number,
  today: string,
): Promise<number[]> {
  const rows = await db.select<{ id: number }>(
    `SELECT id FROM sprints
     WHERE project_id = ? AND start_date IS NOT NULL
       AND start_date != '' AND start_date <= ?
     ORDER BY start_date ASC`,
    [projectId, today],
  );
  return rows.map((r) => r.id);
}

async function projectRepos(db: SqlExecutor, projectId: number): Promise<string[]> {
  const rows = await db.select<{ repo_full_name: string }>(
    `SELECT DISTINCT pr.repo_full_name AS repo_full_name
     FROM pull_requests pr
     JOIN pr_authors pa ON pa.pr_id = pr.id
     JOIN students s ON s.id = pa.student_id
     WHERE s.team_project_id = ? AND pr.repo_full_name IS NOT NULL`,
    [projectId],
  );
  return rows.map((r) => r.repo_full_name);
}

async function documentationRaw(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<{ raw: number; present: boolean }> {
  if (sprintIds.length === 0) {
    return { raw: 0, present: false };
  }
  const ph = placeholders(sprintIds.length);
  const row = await db.queryRow<{ avg: number | null }>(
    `SELECT AVG(pde.total_doc_score) AS avg
     FROM pr_doc_evaluation pde
     JOIN pull_requests pr ON pr.id = pde.pr_id
     JOIN pr_authors pa ON pa.pr_id = pr.id
     JOIN students s ON s.id = pa.student_id
     WHERE s.team_project_id = ?
       AND pde.sprint_id IN (${ph})
       AND pde.total_doc_score IS NOT NULL`,
    [projectId, ...sprintIds],
  );
  const avg = row?.avg ?? null;
  return { raw: avg ?? 0, present: avg !== null };
}

async function codeQualityRaw(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<{ raw: number; ccPct: number; mutation: number; present: boolean }> {
  if (sprintIds.length === 0) {
    return { raw: 0, ccPct: 0, mutation: 0, present: false };
  }
  const ph = placeholders(sprintIds.length);
  const cq = await db.queryRow<{ mi: number | null; cc: number | null }>(
    `SELECT AVG(ssq.avg_maintainability) AS mi,
            AVG(ssq.pct_methods_cc_over_10) AS cc
     FROM student_sprint_quality ssq
     JOIN students s ON s.id = ssq.student_id
     WHERE s.team_project_id = ?
       AND ssq.sprint_id IN (${ph})
       AND ssq.avg_maintainability IS NOT NULL`,
    [projectId, ...sprintIds],
  );
  const mutation = await db.queryRow<{ avg: number | null }>(
    `SELECT AVG(pm.mutation_score) AS avg
     FROM pr_mutation pm
     JOIN pull_requests pr ON pr.id = pm.pr_id
     JOIN pr_authors pa ON pa.pr_id = pr.id
     JOIN students s ON s.id = pa.student_id
     WHERE s.team_project_id = ?
       AND pm.sprint_id IN (${ph})
       AND pm.mutation_score IS NOT NULL`,
    [projectId, ...sprintIds],
  );
  const mi = cq?.mi ?? null;
  return {
    raw: mi ?? 0,
    ccPct: cq?.cc ?? 0,
    mutation: mutation?.avg ?? 0,
    present: mi !== null,
  };
}

async function survivalRaw(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<{ raw: number; present: boolean }> {
  if (sprintIds.length === 0) {
    return { raw: 0, present: false };
  }
  const ph = placeholders(sprintIds.length);
  const rows = await db.select<{
    survival_rate_normalized: number;
    estimation_points_total: number | null;
  }>(
    `SELECT sss.survival_rate_normalized, sss.estimation_points_total
     FROM student_sprint_survival sss
     JOIN students s ON s.id = sss.student_id
     WHERE s.team_project_id = ?
       AND sss.sprint_id IN (${ph})
       AND sss.survival_rate_normalized IS NOT NULL`,
    [projectId, ...sprintIds],
  );
  let weightedSum = 0;
  let weightTotal = 0;
  for (const row of rows) {
    const w = Math.max(row.estimation_points_total ?? 1, 0);
    if (w > 0) {
      weightedSum += row.survival_rate_normalized * w;
      weightTotal += w;
    }
  }
  if (rows.length === 0 || weightTotal <= 0) {
    return { raw: 0, present: false };
  }
  return { raw: weightedSum / weightTotal, present: true };
}

async function architectureScanPresent(
  db: SqlExecutor,
  repos: string[],
): Promise<boolean> {
  for (const repo of repos) {
    // SKIPPED_HEAD_UNCHANGED means a prior OK scan's violations are still
    // valid (the cache gate found the same HEAD) — the data is present.
    const row = await db.queryRow<{ n: number }>(
      `SELECT COUNT(*) AS n FROM architecture_runs
       WHERE repo_full_name = ? AND status IN ('OK', 'SKIPPED_HEAD_UNCHANGED')`,
      [repo],
    );
    if ((row?.n ?? 0) > 0) {
      return true;
    }
  }
  return false;
}

// `layer_dependency` rule names detected/reported but EXCLUDED from the project
// quality penalty by policy (2026-06): Android use-cases / services importing
// API DTOs is an accepted pattern in this course, so it must not lower the
// grade. `presentation->!infrastructure` (controllers reaching repositories)
// stays graded. Mirror of db_axis::LAYER_RULES_UNGRADED.
const LAYER_RULES_UNGRADED = ["domain->!presentation", "application->!presentation"];

// Grading v4 (T2.1): the project quality axis sees only HIGH-LEVEL architecture
// — `layer_dependency` breaches (wrong package layering, a team-level design
// decision), minus the LAYER_RULES_UNGRADED policy exclusions. Every per-file
// AST rule (FINDVIEWBYID_USAGE, FRAGMENT_BYPASSES_…) is charged to the offending
// student via the *_HOTSPOT artifact flags, not to the team. Mirror of
// orchestration::grading_projection::db_axis::architecture_counts.
async function architectureCounts(
  db: SqlExecutor,
  repos: string[],
): Promise<{ crit: number; warn: number }> {
  let crit = 0;
  let warn = 0;
  const placeholders = LAYER_RULES_UNGRADED.map(() => "?").join(", ");
  for (const repo of repos) {
    const rows = await db.select<{ severity: string; n: number }>(
      `SELECT severity, COUNT(*) AS n FROM architecture_violations
       WHERE repo_full_name = ? AND rule_kind = 'layer_dependency'
         AND rule_name NOT IN (${placeholders})
       GROUP BY severity`,
      [repo, ...LAYER_RULES_UNGRADED],
    );
    for (const row of rows) {
      const sev = (row.severity ?? "").toUpperCase();
      if (sev === "CRITICAL" || sev === "ERROR") crit += row.n;
      else if (sev === "WARNING") warn += row.n;
    }
  }
  return { crit, warn };
}

async function loadTasks(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
  aiAllowedFromOrdinal: number,
): Promise<RawTask[]> {
  if (sprintIds.length === 0) return [];
  // `sprintIds` is ordered by start_date ascending, so the first
  // `ordinal - 1` entries are the AI-forbidden early sprints, whose tasks are
  // exempt (keep 100%) regardless of declaration.
  const forbiddenCount = Math.max(aiAllowedFromOrdinal - 1, 0);
  const aiForbiddenSprints = new Set(sprintIds.slice(0, forbiddenCount));

  const ph = placeholders(sprintIds.length);
  // `ptai` is the parent USER_STORY's AI usage: a task whose own declaration is
  // unset inherits its parent's "Ús de IA" attribute. Mirror of raw.rs::load_tasks.
  const rows = await db.select<{
    sprint_id: number;
    assignee_id: string;
    estimation_points: number;
    model_value: string | null;
    level_value: string | null;
    declared: number | null;
    parent_model_value: string | null;
    parent_level_value: string | null;
    parent_declared: number | null;
  }>(
    `SELECT t.sprint_id, t.assignee_id, t.estimation_points,
            tai.model_value, tai.level_value, tai.declared,
            ptai.model_value AS parent_model_value,
            ptai.level_value AS parent_level_value,
            ptai.declared    AS parent_declared
     FROM tasks t
     JOIN students s ON s.id = t.assignee_id
     LEFT JOIN task_ai_usage tai ON tai.task_id = t.id
     LEFT JOIN task_ai_usage ptai ON ptai.task_id = t.parent_task_id
     WHERE s.team_project_id = ?
       AND t.sprint_id IN (${ph})
       AND t.status = 'DONE'
       AND t.type != 'USER_STORY'
       AND t.assignee_id IS NOT NULL
       AND t.estimation_points IS NOT NULL`,
    [projectId, ...sprintIds],
  );
  // "Set" means the both-present gate: declared === 1 AND model AND level.
  const isSet = (
    model: string | null,
    level: string | null,
    declared: number | null,
  ): boolean => (declared ?? 0) === 1 && model !== null && level !== null;
  return rows.map((r): RawTask => {
    const base = { assignee_id: r.assignee_id, raw_points: r.estimation_points };
    if (aiForbiddenSprints.has(r.sprint_id)) {
      return { ...base, ai_model: null, ai_level: null, declared: false, ai_exempt: true };
    }
    // Own attribute → parent USER_STORY's attribute → undeclared.
    if (isSet(r.model_value, r.level_value, r.declared)) {
      return {
        ...base,
        ai_model: r.model_value,
        ai_level: r.level_value,
        declared: true,
        ai_exempt: false,
      };
    }
    if (isSet(r.parent_model_value, r.parent_level_value, r.parent_declared)) {
      return {
        ...base,
        ai_model: r.parent_model_value,
        ai_level: r.parent_level_value,
        declared: true,
        ai_exempt: false,
      };
    }
    return { ...base, ai_model: null, ai_level: null, declared: false, ai_exempt: false };
  });
}

async function loadStudents(db: SqlExecutor, projectId: number): Promise<RawStudent[]> {
  const rows = await db.select<{ id: string; full_name: string }>(
    `SELECT id, full_name FROM students WHERE team_project_id = ? ORDER BY id`,
    [projectId],
  );
  return rows.map((r) => ({ student_id: r.id, full_name: r.full_name }));
}

const COMPLEXITY_HOTSPOT = "COMPLEXITY_HOTSPOT";
const HOTSPOT_FLAG_TYPES = new Set([
  "ARCHITECTURE_HOTSPOT",
  "COMPLEXITY_HOTSPOT",
  "STATIC_ANALYSIS_HOTSPOT",
]);

/**
 * Per-student blame magnitude from a hotspot flag's `details` JSON; null when
 * absent. Mirror of grade_core::policy::hotspot_blame_magnitude wrapped in
 * raw.rs::flag_magnitude (complexity stores `score`, arch/SA store `weighted`).
 */
function flagMagnitude(flagType: string, details: string | null): number | null {
  if (!details) return null;
  let v: { score?: unknown; weighted?: unknown };
  try {
    v = JSON.parse(details);
  } catch {
    return null;
  }
  let mag = 0;
  if (flagType === COMPLEXITY_HOTSPOT) {
    mag = typeof v.score === "number" ? v.score : 0;
  } else if (HOTSPOT_FLAG_TYPES.has(flagType)) {
    mag = typeof v.weighted === "number" ? v.weighted : 0;
  }
  return mag > 0 ? mag : null;
}

async function loadStudentFlags(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<StudentFlag[]> {
  const out: StudentFlag[] = [];
  if (sprintIds.length > 0) {
    const ph = placeholders(sprintIds.length);
    const rows = await db.select<{
      student_id: string;
      severity: string;
      flag_type: string | null;
      details: string | null;
    }>(
      `SELECT student_id, severity, flag_type, details FROM flags
       WHERE sprint_id IN (${ph})
         AND student_id NOT LIKE 'PROJECT_%'`,
      sprintIds,
    );
    for (const row of rows) {
      const enrolled = await db.queryRow<{ n: number }>(
        `SELECT COUNT(*) AS n FROM students WHERE id = ? AND team_project_id = ?`,
        [row.student_id, projectId],
      );
      if ((enrolled?.n ?? 0) > 0) {
        const flagType = row.flag_type ?? "";
        out.push({
          student_id: row.student_id,
          severity: row.severity,
          source: "sprint",
          flag_type: flagType,
          weighted: flagMagnitude(flagType, row.details),
        });
      }
    }
  }
  const artifacts = await db.select<{
    student_id: string | null;
    severity: string | null;
    flag_type: string | null;
    details: string | null;
  }>(
    `SELECT student_id, severity, flag_type, details FROM student_artifact_flags
     WHERE project_id = ? AND student_id NOT LIKE 'PROJECT_%'`,
    [projectId],
  );
  for (const row of artifacts) {
    if (!row.student_id) continue;
    const flagType = row.flag_type ?? "";
    out.push({
      student_id: row.student_id,
      severity: row.severity ?? "",
      source: "artifact",
      flag_type: flagType,
      weighted: flagMagnitude(flagType, row.details),
    });
  }
  return out;
}

async function tableExists(db: SqlExecutor, name: string): Promise<boolean> {
  const row = await db.queryRow<{ n: number }>(
    `SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table' AND name = ?`,
    [name],
  );
  return (row?.n ?? 0) > 0;
}

/** PR-linked repos plus any inventory scan rows for this project. */
async function reposForInventory(
  db: SqlExecutor,
  projectId: number,
  prRepos: string[],
): Promise<string[]> {
  const out = [...prRepos];
  if (!(await tableExists(db, "project_inventory_runs"))) {
    return out;
  }
  const rows = await db.select<{ repo_full_name: string }>(
    `SELECT DISTINCT repo_full_name FROM project_inventory_runs
     WHERE project_id = ? AND metric_count > 0`,
    [projectId],
  );
  for (const row of rows) {
    if (!out.includes(row.repo_full_name)) {
      out.push(row.repo_full_name);
    }
  }
  return out;
}

/** Returns [] when `repo_structural_metrics` is absent (pre–Wave 1 grading.db). */
async function loadInventory(
  db: SqlExecutor,
  repos: string[],
): Promise<RepoMetrics[]> {
  if (!(await tableExists(db, "repo_structural_metrics"))) {
    return [];
  }
  const out: RepoMetrics[] = [];
  for (const repo of repos) {
    const rows = await db.select<{ metric_key: string; value: number }>(
      `SELECT metric_key, value FROM repo_structural_metrics WHERE repo_full_name = ?`,
      [repo],
    );
    if (rows.length === 0) continue;
    const metrics: Record<string, number> = {};
    for (const row of rows) {
      metrics[row.metric_key] = row.value;
    }
    out.push({ repo_full_name: repo, metrics });
  }
  return out;
}

/**
 * Project-grade axes require scanned structural inventory with non-zero code
 * mass (T2.4). Mirror of grade_core::policy::has_gradable_artifact — story
 * points and PR repo names alone do not qualify. Empty-artifact projects are
 * dropped from the cohort so they never appear in the desktop lists.
 */
export function hasGradableArtifact(raw: RawProject): boolean {
  const sum = (key: string): number =>
    (raw.inventory ?? []).reduce((acc, r) => acc + (r.metrics[key] ?? 0), 0);
  return sum("production_loc") > 0 || sum("production_statement_count") > 0;
}

/**
 * Sprint ordinal (1-based) from which AI usage counts toward the keep discount.
 * AI was forbidden in the course's first two sprints, so declarations there are
 * void and those tasks keep 100%. Mirror of Rust
 * `grading_projection::raw::AI_ALLOWED_FROM_SPRINT_ORDINAL` — keep in sync.
 */
export const AI_ALLOWED_FROM_SPRINT_ORDINAL = 3;

export async function loadRawProject(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
  // Defaults to 1 (no restriction) so reference-fixture tests stay stable;
  // production (loadGradingDbFromPath) passes AI_ALLOWED_FROM_SPRINT_ORDINAL.
  aiAllowedFromOrdinal = 1,
): Promise<RawProject> {
  const nameRow = await db.queryRow<{ name: string }>(
    `SELECT name FROM projects WHERE id = ?`,
    [projectId],
  );
  const teamRow = await db.queryRow<{ n: number }>(
    `SELECT COUNT(*) AS n FROM students WHERE team_project_id = ?`,
    [projectId],
  );
  const repos = await projectRepos(db, projectId);
  const inventoryRepos = await reposForInventory(db, projectId, repos);
  const doc = await documentationRaw(db, projectId, sprintIds);
  const cq = await codeQualityRaw(db, projectId, sprintIds);
  const surv = await survivalRaw(db, projectId, sprintIds);
  const arch = await architectureCounts(db, repos);
  const archPresent = await architectureScanPresent(db, repos);

  const axis: AxisInputs = {
    documentation_raw: doc.raw,
    doc_present: doc.present,
    code_quality_raw: cq.raw,
    cc_pct: cq.ccPct,
    mutation_score: cq.mutation,
    cq_present: cq.present,
    survival_raw: surv.raw,
    surv_present: surv.present,
    arch_crit_count: arch.crit,
    arch_warn_count: arch.warn,
    arch_present: archPresent,
  };

  return {
    project_id: projectId,
    name: nameRow?.name ?? "",
    team_size: teamRow?.n ?? 0,
    axis,
    inventory: await loadInventory(db, inventoryRepos),
    tasks: await loadTasks(db, projectId, sprintIds, aiAllowedFromOrdinal),
    students: await loadStudents(db, projectId),
    // v4 (T2.3): density is gone; criticals are charged to students via the
    // *_HOTSPOT artifact flags, so the project carries no crit_findings.
    crit_findings: [],
    student_flags: await loadStudentFlags(db, projectId, sprintIds),
  };
}
