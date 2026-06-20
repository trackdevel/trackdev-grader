import { useState } from "react";

import type { RepoInventory } from "../data/diagnostics";
import type { GradeOutput, GradeSpec, LoadedDb } from "../data/types";
import { exportProjectGrades } from "../data/export";
import { axisScore, qualityEff } from "../logic/gradeAxes";
import { projectReviewGate } from "../logic/gates";
import { fmtNum } from "./SortableTable";
import { FormulaTreeList } from "./Tree";
import { studentHref, useNavHistory } from "../hooks/useHashRoute";

type Props = {
  db: LoadedDb;
  grades: Map<number, GradeOutput>;
  spec: GradeSpec;
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

/**
 * Absolute size/structure metric rows for the "Project size & structure"
 * section, in display order: size first, then component counts. `file_count`
 * comes from the inventory run, not `repo_structural_metrics`. Densities /
 * averages and EXTRA_TECH keys are intentionally excluded (the latter have
 * their own section). Unknown keys fall back to the raw key.
 */
const STRUCTURAL_METRICS: Array<{ key: string; label: string }> = [
  { key: "production_loc", label: "Production LOC" },
  { key: "production_statement_count", label: "Statements" },
  { key: "file_count", label: "Source files" },
  { key: "controller_count", label: "Controllers" },
  { key: "service_count", label: "Services" },
  { key: "entity_count", label: "Entities" },
  { key: "repository_count", label: "Repositories" },
  { key: "endpoint_count", label: "REST endpoints" },
  { key: "fragment_count", label: "Fragments" },
  { key: "activity_count", label: "Activities" },
  { key: "viewmodel_count", label: "ViewModels" },
  { key: "room_database_count", label: "Room databases" },
  { key: "custom_query_count", label: "Custom queries" },
  { key: "scheduled_task_count", label: "Scheduled tasks" },
  { key: "observe_call_count", label: "Observe calls" },
  { key: "nav_dispatch_count", label: "Navigation dispatches" },
  { key: "reactive_state_field_count", label: "Reactive state fields" },
];

/** Size rows always shown even when zero; the rest are dropped if zero
 * across every repo (decision: keep the grid honest about the headline size). */
const STRUCTURAL_SIZE_KEYS = new Set([
  "production_loc",
  "production_statement_count",
  "file_count",
]);

/** Short last segment of `owner/repo` for a compact column header. */
function repoShortName(fullName: string): string {
  const slash = fullName.lastIndexOf("/");
  return slash >= 0 ? fullName.slice(slash + 1) : fullName;
}

/** Value of one metric key for a repo; `file_count` lives off the metrics map.
 * Missing structural keys read as 0 (the scanner zero-fills present repos). */
function structuralValue(repo: RepoInventory, key: string): number {
  if (key === "file_count") return repo.file_count;
  return repo.metrics[key] ?? 0;
}

export default function ProjectDetail({ db, grades, spec, projectId }: Props) {
  const { goBack } = useNavHistory();
  const raw = db.projects.find((p) => p.project_id === projectId);
  const out = grades.get(projectId);
  const diag = db.diagnostics.get(projectId);
  const [exportMsg, setExportMsg] = useState<string | null>(null);

  if (!raw || !out) {
    return <p className="error">Project not found.</p>;
  }

  const handleExport = async () => {
    setExportMsg(null);
    try {
      const path = await exportProjectGrades(raw, out, spec);
      setExportMsg(path ? `Notes desades a ${path}` : null);
    } catch (e) {
      setExportMsg(`Error en exportar: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  const manualDefs = spec.manual_fields?.defs ?? [];
  const manualValues = spec.manual_fields?.values?.[String(projectId)] ?? {};
  const manualNotes = spec.manual_fields?.notes?.[String(projectId)] ?? {};

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

  // Absolute size/structure inventory, one column per repo. Drop component rows
  // that are zero across every repo; size rows (LOC/statements/files) always
  // show so the headline magnitude is never silently absent.
  const structural = diag?.structural ?? [];
  const structuralRows = STRUCTURAL_METRICS.filter(
    ({ key }) =>
      STRUCTURAL_SIZE_KEYS.has(key) ||
      structural.some((repo) => structuralValue(repo, key) > 0),
  );

  // Contribution breakdown: raw estimation points → raw share → AI-weighted
  // points → final contribution share. Sums are over the listed students so the
  // share columns add up to 1.0 by construction.
  const students = out.grades.students;
  const sumRaw = students.reduce((acc, s) => acc + s.raw_points, 0);
  const sumEff = students.reduce((acc, s) => acc + s.effective_points, 0);
  const sumFinalShare = students.reduce((acc, s) => acc + (s.contribution ?? 0), 0);

  return (
    <div className="detail-page">
      <button type="button" className="back-link" onClick={() => goBack("#/projects")}>
        ← Back
      </button>
      <h2>{raw.name}</h2>
      <p className="subtitle">Team size: {raw.team_size}</p>

      <div className="export-bar">
        <button type="button" onClick={() => void handleExport()}>
          Exporta notes (.xlsx)
        </button>
        {exportMsg && <span className="hint">{exportMsg}</span>}
      </div>

      <section className="detail-section">
        <h3>Team grade</h3>
        <KvTable
          pairs={[
            ["work_base", fmtAxis(axisScore(out.grades.axes, "work_base"))],
            ["quality_eff", fmtAxis(qualityEff(out.grades.axes))],
            ["quality_multiplier", fmtAxis(axisScore(out.grades.axes, "quality_multiplier"))],
            ["Team quality penalty", fmtNum(out.grades.team_quality_penalty ?? 0)],
            ["Final grade", fmtNum(out.grades.project_final)],
            ["Composite quality", fmtNum(out.grades.quality_grade)],
            ["After penalties", fmtNum(out.grades.quality_penalized)],
            ["Team AI factor", fmtNum(out.grades.ai_factor, 3)],
            ["Project penalty", fmtNum(out.grades.project_penalty)],
            ["Review gate", gate],
          ]}
        />
      </section>

      {manualDefs.length > 0 && (
        <section className="detail-section">
          <h3>Custom fields</h3>
          <table>
            <thead>
              <tr>
                <th>field</th>
                <th>value</th>
                <th>explanation</th>
              </tr>
            </thead>
            <tbody>
              {manualDefs.map((d) => {
                const override = manualValues[d.name];
                const value = override === undefined ? d.value : override;
                const note = manualNotes[d.name];
                return (
                  <tr key={d.name}>
                    <td>
                      {d.name}
                      {d.description ? <span className="hint"> — {d.description}</span> : null}
                    </td>
                    <td>
                      {fmtNum(value)}
                      {override === undefined ? <span className="hint"> (default)</span> : null}
                    </td>
                    <td className="manual-note-cell">{note ?? ""}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </section>
      )}

      <section className="detail-section">
        <h3>Quality axes</h3>
        <KvTable pairs={axisPairs} />
      </section>

      {structural.length > 0 && (
        <section className="detail-section">
          <h3>Project size &amp; structure</h3>
          <p className="hint">
            Absolute counters from the structural inventory — how big and complex the delivered
            code is. One column per repository; component counts that are zero everywhere are
            hidden. Display-only; not a grade input.
          </p>
          <table>
            <thead>
              <tr>
                <th>metric</th>
                {structural.map((repo) => (
                  <th key={repo.repo_full_name} title={repo.repo_full_name}>
                    {repoShortName(repo.repo_full_name)}
                    {repo.status !== "OK" ? <span className="hint"> ({repo.status})</span> : null}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {structuralRows.map(({ key, label }) => (
                <tr key={key}>
                  <th>{label}</th>
                  {structural.map((repo) => (
                    <td key={repo.repo_full_name}>
                      {fmtNum(structuralValue(repo, key), 0)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {((out.grades.extra_tech ?? 0) > 0 || (diag?.technologies?.length ?? 0) > 0) && (
        <section className="detail-section">
          <h3>Extra technologies vs. baseline</h3>
          <KvTable
            pairs={[
              ["extra_tech (weighted)", fmtNum(out.grades.extra_tech ?? 0)],
              ...(out.grades.extra_tech_components ?? []).map(
                (c) =>
                  [
                    c.key,
                    `${fmtNum(c.raw)} × ${fmtNum(c.weight)} = ${fmtNum(c.contribution)}`,
                  ] as [string, string],
              ),
            ]}
          />
          {(diag?.technologies?.length ?? 0) > 0 && (
            <table>
              <thead>
                <tr>
                  <th>technology</th>
                  <th>category</th>
                  <th>source</th>
                  <th>depth</th>
                  <th>evidence</th>
                </tr>
              </thead>
              <tbody>
                {(diag?.technologies ?? []).map((t) => (
                  <tr key={`${t.repo_full_name}:${t.category}:${t.technology}`}>
                    <td>{t.technology}</td>
                    <td>{t.category}</td>
                    <td>{t.source}</td>
                    <td>{fmtNum(t.depth)}</td>
                    <td className="hint">{t.evidence ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      )}

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
        <h3>Contribution breakdown</h3>
        <p className="hint">
          Raw estimation points give each student a raw share of the team; the declared-AI
          discount turns them into AI-weighted points, whose shares are the final contribution
          used for grading. Both “contribution” columns are fractions of 1.0.
        </p>
        <table>
          <thead>
            <tr>
              <th>student</th>
              <th>raw points</th>
              <th>raw contribution</th>
              <th>weighted points (AI)</th>
              <th>final contribution</th>
            </tr>
          </thead>
          <tbody>
            {[...students]
              .sort((a, b) => b.effective_points - a.effective_points)
              .map((s) => {
                const meta = raw.students.find((x) => x.student_id === s.student_id);
                const rawShare = sumRaw > 0 ? s.raw_points / sumRaw : null;
                return (
                  <tr key={s.student_id}>
                    <td>
                      <a className="entity-link" href={studentHref(projectId, s.student_id)}>
                        {meta?.full_name ?? s.student_id}
                      </a>
                    </td>
                    <td>{fmtNum(s.raw_points, 2)}</td>
                    <td>{rawShare != null ? fmtNum(rawShare, 3) : "—"}</td>
                    <td>{fmtNum(s.effective_points, 2)}</td>
                    <td>{s.contribution != null ? fmtNum(s.contribution, 3) : "—"}</td>
                  </tr>
                );
              })}
            <tr className="totals-row">
              <th>Team total</th>
              <td>{fmtNum(sumRaw, 2)}</td>
              <td>{sumRaw > 0 ? fmtNum(1, 3) : "—"}</td>
              <td>{fmtNum(sumEff, 2)}</td>
              <td>{sumEff > 0 ? fmtNum(sumFinalShare, 3) : "—"}</td>
            </tr>
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
