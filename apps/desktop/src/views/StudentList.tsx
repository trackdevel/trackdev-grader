import type { LoadedDb } from "../data/types";
import type { GradeOutput } from "../data/types";
import SortableTable, { fmtNum } from "./SortableTable";
import { projectHref, studentHref } from "../hooks/useHashRoute";

type Row = {
  project_id: number;
  team: string;
  student_id: string;
  student: string;
  grade: number;
  base: number;
  stu_pen: number;
  cq_pen: number;
  ai_keep: number | null;
  contribution: number | null;
  project_abs: number;
  team_size: number;
};

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
};

export default function StudentList({ db, grades }: Props) {
  const rows: Row[] = [];
  for (const raw of db.projects) {
    const out = grades.get(raw.project_id);
    for (const stu of raw.students) {
      const g = out?.grades.students.find((s) => s.student_id === stu.student_id);
      rows.push({
        project_id: raw.project_id,
        team: raw.name,
        student_id: stu.student_id,
        student: stu.full_name,
        grade: g?.student_final ?? Number.NaN,
        base: g?.base_grade ?? Number.NaN,
        stu_pen: g?.student_penalty ?? Number.NaN,
        cq_pen: g?.codequality_penalty ?? Number.NaN,
        ai_keep: g?.ai_keep ?? null,
        contribution: g?.contribution ?? null,
        // Project absolute grade (project_final): same for every team member,
        // unnormalized by team size. Pairs with team_size to explain student_base.
        project_abs: out?.grades.project_final ?? Number.NaN,
        team_size: raw.team_size,
      });
    }
  }

  const hasGrades = rows.some((r) => Number.isFinite(r.grade));

  return (
    <section className="view">
      <h3>All students</h3>
      <p className="hint">Click column headers to sort. Grades from the live WASM engine.</p>
      {rows.length === 0 ? (
        <p className="hint">No students in this database.</p>
      ) : !hasGrades ? (
        <p className="hint">Students loaded — waiting for grade engine (check errors above).</p>
      ) : null}
      {rows.length > 0 && (
        <SortableTable
          rows={rows}
          rowKey={(r) => `${r.project_id}-${r.student_id}`}
          defaultSort={{ key: "student", dir: "asc" }}
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
            {
              key: "team_size",
              header: "team_size",
              title: "Team size (enrolled roster)",
              numeric: true,
              sortable: true,
              sortValue: (r) => r.team_size,
              render: (r) => r.team_size,
            },
            {
              key: "student",
              header: "student",
              sortable: true,
              sortValue: (r) => r.student,
              render: (r) => (
                <a className="entity-link" href={studentHref(r.project_id, r.student_id)}>
                  {r.student}
                </a>
              ),
            },
            {
              key: "grade",
              header: "grade",
              sortable: true,
              numeric: true,
              sortValue: (r) => r.grade,
              render: (r) => (Number.isFinite(r.grade) ? fmtNum(r.grade) : "—"),
            },
            {
              key: "base",
              header: "base",
              render: (r) => (Number.isFinite(r.base) ? fmtNum(r.base) : "—"),
            },
            {
              key: "project_abs",
              header: "project_abs",
              title: "Project grade — absolute, unnormalized by team size (project_final)",
              numeric: true,
              sortable: true,
              sortValue: (r) => r.project_abs,
              render: (r) => (Number.isFinite(r.project_abs) ? fmtNum(r.project_abs) : "—"),
            },
            {
              key: "stu_pen",
              header: "stu_pen",
              render: (r) => (Number.isFinite(r.stu_pen) ? fmtNum(r.stu_pen) : "—"),
            },
            {
              key: "cq_pen",
              header: "cq_pen",
              numeric: true,
              sortable: true,
              sortValue: (r) => r.cq_pen,
              render: (r) => (Number.isFinite(r.cq_pen) ? fmtNum(r.cq_pen) : "—"),
            },
            {
              key: "ai_keep",
              header: "ai_keep",
              render: (r) => (r.ai_keep != null ? fmtNum(r.ai_keep, 3) : ""),
            },
            {
              key: "contribution",
              header: "contribution",
              render: (r) => (r.contribution != null ? fmtNum(r.contribution, 3) : ""),
            },
          ]}
        />
      )}
    </section>
  );
}
