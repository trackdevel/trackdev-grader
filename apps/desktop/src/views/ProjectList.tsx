import { useState } from "react";

import type { LoadedDb, GradeOutput, GradeSpec } from "../data/types";
import { exportAllGrades } from "../data/export";
import { axisScore, qualityEff } from "../logic/gradeAxes";
import SortableTable, { fmtNum } from "./SortableTable";
import { projectHref } from "../hooks/useHashRoute";

type Row = {
  project_id: number;
  team: string;
  work_base: number;
  quality_eff: number;
  quality_multiplier: number;
  grade: number;
};

function fmtAxis(n: number): string {
  return Number.isFinite(n) ? fmtNum(n) : "—";
}

function rowFromGrade(raw: { project_id: number; name: string }, out: GradeOutput | undefined): Row {
  const axes = out?.grades.axes ?? [];
  return {
    project_id: raw.project_id,
    team: raw.name,
    work_base: axisScore(axes, "work_base") ?? Number.NaN,
    quality_eff: qualityEff(axes) ?? Number.NaN,
    quality_multiplier: axisScore(axes, "quality_multiplier") ?? Number.NaN,
    grade: out?.grades.project_final ?? Number.NaN,
  };
}

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
  spec: GradeSpec;
};

export default function ProjectList({ db, grades, spec }: Props) {
  const rows: Row[] = db.projects.map((raw) => rowFromGrade(raw, grades.get(raw.project_id)));

  const hasGrades = rows.some((r) => Number.isFinite(r.grade));
  const [exportMsg, setExportMsg] = useState<string | null>(null);

  const handleExportAll = async () => {
    setExportMsg(null);
    try {
      const n = await exportAllGrades(db.projects, grades, spec);
      setExportMsg(n === null ? null : `${n} fitxers de notes desats`);
    } catch (e) {
      setExportMsg(`Error en exportar: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  return (
    <section className="view">
      <h3>All projects</h3>
      <p className="hint">
        v3 formula: final ≈ work_base × quality_multiplier (quality_eff drives the multiplier).
        Click a project for size/complexity/quality breakdown and formula tree.
      </p>
      {hasGrades && (
        <div className="export-bar">
          <button type="button" onClick={() => void handleExportAll()}>
            Exporta totes les notes (.xlsx)
          </button>
          {exportMsg && <span className="hint">{exportMsg}</span>}
        </div>
      )}
      {rows.length === 0 ? (
        <p className="hint">No projects in this database.</p>
      ) : !hasGrades ? (
        <p className="hint">Projects loaded — waiting for grade engine (check errors above).</p>
      ) : null}
      {rows.length > 0 && (
        <div className="table-scroll">
          <SortableTable
            id="projects"
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
                key: "work_base",
                header: "work_base",
                sortable: true,
                numeric: true,
                sortValue: (r) => r.work_base,
                render: (r) => fmtAxis(r.work_base),
              },
              {
                key: "quality_eff",
                header: "quality_eff",
                sortable: true,
                numeric: true,
                sortValue: (r) => r.quality_eff,
                render: (r) => fmtAxis(r.quality_eff),
              },
              {
                key: "quality_multiplier",
                header: "× multiplier",
                sortable: true,
                numeric: true,
                sortValue: (r) => r.quality_multiplier,
                render: (r) => fmtAxis(r.quality_multiplier),
              },
              {
                key: "grade",
                header: "final",
                sortable: true,
                numeric: true,
                sortValue: (r) => r.grade,
                render: (r) => fmtAxis(r.grade),
              },
            ]}
          />
        </div>
      )}
    </section>
  );
}
