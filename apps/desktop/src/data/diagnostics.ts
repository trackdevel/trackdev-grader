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

export type ProjectDiagnostics = {
  flags: DetailedFlag[];
  aiDetect: AiDetectRow[];
  plagiarism: boolean;
};

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
      `SELECT f.student_id, f.flag_type, f.severity, f.details, sp.label AS sprint_label
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
      `SELECT ssai.student_id, ssai.risk_level, sp.label AS sprint_label
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

  return { flags, aiDetect, plagiarism };
}
