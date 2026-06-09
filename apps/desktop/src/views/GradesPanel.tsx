import type { GradeOutput } from "../data/types";

type Props = {
  grades: Map<number, GradeOutput>;
  loading: boolean;
  error: string | null;
};

export default function GradesPanel({ grades, loading, error }: Props) {
  if (grades.size === 0 && !loading) {
    return (
      <section className="grades-panel">
        <p className="meta">Open a grading.db to compute live grades.</p>
      </section>
    );
  }

  return (
    <section className="grades-panel">
      <h2>Live grades</h2>
      {loading && <p className="meta">Recomputing…</p>}
      {error && <p className="error">Engine: {error} (showing last-good grades)</p>}
      <table>
        <thead>
          <tr>
            <th>Project</th>
            <th>Quality</th>
            <th>Penalized</th>
            <th>Final</th>
            <th>Students</th>
          </tr>
        </thead>
        <tbody>
          {[...grades.values()].map((g) => (
            <tr key={g.grades.project_id}>
              <td>{g.grades.project_id}</td>
              <td>{g.grades.quality_grade.toFixed(2)}</td>
              <td>{g.grades.quality_penalized.toFixed(2)}</td>
              <td>{g.grades.project_final.toFixed(2)}</td>
              <td>
                {g.grades.students
                  .map((s) => `${s.student_id}: ${s.student_final.toFixed(2)}`)
                  .join(", ")}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
