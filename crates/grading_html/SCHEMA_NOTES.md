# `grading.html` — snapshot schema & agent authoring guide

This is the contract between the Rust snapshot builder (`snapshot.rs`), the JS
grade engine (`assets/engine.js`), and the HTML shell (`assets/*` +
`render.rs`). It is the API surface a reviewing agent uses to add views. It
grows through the implementation phases; treat it as authoritative once Phase 3
lands.

## What this is

`grading.html` is one double-clickable, offline file. It embeds:

- a small **denormalized SQLite snapshot** (base64) built from
  `grading_xlsx::WorkbookData` + `GradingConfig`;
- **sql.js 1.12.0** (wasm) initialized via `wasmBinary` (no network);
- **math.js 14.9.1** for the ad-hoc formula box;
- **`engine.js`**, a JS port of the Rust grade arithmetic that recomputes every
  project/student grade on knob change and writes the results back into the
  in-page DB.

`grading.db` remains the single source of truth. The HTML is a *view*: the
snapshot + the recorded knob vector reproduce any grade.

## Locked design decisions

- **Architecture knobs are LIVE.** `k_crit` / `k_warn` / `arch_norm` move the
  architecture axis in-engine. The snapshot therefore carries
  `arch_crit_count` / `arch_warn_count` in `project_axis` (not just the baked
  `architecture_density`), and `engine.js` recomputes
  `arch_density = (k_crit·crit + k_warn·warn) / arch_norm`. Parity is exact at
  default knobs because the recomputed density equals the baked one there.
- **`penalty_mode` is a live control.** Seeded into `meta`, read by
  `knobsFromTables`, and gating both project and student penalties: when it is
  not `"subtractive"`, both are forced to `0` (mirrors `penalty.rs`).
- **Audience is faculty-internal.** The file embeds the whole cohort's data and
  an SQL console. Do not distribute per-student. For per-team handouts use the
  CLI `--projects` filter, which scopes the snapshot to the named teams; the
  page builds no cross-team file.

## Snapshot schema (denormalized — built by `snapshot.rs`, Phase 2 ✅)

Every table below is materialized by `build_snapshot_bytes`. Booleans are stored
as INTEGER 0/1; nullable REAL/TEXT columns are NULL when the value is absent. The
parity contract pins the exact columns.

- `meta(generated_at, weights_version, decimals, quantize_final, penalty_mode)`
- `weights(name, value)` — the 25 scalar knobs as name/value rows.
- `models(name, m)`, `levels(name, l)` — enum maps from `cfg.ai_usage`.
- `project(project_id, name, team_size)`
- `project_axis(project_id, documentation_raw, documentation_present,
  documentation_score, code_quality_raw, cc_pct, mutation_score,
  code_quality_present, code_quality_score, survival_raw, survival_present,
  survival_score, architecture_density, arch_crit_count, arch_warn_count,
  architecture_present, architecture_score)`
- `student(student_id, project_id, full_name)`
- `task(project_id, task_id, assignee_id, raw_points, model, level, declared)`
- `crit_flag(project_id, repo_full_name, kind, rule_id, severity, category)`
- `flag(project_id, student_id, sprint_id, flag_type, severity, details, source)`
  — UNION of sprint flags (`source='sprint'`) and artifact flags
  (`source='artifact'`, `sprint_id` NULL).
- `ai_detect(project_id, student_id, sprint_id, risk_level)`
- `llm_flag(project_id, student_id, sprint_id, scope, target_ref, category,
  severity, summary)`
- `reference_student(student_id, project_id, final_grade, base_grade, ai_keep,
  contribution, stu_pen, review_gate)` — Rust-computed student grades.
- `reference_project(project_id, quality_grade, quality_penalized, ai_factor,
  final_grade, review_gate)` — Rust-computed project grades. The Phase-3 parity
  self-test pins `derived_*` against these.
- `v_student` = `student ⋈ project ⋈ reference_student` (adds `project_name`,
  `team_size`, and the reference grade columns).
- `v_team` = per-project rollup over `project ⋈ reference_project` (adds
  `enrolled` = count of `student` rows for the project).
- Engine-created at runtime (Phase 3): `derived_project`, `derived_student`.

## Engine API (Phase 3)

```js
GradeEngine.knobsFromTables(db) -> knobs
GradeEngine.recompute(db, knobs)                 // writes derived_project + derived_student
GradeEngine.checkParity(db, knobs, decimals) -> { ok, maxDelta, offenders[] }
```

## Adding a view (Phase 4+)

Append one declarative entry to `const VIEWS` in `app.js`:

```js
{ id: "my_view", title: "My view", sql: "SELECT … FROM v_student …",
  chart: "table" | "bar" | "line" | "scatter" | "hist" }
```

Charts read column order by convention (label first, value(s) after). For
bespoke rendering, add `render(container, rows)` to the entry. Weights,
formulas, and views are **live** — editing them needs no binary re-run; only
new data or a new metric requires re-running `sprint-grader grading-html`.

## Regenerating the page

```sh
sprint-grader grading-html                 # whole cohort → data/entregues/grading.html
sprint-grader grading-html --projects team-01   # scoped snapshot (safe for handouts)
sprint-grader grading-html --workbook-only      # rebuild from all graded projects
```

Weights, formulas and views are **live in the page** — editing them needs no
binary re-run. Only *new data* (re-collect / re-grade) or a *new metric* (a new
snapshot column) requires re-running the command. `grading-html` shares
`grading_xlsx::grade_persist_and_load`, so it never changes grades vs
`grading-sheet` (`diff-db --derived-only` between the two is a no-op).

## Running the Node-gated tests

```sh
cargo test -p sprint-grader-grading-html --features node-tests
```

(Default `cargo test --workspace` skips these — no Node required.) The harness
(`tests/parity.mjs`) also runs standalone against any extracted snapshot:
`node crates/grading_html/tests/parity.mjs <snapshot.db>`.
