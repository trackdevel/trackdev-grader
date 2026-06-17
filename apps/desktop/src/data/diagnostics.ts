import type { SqlExecutor } from "./types";

function placeholders(n: number): string {
  return Array.from({ length: n }, () => "?").join(", ");
}

export type DetailedFlag = {
  student_id: string;
  flag_type: string;
  severity: string;
  details: string | null;
  sprint_label: string | null;
  source: "sprint" | "artifact";
};

export type AiDetectRow = {
  student_id: string;
  risk_level: string;
  sprint_label: string | null;
};

/**
 * A graded task enriched for the student-detail table: TrackDev identity
 * (`task_id`/`task_key` for the task-info link) plus sprint context. This is a
 * display-only mirror of the grade-input `RawTask` (which stays minimal so the
 * Rust-generated `reference.raw_projects.json` fixture is not perturbed).
 */
export type DisplayTask = {
  task_id: number;
  task_key: string | null;
  sprint: string | null;
  assignee_id: string;
  raw_points: number;
  ai_model: string | null;
  ai_level: string | null;
  declared: boolean;
};

/** One "extra technology vs. baseline" itemized row (EXTRA_TECH, display-only). */
export type ProjectTechnology = {
  repo_full_name: string;
  technology: string;
  /** fcm | specifications | email | graphics | av | dependency */
  category: string;
  /** gradle | ast | both */
  source: string;
  evidence: string | null;
  depth: number;
};

export type ProjectDiagnostics = {
  flags: DetailedFlag[];
  aiDetect: AiDetectRow[];
  plagiarism: boolean;
  tasks: DisplayTask[];
  technologies: ProjectTechnology[];
};

async function loadDisplayTasks(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<DisplayTask[]> {
  if (sprintIds.length === 0) return [];
  const ph = placeholders(sprintIds.length);
  // Ordered by sprint then id so the per-student table reads chronologically.
  // AI declaration was adopted mid-course, so early-sprint tasks legitimately
  // have empty model/level cells — the sprint column makes that self-evident.
  const rows = await db.select<{
    task_id: number;
    task_key: string | null;
    sprint: string | null;
    assignee_id: string;
    estimation_points: number;
    model_value: string | null;
    level_value: string | null;
    declared: number | null;
  }>(
    `SELECT t.id AS task_id, t.task_key AS task_key, sp.name AS sprint,
            t.assignee_id, t.estimation_points,
            tai.model_value, tai.level_value, tai.declared
     FROM tasks t
     JOIN students s ON s.id = t.assignee_id
     LEFT JOIN sprints sp ON sp.id = t.sprint_id
     LEFT JOIN task_ai_usage tai ON tai.task_id = t.id
     WHERE s.team_project_id = ?
       AND t.sprint_id IN (${ph})
       AND t.status = 'DONE'
       AND t.type != 'USER_STORY'
       AND t.assignee_id IS NOT NULL
       AND t.estimation_points IS NOT NULL
     ORDER BY t.sprint_id ASC, t.id ASC`,
    [projectId, ...sprintIds],
  );
  return rows.map((r) => ({
    task_id: r.task_id,
    task_key: r.task_key,
    sprint: r.sprint,
    assignee_id: r.assignee_id,
    raw_points: r.estimation_points,
    ai_model: r.model_value,
    ai_level: r.level_value,
    declared: (r.declared ?? 0) === 1,
  }));
}

async function tableExists(db: SqlExecutor, name: string): Promise<boolean> {
  const rows = await db.select<{ n: number }>(
    `SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table' AND name = ?`,
    [name],
  );
  return (rows[0]?.n ?? 0) > 0;
}

/** EXTRA_TECH: itemized extra technologies for the project's repos, scoped via
 * `project_inventory_runs`. Empty when the table is absent (pre-EXTRA_TECH DB). */
async function loadExtraTechnologies(
  db: SqlExecutor,
  projectId: number,
): Promise<ProjectTechnology[]> {
  if (!(await tableExists(db, "repo_extra_technologies"))) return [];
  const rows = await db.select<{
    repo_full_name: string;
    technology: string;
    category: string;
    source: string;
    evidence: string | null;
    depth: number;
  }>(
    `SELECT t.repo_full_name, t.technology, t.category, t.source, t.evidence, t.depth
     FROM repo_extra_technologies t
     JOIN project_inventory_runs r ON r.repo_full_name = t.repo_full_name
     WHERE r.project_id = ?
     ORDER BY t.category ASC, t.depth DESC, t.technology ASC`,
    [projectId],
  );
  return rows.map((r) => ({
    repo_full_name: r.repo_full_name,
    technology: r.technology,
    category: r.category,
    source: r.source,
    evidence: r.evidence,
    depth: r.depth,
  }));
}

export async function loadProjectDiagnostics(
  db: SqlExecutor,
  projectId: number,
  sprintIds: number[],
): Promise<ProjectDiagnostics> {
  const flags: DetailedFlag[] = [];

  if (sprintIds.length > 0) {
    const ph = placeholders(sprintIds.length);
    const sprintFlags = await db.select<{
      student_id: string;
      flag_type: string;
      severity: string;
      details: string | null;
      sprint_label: string | null;
    }>(
      `SELECT f.student_id, f.flag_type, f.severity, f.details, sp.name AS sprint_label
       FROM flags f
       JOIN sprints sp ON sp.id = f.sprint_id
       WHERE f.sprint_id IN (${ph})`,
      sprintIds,
    );
    for (const row of sprintFlags) {
      flags.push({
        student_id: row.student_id,
        flag_type: row.flag_type,
        severity: row.severity,
        details: row.details,
        sprint_label: row.sprint_label,
        source: "sprint",
      });
    }
  }

  const artifactFlags = await db.select<{
    student_id: string | null;
    flag_type: string;
    severity: string | null;
    details: string | null;
  }>(
    `SELECT student_id, flag_type, severity, details
     FROM student_artifact_flags WHERE project_id = ?`,
    [projectId],
  );
  for (const row of artifactFlags) {
    if (!row.student_id) continue;
    flags.push({
      student_id: row.student_id,
      flag_type: row.flag_type,
      severity: row.severity ?? "",
      details: row.details,
      sprint_label: null,
      source: "artifact",
    });
  }

  const synthetic = `PROJECT_${projectId}`;
  const plagiarism = flags.some(
    (f) => f.student_id === synthetic && f.flag_type === "CROSS_TEAM_SIMILARITY",
  );

  const aiDetect: AiDetectRow[] = [];
  if (sprintIds.length > 0) {
    const ph = placeholders(sprintIds.length);
    const rows = await db.select<{
      student_id: string;
      risk_level: string;
      sprint_label: string | null;
    }>(
      `SELECT ssai.student_id, ssai.risk_level, sp.name AS sprint_label
       FROM student_sprint_ai_usage ssai
       JOIN sprints sp ON sp.id = ssai.sprint_id
       WHERE ssai.project_id = ? AND ssai.sprint_id IN (${ph})`,
      [projectId, ...sprintIds],
    );
    for (const row of rows) {
      aiDetect.push({
        student_id: row.student_id,
        risk_level: row.risk_level,
        sprint_label: row.sprint_label,
      });
    }
  }

  const tasks = await loadDisplayTasks(db, projectId, sprintIds);
  const technologies = await loadExtraTechnologies(db, projectId);

  return { flags, aiDetect, plagiarism, tasks, technologies };
}
