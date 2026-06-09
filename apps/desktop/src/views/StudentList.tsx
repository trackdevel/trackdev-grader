import type { LoadedDb } from "../data/types";
import type { GradeOutput } from "../data/types";
import { studentReviewGate } from "../logic/gates";
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
  ai_keep: number | null;
  contribution: number | null;
  gate: string | null;
};

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
};

export default function StudentList({ db, grades }: Props) {
  const rows: Row[] = [];
  for (const raw of db.projects) {
    const out = grades.get(raw.project_id);
    if (!out) continue;
    const diag = db.diagnostics.get(raw.project_id);
    for (const stu of raw.students) {
      const g = out.grades.students.find((s) => s.student_id === stu.student_id);
      if (!g) continue;
      rows.push({
        project_id: raw.project_id,
        team: raw.name,
        student_id: stu.student_id,
        student: stu.full_name,
        grade: g.student_final,
        base: g.base_grade,
        stu_pen: g.student_penalty,
        ai_keep: g.ai_keep,
        contribution: g.contribution,
        gate: diag
          ? studentReviewGate(raw, diag, stu.student_id, g.effective_points)
          : null,
      });
    }
  }

  return (
    <section className="view">
      <h3>All students</h3>
      <p className="hint">Click column headers to sort. Grades from the live WASM engine.</p>
      {rows.length === 0 ? (
        <p className="hint">Open a grading.db to see students.</p>
      ) : (
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
              render: (r) => fmtNum(r.grade),
            },
            { key: "base", header: "base", render: (r) => fmtNum(r.base) },
            { key: "stu_pen", header: "stu_pen", render: (r) => fmtNum(r.stu_pen) },
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
            { key: "gate", header: "gate", render: (r) => r.gate ?? "" },
          ]}
        />
      )}
    </section>
  );
}
