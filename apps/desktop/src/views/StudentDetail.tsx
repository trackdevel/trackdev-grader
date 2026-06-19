import { invoke } from "@tauri-apps/api/core";

import type { LoadedDb, GradeOutput } from "../data/types";
import { studentReviewGate } from "../logic/gates";
import { formatFlagDetails, flagSeverityClass } from "../logic/flagDetails";
import Tree, { FormulaTreeList } from "./Tree";
import { fmtNum } from "./SortableTable";
import { projectHref } from "../hooks/useHashRoute";

/** TrackDev task page; mirrors report::markdown::trackdev_task_url. */
function trackdevTaskUrl(taskId: number): string {
  return `https://trackdev.org/dashboard/tasks/${taskId}`;
}

/** Open in the system browser (Tauri); fall back to a new tab under `pnpm dev`. */
async function openExternal(url: string): Promise<void> {
  try {
    await invoke("open_external", { url });
  } catch {
    window.open(url, "_blank", "noreferrer");
  }
}

const CQ_DIMENSION_LABELS: Record<string, string> = {
  architecture: "Architecture conformance",
  complexity: "Code complexity",
  static_analysis: "Static analysis",
};

function cqDimensionLabel(dimension: string): string {
  return CQ_DIMENSION_LABELS[dimension] ?? dimension;
}

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
  projectId: number;
  studentId: string;
};

function KvTable({ pairs }: { pairs: Array<[string, string | number | null]> }) {
  return (
    <table className="kv-table">
      <tbody>
        {pairs.map(([k, v]) => (
          <tr key={k}>
            <th>{k}</th>
            <td>{v === null || v === undefined ? "" : String(v)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export default function StudentDetail({ db, grades, projectId, studentId }: Props) {
  const raw = db.projects.find((p) => p.project_id === projectId);
  const out = grades.get(projectId);
  const diag = db.diagnostics.get(projectId);
  const stuMeta = raw?.students.find((s) => s.student_id === studentId);
  const stuGrades = out?.grades.students.find((s) => s.student_id === studentId);

  if (!raw || !stuMeta || !stuGrades || !out) {
    return <p className="error">Student not found.</p>;
  }

  const gate = diag ? studentReviewGate(raw, diag, studentId, stuGrades.effective_points) : null;
  const studentTree = out.trees.students.find((s) => s.student_id === studentId);
  const taskTrees = out.trees.tasks.filter((t) => t.assignee_id === studentId);
  // Display tasks come from the diagnostics channel (id/key/sprint enrichment);
  // raw.tasks stays the minimal grade input consumed by the engine and gates.
  const studentTasks = (diag?.tasks ?? []).filter((t) => t.assignee_id === studentId);
  const studentFlags = (diag?.flags ?? []).filter((f) => f.student_id === studentId);
  const aiRows = (diag?.aiDetect ?? []).filter((a) => a.student_id === studentId);

  return (
    <div className="detail-page">
      <a className="back-link" href="#/students">
        ← All students
      </a>
      <h2>{stuMeta.full_name}</h2>
      <p className="subtitle">
        Team:{" "}
        <a className="entity-link" href={projectHref(projectId)}>
          {raw.name}
        </a>
      </p>

      <section className="detail-section">
        <h3>Grade breakdown</h3>
        <KvTable
          pairs={[
            ["Final grade", fmtNum(stuGrades.student_final)],
            ["Base grade", fmtNum(stuGrades.base_grade)],
            ["Student penalty", fmtNum(stuGrades.student_penalty)],
            ["Code-quality penalty", fmtNum(stuGrades.codequality_penalty)],
            ["Tasques sense declarar IA", stuGrades.ai_undeclared_count],
            ["AI keep factor", stuGrades.ai_keep != null ? fmtNum(stuGrades.ai_keep, 3) : null],
            ["Contribution share", stuGrades.contribution != null ? fmtNum(stuGrades.contribution, 3) : null],
            ["Review gate", gate],
          ]}
        />
      </section>

      <section className="detail-section">
        <h3>Code-quality penalty breakdown</h3>
        {stuGrades.codequality_components.length === 0 ? (
          <p className="hint">No code-quality penalty (−0.00).</p>
        ) : (
          (() => {
            const rawSum = stuGrades.codequality_components.reduce((a, c) => a + c.points, 0);
            const capped = rawSum > stuGrades.codequality_penalty + 1e-9;
            return (
              <>
                <p className="hint">
                  Each signal ranks this student against the whole cohort by blame per
                  effective point: the worst ~10% land in the critical band, the next
                  ~20% in the warning band. The penalty is the sum of the bands below.
                </p>
                <table>
                  <thead>
                    <tr>
                      <th>negative contribution</th>
                      <th>band</th>
                      <th>blame</th>
                      <th>blame / point</th>
                      <th>points</th>
                    </tr>
                  </thead>
                  <tbody>
                    {stuGrades.codequality_components.map((c, i) => (
                      <tr
                        key={i}
                        className={c.tier === "critical" ? "flag-critical" : "flag-warning"}
                      >
                        <td>{cqDimensionLabel(c.dimension)}</td>
                        <td>{c.tier}</td>
                        <td>{fmtNum(c.blame, 2)}</td>
                        <td>{fmtNum(c.blame_per_point, 2)}</td>
                        <td>−{fmtNum(c.points, 2)}</td>
                      </tr>
                    ))}
                    <tr>
                      <td colSpan={4}>
                        <strong>Total code-quality penalty</strong>
                        {capped ? ` (capped from −${fmtNum(rawSum, 2)})` : ""}
                      </td>
                      <td>
                        <strong>−{fmtNum(stuGrades.codequality_penalty, 2)}</strong>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </>
            );
          })()
        )}
      </section>

      <section className="detail-section">
        <h3>How the final grade is computed</h3>
        {studentTree ? (
          <FormulaTreeList items={studentTree.formulas} />
        ) : (
          <p className="hint">No student formula tree.</p>
        )}
        {taskTrees.length > 0 && (
          <details className="tree-formula-block">
            <summary>Per-task keep ({taskTrees.length})</summary>
            {taskTrees.map((t, i) => (
              <div key={`${t.assignee_id}-${i}`} className="task-tree">
                <p className="meta">
                  Task {i + 1}: raw={t.raw_points}, keep={fmtNum(t.keep, 4)}
                </p>
                <Tree node={t.node} />
              </div>
            ))}
          </details>
        )}
      </section>

      <section className="detail-section">
        <h3>Tasks ({studentTasks.length})</h3>
        {studentTasks.length === 0 ? (
          <p className="hint">No tasks.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>task</th>
                <th>sprint</th>
                <th>raw_points</th>
                <th>ai_model</th>
                <th>ai_level</th>
                <th>declared</th>
              </tr>
            </thead>
            <tbody>
              {studentTasks.map((t, i) => {
                const url = trackdevTaskUrl(t.task_id);
                return (
                  <tr key={t.task_id || i}>
                    <td>
                      <a
                        className="entity-link"
                        href={url}
                        onClick={(e) => {
                          e.preventDefault();
                          void openExternal(url);
                        }}
                      >
                        {t.task_key ?? `#${t.task_id}`}
                      </a>
                    </td>
                    <td>{t.sprint ?? ""}</td>
                    <td>{t.raw_points}</td>
                    <td>{t.ai_model ?? ""}</td>
                    <td>{t.ai_level ?? ""}</td>
                    <td>{t.declared ? "yes" : "no"}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </section>

      <section className="detail-section">
        <h3>Flags</h3>
        {studentFlags.length === 0 ? (
          <p className="hint">No flags.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>sprint</th>
                <th>type</th>
                <th>severity</th>
                <th>details</th>
              </tr>
            </thead>
            <tbody>
              {studentFlags.map((f, i) => (
                <tr key={i} className={flagSeverityClass(f.severity)}>
                  <td>{f.sprint_label ?? f.source}</td>
                  <td>{f.flag_type}</td>
                  <td>{f.severity}</td>
                  <td>{formatFlagDetails(f.flag_type, f.details)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <section className="detail-section">
        <h3>AI detection</h3>
        {aiRows.length === 0 ? (
          <p className="hint">No AI detection rows.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>sprint</th>
                <th>risk_level</th>
              </tr>
            </thead>
            <tbody>
              {aiRows.map((a, i) => (
                <tr key={i}>
                  <td>{a.sprint_label ?? ""}</td>
                  <td>{a.risk_level}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  );
}
