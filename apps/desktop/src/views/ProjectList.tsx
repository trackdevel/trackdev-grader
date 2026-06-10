import type { LoadedDb, GradeOutput } from "../data/types";
import SortableTable, { fmtNum } from "./SortableTable";
import { projectHref } from "../hooks/useHashRoute";

type Row = {
  project_id: number;
  team: string;
  grade: number;
};

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
};

export default function ProjectList({ db, grades }: Props) {
  const rows: Row[] = db.projects.flatMap((raw) => {
    const out = grades.get(raw.project_id);
    if (!out) return [];
    return [
      {
        project_id: raw.project_id,
        team: raw.name,
        grade: out.grades.project_final,
      },
    ];
  });

  return (
    <section className="view">
      <h3>All projects</h3>
      <p className="hint">Click a project for full details: axes, formula tree, students, flags.</p>
      {rows.length === 0 ? (
        <p className="hint">Open a grading.db to see projects.</p>
      ) : (
        <SortableTable
          rows={rows}
          rowKey={(r) => String(r.project_id)}
          defaultSort={{ key: "grade", dir: "desc" }}
          columns={[
            {
              key: "team",
              header: "project",
              sortable: true,
              sortValue: (r) => r.team,
              render: (r) => (
                <a className="entity-link" href={projectHref(r.project_id)}>
                  {r.team}
                </a>
              ),
            },
            {
              key: "grade",
              header: "final grade",
              sortable: true,
              numeric: true,
              sortValue: (r) => r.grade,
              render: (r) => fmtNum(r.grade),
            },
          ]}
        />
      )}
    </section>
  );
}
