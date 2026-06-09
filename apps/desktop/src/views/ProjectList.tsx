import type { LoadedDb, GradeOutput } from "../data/types";
import { projectReviewGate } from "../logic/gates";
import SortableTable, { fmtNum } from "./SortableTable";
import { projectHref } from "../hooks/useHashRoute";

type Row = {
  project_id: number;
  team: string;
  grade: number;
  quality: number;
  quality_penalized: number;
  ai_factor: number;
  team_size: number;
  gate: string | null;
};

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
};

export default function ProjectList({ db, grades }: Props) {
  const rows: Row[] = db.projects
    .map((raw) => {
      const out = grades.get(raw.project_id);
      if (!out) return null;
      const diag = db.diagnostics.get(raw.project_id);
      return {
        project_id: raw.project_id,
        team: raw.name,
        grade: out.grades.project_final,
        quality: out.grades.quality_grade,
        quality_penalized: out.grades.quality_penalized,
        ai_factor: out.grades.ai_factor,
        team_size: raw.team_size,
        gate: diag ? projectReviewGate(diag) : null,
      };
    })
    .filter((r): r is Row => r !== null)
    .sort((a, b) => b.grade - a.grade);

  return (
    <section className="view">
      <h3>All projects</h3>
      {rows.length === 0 ? (
        <p className="hint">Open a grading.db to see projects.</p>
      ) : (
        <SortableTable
          rows={rows}
          rowKey={(r) => String(r.project_id)}
          columns={[
            {
              key: "team",
              header: "team",
              render: (r) => (
                <a className="entity-link" href={projectHref(r.project_id)}>
                  {r.team}
                </a>
              ),
            },
            { key: "grade", header: "grade", render: (r) => fmtNum(r.grade) },
            { key: "quality", header: "quality", render: (r) => fmtNum(r.quality) },
            {
              key: "quality_penalized",
              header: "quality_penalized",
              render: (r) => fmtNum(r.quality_penalized),
            },
            { key: "ai_factor", header: "ai_factor", render: (r) => fmtNum(r.ai_factor, 3) },
            { key: "team_size", header: "team_size", render: (r) => String(r.team_size) },
            { key: "gate", header: "gate", render: (r) => r.gate ?? "" },
          ]}
        />
      )}
    </section>
  );
}
