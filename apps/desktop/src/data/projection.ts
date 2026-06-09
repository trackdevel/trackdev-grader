import type {
  AxisInputs,
  CritFinding,
  RawProject,
  RawStudent,
  RawTask,
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

async function architectureCounts(
  db: SqlExecutor,
  repos: string[],
): Promise<{ crit: number; warn: number }> {
  let crit = 0;
  let warn = 0;
  for (const repo of repos) {
    const rows = await db.select<{ severity: string }>(
      `SELECT severity FROM architecture_violations WHERE repo_full_name = ?`,
      [repo],
    );
    for (const row of rows) {
      if (row.severity === "CRITICAL") crit += 1;
      else if (row.severity === "WARNING") warn += 1;
    }
  }
  return { crit, warn };
}

async function loadTasks(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<RawTask[]> {
  if (sprintIds.length === 0) return [];
  const ph = placeholders(sprintIds.length);
  const rows = await db.select<{
    assignee_id: string;
    estimation_points: number;
    model_value: string | null;
    level_value: string | null;
    declared: number | null;
  }>(
    `SELECT t.assignee_id, t.estimation_points,
            tai.model_value, tai.level_value, tai.declared
     FROM tasks t
     JOIN students s ON s.id = t.assignee_id
     LEFT JOIN task_ai_usage tai ON tai.task_id = t.id
     WHERE s.team_project_id = ?
       AND t.sprint_id IN (${ph})
       AND t.status = 'DONE'
       AND t.type != 'USER_STORY'
       AND t.assignee_id IS NOT NULL
       AND t.estimation_points IS NOT NULL`,
    [projectId, ...sprintIds],
  );
  return rows.map((r) => ({
    assignee_id: r.assignee_id,
    raw_points: r.estimation_points,
    ai_model: r.model_value,
    ai_level: r.level_value,
    declared: (r.declared ?? 0) === 1,
  }));
}

async function loadStudents(db: SqlExecutor, projectId: number): Promise<RawStudent[]> {
  const rows = await db.select<{ id: string; full_name: string }>(
    `SELECT id, full_name FROM students WHERE team_project_id = ? ORDER BY id`,
    [projectId],
  );
  return rows.map((r) => ({ student_id: r.id, full_name: r.full_name }));
}

async function loadCritFindings(
  db: SqlExecutor,
  projectId: number,
): Promise<CritFinding[]> {
  const out: CritFinding[] = [];
  const repos = await projectRepos(db, projectId);
  for (const repo of repos) {
    const sa = await db.select<{ category: string | null }>(
      `SELECT category FROM static_analysis_findings
       WHERE repo_full_name = ? AND severity = 'CRITICAL'`,
      [repo],
    );
    for (const row of sa) {
      out.push({ kind: "static_analysis", category: row.category });
    }
    const cx = await db.select<{ severity: string }>(
      `SELECT severity FROM method_complexity_findings WHERE repo_full_name = ?`,
      [repo],
    );
    for (const row of cx) {
      if (row.severity === "CRITICAL") {
        out.push({ kind: "complexity", category: null });
      }
    }
  }
  return out;
}

async function loadStudentFlags(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<StudentFlag[]> {
  const out: StudentFlag[] = [];
  if (sprintIds.length > 0) {
    const ph = placeholders(sprintIds.length);
    const rows = await db.select<{ student_id: string; severity: string }>(
      `SELECT f.student_id, f.severity FROM flags f
       WHERE f.sprint_id IN (${ph})
         AND f.student_id NOT LIKE 'PROJECT_%'`,
      sprintIds,
    );
    for (const row of rows) {
      const enrolled = await db.queryRow<{ n: number }>(
        `SELECT COUNT(*) AS n FROM students WHERE id = ? AND team_project_id = ?`,
        [row.student_id, projectId],
      );
      if ((enrolled?.n ?? 0) > 0) {
        out.push({
          student_id: row.student_id,
          severity: row.severity,
          source: "sprint",
        });
      }
    }
  }
  const artifacts = await db.select<{ student_id: string | null; severity: string | null }>(
    `SELECT student_id, severity FROM student_artifact_flags
     WHERE project_id = ? AND student_id NOT LIKE 'PROJECT_%'`,
    [projectId],
  );
  for (const row of artifacts) {
    if (!row.student_id) continue;
    out.push({
      student_id: row.student_id,
      severity: row.severity ?? "",
      source: "artifact",
    });
  }
  return out;
}

export async function loadRawProject(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
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
  const doc = await documentationRaw(db, projectId, sprintIds);
  const cq = await codeQualityRaw(db, projectId, sprintIds);
  const surv = await survivalRaw(db, projectId, sprintIds);
  const arch = await architectureCounts(db, repos);

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
    arch_present: repos.length > 0,
  };

  return {
    project_id: projectId,
    name: nameRow?.name ?? "",
    team_size: teamRow?.n ?? 0,
    axis,
    tasks: await loadTasks(db, projectId, sprintIds),
    students: await loadStudents(db, projectId),
    crit_findings: await loadCritFindings(db, projectId),
    student_flags: await loadStudentFlags(db, projectId, sprintIds),
  };
}
