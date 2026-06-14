import type { LoadedDb, GradeOutput } from "../data/types";
import { studentReviewGate } from "../logic/gates";
import { formatFlagDetails, flagSeverityClass } from "../logic/flagDetails";
import Tree, { FormulaTreeList } from "./Tree";
import { fmtNum } from "./SortableTable";
import { projectHref } from "../hooks/useHashRoute";

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
  const studentTasks = raw.tasks.filter((t) => t.assignee_id === studentId);
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
            ["AI keep factor", stuGrades.ai_keep != null ? fmtNum(stuGrades.ai_keep, 3) : null],
            ["Contribution share", stuGrades.contribution != null ? fmtNum(stuGrades.contribution, 3) : null],
            ["Review gate", gate],
          ]}
        />
      </section>

      <section className="detail-section">
        <h3>How the final grade is computed</h3>
        {studentTree ? (
          <FormulaTreeList items={studentTree.formulas} />
        ) : (
          <p className="hint">No student formula tree.</p>
        )}
        {taskTrees.length > 0 && (
          <div className="tree-formula-block">
            <h4>Per-task keep</h4>
            {taskTrees.map((t, i) => (
              <div key={`${t.assignee_id}-${i}`} className="task-tree">
                <p className="meta">
                  Task {i + 1}: raw={t.raw_points}, keep={fmtNum(t.keep, 4)}
                </p>
                <Tree node={t.node} />
              </div>
            ))}
          </div>
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
                <th>raw_points</th>
                <th>ai_model</th>
                <th>ai_level</th>
                <th>declared</th>
              </tr>
            </thead>
            <tbody>
              {studentTasks.map((t, i) => (
                <tr key={i}>
                  <td>{t.raw_points}</td>
                  <td>{t.ai_model ?? ""}</td>
                  <td>{t.ai_level ?? ""}</td>
                  <td>{t.declared ? "yes" : "no"}</td>
                </tr>
              ))}
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
