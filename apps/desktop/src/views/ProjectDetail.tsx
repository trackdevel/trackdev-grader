import type { LoadedDb, GradeOutput } from "../data/types";
import { axisScore, qualityEff } from "../logic/gradeAxes";
import { projectReviewGate } from "../logic/gates";
import { fmtNum } from "./SortableTable";
import { FormulaTreeList } from "./Tree";
import { studentHref } from "../hooks/useHashRoute";

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
  projectId: number;
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

function fmtAxis(n: number | null): string {
  return n != null && Number.isFinite(n) ? fmtNum(n) : "—";
}

const AXIS_LABELS: Record<string, string> = {
  documentation: "Documentation score",
  code_quality: "Code quality score",
  survival: "Survival score",
  architecture: "Architecture score",
};

export default function ProjectDetail({ db, grades, projectId }: Props) {
  const raw = db.projects.find((p) => p.project_id === projectId);
  const out = grades.get(projectId);
  const diag = db.diagnostics.get(projectId);

  if (!raw || !out) {
    return <p className="error">Project not found.</p>;
  }

  const gate = diag ? projectReviewGate(diag) : null;
  const axisPairs: Array<[string, string | number | null]> = out.grades.axes.map((a) => [
    AXIS_LABELS[a.key] ?? a.key,
    a.present && a.score != null ? fmtNum(a.score) : null,
  ]);
  axisPairs.push(
    ["CC %", raw.axis.cq_present ? fmtNum(raw.axis.cc_pct) : null],
    ["Mutation score", raw.axis.cq_present ? fmtNum(raw.axis.mutation_score) : null],
    [
      "Arch crit / warn",
      `${raw.axis.arch_crit_count} / ${raw.axis.arch_warn_count}`,
    ],
  );

  const projectFlags = (diag?.flags ?? []).filter(
    (f) => f.student_id === `PROJECT_${projectId}` || f.severity === "CRITICAL",
  );

  return (
    <div className="detail-page">
      <a className="back-link" href="#/projects">
        ← All projects
      </a>
      <h2>{raw.name}</h2>
      <p className="subtitle">Team size: {raw.team_size}</p>

      <section className="detail-section">
        <h3>Team grade</h3>
        <KvTable
          pairs={[
            ["work_base", fmtAxis(axisScore(out.grades.axes, "work_base"))],
            ["quality_eff", fmtAxis(qualityEff(out.grades.axes))],
            ["quality_multiplier", fmtAxis(axisScore(out.grades.axes, "quality_multiplier"))],
            ["Final grade", fmtNum(out.grades.project_final)],
            ["Composite quality", fmtNum(out.grades.quality_grade)],
            ["After penalties", fmtNum(out.grades.quality_penalized)],
            ["Team AI factor", fmtNum(out.grades.ai_factor, 3)],
            ["Project penalty", fmtNum(out.grades.project_penalty)],
            ["Review gate", gate],
          ]}
        />
      </section>

      <section className="detail-section">
        <h3>Quality axes</h3>
        <KvTable pairs={axisPairs} />
      </section>

      <section className="detail-section">
        <h3>Project formula tree</h3>
        <FormulaTreeList items={out.trees.project} />
      </section>

      <section className="detail-section">
        <h3>Students (summary)</h3>
        <table>
          <thead>
            <tr>
              <th>student</th>
              <th>grade</th>
              <th>base</th>
              <th>cq pen</th>
              <th>contribution</th>
            </tr>
          </thead>
          <tbody>
            {[...out.grades.students]
              .sort((a, b) => b.student_final - a.student_final)
              .map((s) => {
                const meta = raw.students.find((x) => x.student_id === s.student_id);
                return (
                  <tr key={s.student_id}>
                    <td>
                      <a className="entity-link" href={studentHref(projectId, s.student_id)}>
                        {meta?.full_name ?? s.student_id}
                      </a>
                    </td>
                    <td>{fmtNum(s.student_final)}</td>
                    <td>{fmtNum(s.base_grade)}</td>
                    <td>{fmtNum(s.codequality_penalty)}</td>
                    <td>{s.contribution != null ? fmtNum(s.contribution, 3) : ""}</td>
                  </tr>
                );
              })}
          </tbody>
        </table>
      </section>

      <section className="detail-section">
        <h3>Critical findings ({raw.crit_findings.length})</h3>
        {raw.crit_findings.length === 0 ? (
          <p className="hint">No critical findings.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>kind</th>
                <th>category</th>
              </tr>
            </thead>
            <tbody>
              {raw.crit_findings.map((c, i) => (
                <tr key={i}>
                  <td>{c.kind}</td>
                  <td>{c.category ?? ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      {projectFlags.length > 0 && (
        <section className="detail-section">
          <h3>Project flags</h3>
          <table>
            <thead>
              <tr>
                <th>type</th>
                <th>severity</th>
                <th>details</th>
              </tr>
            </thead>
            <tbody>
              {projectFlags.map((f, i) => (
                <tr key={i}>
                  <td>{f.flag_type}</td>
                  <td>{f.severity}</td>
                  <td>{f.details ?? ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}
    </div>
  );
}
