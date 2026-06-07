# Plan — `grading-sheet` + `grading_xlsx` + declared-AI points modulation (+ feedback-only LLM quality flags)

## Motivation and goals

Add one read-mostly subcommand, `sprint-grader grading-sheet`, that reads
`grading.db` and writes a single self-recalculating `.xlsx` holding **one
project grade (0–10) per team** and **one student grade (0–10) per student**,
both absolute and comparable across projects, derived **only** from TrackDev +
GitHub evidence already in `grading.db`. No exams, peer/self scores, or manual
entry.

Two ideas drive the model:

1. **Quality is measured once, at the project level.** Documentation, code
   quality, code survival, and architecture conformance are measured identically
   in every repository, so they carry cross-team comparability. The team gets
   **one** quality grade `Q`.
2. **Points only split work inside a team, after an AI discount.** Story points
   are team-subjective, so they never enter cross-team comparison; they decide
   *who pulled their weight* within a team. Each task's points are first
   discounted by the student's declared AI usage (the TrackDev "Ús de IA"
   attribute), and a student's grade is the project grade scaled by their share
   of the team's AI-discounted effective points.

A second, independent track adds **feedback-only LLM quality flags**: advisory
context surfaced next to the grade and **never** a grade input, because LLM
output is non-deterministic. The grade pipeline never reads `llm_quality_flag`.

The grade model below is authoritative; implement exactly this arithmetic in
both the Rust assembler and the workbook formulas.

### The grade model (implement exactly)

**Per-task AI discount.** For a DONE, non-`USER_STORY` task `t` with raw
estimation points `raw_t` and declared "Ús de IA" = (model, level):

- `m` = model scalar in `[0,1]` (`Cap`/none = 0 … frontier = 1), from
  `config [ai_usage.models]` — an **explicit** shipped mapping. TrackDev enum
  *order* is **not** capability order (the live "Model IA" enum has `Cap`
  mid-list), so there is no enum-order default; an unmapped model logs a `warn`
  and falls back to `m = 1.0` (treat unknown as frontier — conservative, never
  under-penalises).
- `l` = level scalar in `[0,1]` (`A`=0, `B`=.25, `C`=.5, `D`=.75, `E`=1), from
  `config [ai_usage.levels]`.
- `α` = global strength (`config [ai_usage].strength`, default **1.0**, range
  `[0,1]`); `floor_keep` (default 0.20) sets the maximum discount.
- **`keep_t = 1 − (1 − floor_keep)·α·m·l`** (default span 0.80 ⇒ `1 − 0.8·α·m·l`).
- **`effective_t = raw_t · keep_t`**.
- **Undeclared** task → flag `MISSING_AI_DECLARATION` (WARNING) **and** apply a
  configurable assumed `(undeclared_model_m, undeclared_level_l)` (default
  frontier × C ⇒ keep ≈ 0.60 at α=1). Never silently keep 100%.

`α = 0` ⇒ every `keep_t = 1` ⇒ points unchanged. `α = 1` ⇒ full grid
(`Cap`→1.0, frontier×E→`floor_keep`).

**Aggregates** over `sprint_ids_up_to_current(project, today)`, DONE
non-`USER_STORY` tasks, attributed to the task's single `assignee_id`:

- `raw_u = Σ raw_t`, `eff_u = Σ effective_t` (sum over student `u`'s tasks).
- `Σraw = Σ_u raw_u`, `Σeff = Σ_u eff_u` (team totals).
- `N` = team size (`students.team_project_id = project_id`; default **enrolled**).
- `mean_raw = Σraw / N`.

**Project grade.**

- `Q` = quality composite (0–10): weighted, present-renormalized mean of the
  four project-level quality sub-scores (documentation, code_quality, survival,
  architecture); weights `[weights.project]`, normalization `[normalization]`,
  computed at **team** grain.
- `project_penalty = MIN(max_penalty_points, Σ CRITICAL static-analysis +
  Σ CRITICAL complexity findings)` at the repo/team level (full weight per repo;
  `+ security_extra` per CRITICAL finding whose `category = 'security'`).
- `Q_pen = CLAMP(Q − project_penalty, 0, 10)`.
- `A` (team AI factor) `= Σeff / Σraw` (points-weighted mean keep; `1.0` if
  `Σraw = 0`).
- **`project_final = Q_pen · A`** — the reported team grade, and the team mean of
  the student grades.

**Student grade.**

- `r_u = eff_u / Σeff` (contribution ratio; Σ over team = 1).
- `base_u = Q_pen · A · r_u · N`. Algebraic identity (implement this form):
  `A·r_u·N = (Σeff/Σraw)·(eff_u/Σeff)·N = eff_u·N/Σraw = eff_u / mean_raw`, so
  **`base_u = Q_pen · eff_u / mean_raw`**. The team AI factor *cancels* for
  individuals: each student's grade reflects **their own** declared AI, not
  teammates'. The reported project grade still shows team AI (`Q_pen·A`), and
  `mean_u(base_u) = Q_pen·A = project_final`.
- `student_penalty_u = MIN(student_penalty_cap, Σ that student's CRITICAL
  behavioural flags)` from `flags` (real students only — **exclude synthetic
  `PROJECT_*` rows**) `+ student_artifact_flags`.
- **`final_u = CLAMP(base_u − student_penalty_u, 0, 10)`**.

**Gates** (review routes recorded in `review_gate`, not silent zeros):

- **No-delivery** (automatic): `eff_u = 0` cumulative ⇒ `final_u = 0`,
  `review_gate = 'NO_DELIVERY'`. The formula already yields 0; the label is
  informational.
- **Plagiarism**: a `CROSS_TEAM_SIMILARITY` synthetic `PROJECT_<id>` row in
  `flags` ⇒ `review_gate = 'PLAGIARISM'` on the project **and all members**; **no
  auto-zero** (instructor decides).
- **AI honesty cross-check**: when `student_sprint_ai_usage.risk_level = 'HIGH'`
  for a student/sprint **and** that student's declared AI is low (aggregated
  level ≤ `B` or undeclared), emit `AI_DECLARATION_MISMATCH` and set
  `review_gate = 'AI_REVIEW'`; **no automatic grade change** by default
  (detection is probabilistic). Setting `ai_mismatch_auto_apply_worstcase = true`
  substitutes the worst-case assumed discount. Declared and detected agreeing ⇒
  no action.

**Worked example** (pin this in the Wave 3 test). Alice (no AI, raw 10 ⇒ eff 10)
+ Bob (frontier×E, keep 0.2, raw 10 ⇒ eff 2), `Q_pen = 8`, `N = 2`:
`A = 12/20 = 0.6` ⇒ **project = 4.8**; `mean_raw = 10`; Alice `= 8·10/10 = 8.0`;
Bob `= 8·2/10 = 1.6`; mean `= 4.8`. The honest student recovers the quality
grade; the AI-heavy student absorbs the discount.

## Architecture and component breakdown

The `grading_xlsx` crate and `crates/grading_xlsx/SCHEMA_NOTES.md` **do not yet
exist** in the tree and are created by this plan; the crate is not yet a
workspace member. `quality_llm` (Track B) is likewise new.

New crates: `grading_xlsx` (Track A), `quality_llm` (Track B). Edited crates:
`collect` (AI-attribute capture), `core` (schema + a shared default constant),
`analyze` (one new flag detector), `orchestration` (parity-exclusion test),
`cli` (two subcommands). New config `config/grading.toml`; new system prompt
`config/quality-llm-rubric.md` (Track B).

**Reuse map (verified against source).**

| Need | Reuse |
|---|---|
| Open/migrate DB, schema | `core::db::Database::{open,create_tables}`, `core::db::apply_schema` (db.rs:12,32,44) — `create_tables` runs additive migrations then `execute_batch(SCHEMA_SQL)`, so new `CREATE TABLE IF NOT EXISTS` blocks auto-apply with no migration entry. |
| Cumulative sprint set | `Database::sprint_ids_up_to_current(project_id, today) -> Vec<i64>` (db.rs:551) |
| PR→student (TrackDev-scoped) | `pr_authors` view `(pr_id, student_id, author_points, author_task_count)` (schema.sql:1091) |
| Student→project, team size, name | `students.team_project_id`, `students.full_name` |
| Project name / slug | `projects.name`, `projects.slug` |
| Blame-weighted findings (arch axis + SA/CX penalty) | `core::rule_attribution::load_attributed_findings_for_repo(conn, repo, RuleKind)` (rule_attribution.rs:77) — returns `AttributedFinding { finding: RuleFinding, attributions }`; owns the `architecture_violations.rowid` join and the `*_attribution.finding_id` joins. `RuleKind::{Architecture, Complexity, StaticAnalysis}` select the axis. |
| SA `category` (not on `RuleFinding`) | direct read of `static_analysis_findings.category` (schema.sql:928; values `'style' \| 'bug' \| 'security' \| …`) |
| Tasks + points + assignee + sprint | `tasks` (`estimation_points`, `assignee_id`, `sprint_id`, `type`, `status`) |
| Declared AI (model, level) per task | collected from `/export/tasks` sibling `attributeValues[]` (ENUM_PAIR `value`/`valueB`) → new `task_ai_usage` table |
| TrackDev export client | `collect::pm_client::TrackDevClient::get_project_export_tasks` (already called in `collector::collect_project_via_exports`, collector.rs:322) |
| Basic xlsx idioms | `crates/report/src/xlsx.rs` (`Workbook::new`, `add_worksheet().set_name`, `write_string`, `write_number(_with_format)`, `write_url_with_format`, `Url`, `Format`, the `to_rusqlite` error shim). Formula / defined-name / data-validation / protection are **not** used there and are new — implement fresh on `rust_xlsxwriter 0.94`. |
| LLM via subscription (Track B) | `evaluate::{ClaudeCliClient, CursorCliClient}`, the JSON extractor `evaluate::llm_eval::extract_json_object`, `evaluate_local::OllamaClient` |
| CLI shape | `Command` enum (cli/src/main.rs:210) + `ProjectsArg` + `parse_project_filter` + `resolve_all_sprint_tuples` (re-exported from `orchestration::pipeline`) + the `entregues_dir` local (`crates/cli/src/main.rs`) |
| Parity exemption | `orchestration::db_diff::{DERIVED_TABLES, COLLECTION_TABLES}` (db_diff.rs:18,64) — new tables stay out of both lists. |

**Schema facts that shape the design.** Architecture/SA/complexity findings are
artifact-grain (repo→project via the PR graph, not `author_id`); `RuleFinding`
has no `category`; `CROSS_TEAM_SIMILARITY` is a synthetic `PROJECT_<id>` row in
the sprint-keyed `flags` table `(id, student_id, sprint_id, flag_type, severity,
details)`; `student_artifact_flags` is project-keyed `(id, student_id,
project_id, flag_type, severity, details)`; `student_sprint_ai_usage` carries
`risk_level TEXT` with PK `(student_id, sprint_id)`. These are pinned in
`SCHEMA_NOTES.md` in Wave 0.

**TrackDev "Ús de IA" shape.** A first-class `ENUM_PAIR` ProfileAttribute. In
`/export/tasks`, each entry carries a **sibling** `attributeValues[]` array (a
sibling of `entry["task"]`, **not** nested inside it). The AI element has
`attributeName = "Ús de IA"`, `attributeType = "ENUM_PAIR"`, `value` = model
(slot 1), `valueB` = level (slot 2), and inline `enumValues` / `enumValues2`
domains. Model strings/order are professor-instance data → mapped to `m` via
config.

**Repo→project mapping** (DB-only, robust to NULL `author_id`):

```sql
SELECT DISTINCT s.team_project_id FROM pull_requests pr
JOIN pr_authors pa ON pa.pr_id = pr.id
JOIN students s ON s.id = pa.student_id
WHERE pr.repo_full_name = ? AND s.team_project_id IS NOT NULL;
```

## Implementation steps

Run the **same four-command verification gate after every wave**, in order:

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `cargo run --bin sprint-grader -- diff-db --derived-only <a> <b>` (stays clean;
   the new tables never enter the compared set).

Each wave ends in one commit. Implement the waves in order.

### Wave 0 — schema audit document

Create `crates/grading_xlsx/SCHEMA_NOTES.md` (the directory does not exist yet;
create it). Pin, with exact column names read from `crates/core/src/schema.sql`
and the loaders:

- Each bound column to its grain (per-task / per-student / per-sprint /
  artifact-grain repo) and the repo→project mapping query above.
- The loader bindings: `load_attributed_findings_for_repo` returns
  `AttributedFinding` (severity on `finding.severity`; per-student shares on
  `attributions`), and the SA-`category` gap (`category` lives only on
  `static_analysis_findings`, not on `RuleFinding`).
- The gate-flag storage: `CROSS_TEAM_SIMILARITY` synthetic `PROJECT_<id>` row in
  `flags`; `ZERO_TASKS`/`MISSING_AI_DECLARATION` per-sprint real-student in
  `flags`; behavioural CRITICAL rows in `flags` + `student_artifact_flags`.
- The **declared-AI bindings**: the `/export/tasks` sibling `attributeValues[]`
  ENUM_PAIR shape; the new `task_ai_usage` / `ai_usage_enum_domain` columns; the
  model→`m` config mapping; and the `student_sprint_ai_usage.risk_level` source
  for the cross-check.
- The **exact raw inputs for the four quality axes** at team grain, all verified
  present in `schema.sql`, so Wave 3 replicates them rather than re-deriving:
  - documentation raw = team mean of `pr_doc_evaluation.total_doc_score`
    (schema.sql:213).
  - code quality raw = `student_sprint_quality.avg_maintainability`
    (schema.sql:324), with the cc term from `pct_methods_cc_over_10`
    (schema.sql:323) and the test term from `pr_mutation.mutation_score`
    (schema.sql:887).
  - survival raw = team mean of `student_sprint_survival.survival_rate_normalized`
    (schema.sql:162).
  - architecture raw = severity-weighted density of
    `load_attributed_findings_for_repo(conn, repo, RuleKind::Architecture)`
    (CRITICAL/WARNING counts).

*Commit:* `chore(grading_xlsx): pin schema + AI-attribute bindings from audit`.

### Wave 1 — schema additions (single owner of `schema.sql`)

Append to `crates/core/src/schema.sql` (idempotent `CREATE TABLE IF NOT EXISTS`,
with house comment blocks) the seven new tables: the AI tables (`task_ai_usage`,
`ai_usage_enum_domain`), the grade tables (`project_final_grade`,
`student_final_grade`, `project_component_score`, `student_component_score`), and
`llm_quality_flag` (Track B). DDL in **Data structures**. No
`apply_additive_migrations` entry is needed — these are whole new tables and
`create_tables` re-runs `SCHEMA_SQL`.

Add a shared constant in `crates/core` (e.g. `core::config` or a small
`core::ai_usage` module): `pub const DEFAULT_AI_ATTRIBUTE_NAME: &str = "Ús de
IA";` so `collect` and `grading_xlsx` reference one literal.

Keep **all seven** new tables out of `DERIVED_TABLES` **and** `COLLECTION_TABLES`
in `crates/orchestration/src/db_diff.rs` (grade output is downstream; AI tables
are collected inputs/diagnostics deliberately excluded from the dual-run parity
contract). Add an `orchestration` unit test asserting none of the seven names
appears in either list.

*Acceptance:* `apply_schema` succeeds on a fresh in-memory DB and on an existing
populated DB (additive); the exclusion test is green; `diff-db --derived-only`
unchanged. *Commit:* `feat(core): grade + AI-usage + llm_quality_flag tables`.

### Wave 2 — declared-AI collection + `MISSING_AI_DECLARATION`

- **`crates/core/src/db.rs`**: add `Database::upsert_task_ai_usage(task_id,
  model_value: Option<&str>, level_value: Option<&str>, declared: bool,
  captured_at: &str)` and `Database::upsert_ai_usage_enum_value(slot: i64, value:
  &str, description: Option<&str>, ord: Option<i64>)`, both `INSERT OR REPLACE`
  keyed on PK, mirroring the existing `upsert_pr_commit` idiom (db.rs:482).
- **`crates/collect/src/collector.rs`**: in `collect_project_via_exports`, inside
  the `for entry in entries` loop (after the `db.upsert_task(...)` call), read the
  **sibling** `entry.get("attributeValues")` array; find the element whose
  `attributeName == ai_attribute_name` (default
  `core::DEFAULT_AI_ATTRIBUTE_NAME`), validated `attributeType == "ENUM_PAIR"`;
  upsert `task_ai_usage(task_id, model_value = value, level_value = valueB,
  declared = both slots present and non-empty, captured_at = today)`. Upsert
  `ai_usage_enum_domain` from `enumValues` (slot 1) and `enumValues2` (slot 2):
  `(slot, value, description, ord = array index)`. Thread the attribute name via
  a new optional `CollectOpts.ai_attribute_name` (defaulting to the core
  constant) so a renamed instance attribute is configurable without touching
  `collect`. **`collect` must not depend on `config/grading.toml`** — that file
  ships in Wave 3, so the attribute name comes from the core constant or
  `CollectOpts`, never from the grading config.
- **`crates/analyze/src/flags.rs`**: add detector `fn missing_ai_declaration(conn,
  sprint_id) -> rusqlite::Result<Vec<Flag>>` reading `tasks ⋈ task_ai_usage`,
  emitting `MISSING_AI_DECLARATION` (severity **WARNING**) per `(assignee_id,
  sprint_id)` for DONE non-`USER_STORY` tasks lacking a `task_ai_usage` row with
  `declared = 1`; `details` lists the undeclared `task_key`s and a count. Register
  it in `detect_flags_for_sprint_id` (flags.rs:2709) with
  `total += run!("MISSING_AI_DECLARATION", missing_ai_declaration(conn,
  sprint_id));`.

*Acceptance:* a `collect` unit test over a fixture `/export/tasks` JSON (with a
sibling ENUM_PAIR `attributeValues` entry) populates `task_ai_usage` +
`ai_usage_enum_domain`; a new `crates/analyze/tests/flag_missing_ai_declaration.rs`
test (reusing the `crates/analyze/tests/common/` harness) asserts WARNING on an
undeclared DONE task and silence when declared. *Commit:* `feat(collect,analyze):
capture Ús-de-IA + MISSING_AI_DECLARATION`.

### Wave 3 — `grading_xlsx`: config + aggregation + modulation + grade

Create `crates/grading_xlsx` and add it to `[workspace] members` in the root
`Cargo.toml`. Dependencies: `sprint-grader-core` (path), `rust_xlsxwriter =
"0.94"` (MSRV-1.80 pin — do **not** bump), and the workspace `serde`, `toml`,
`anyhow`, `tracing`, `rusqlite`, `chrono`; plus `sha2 = "0.10"`. Add the
`config/grading.toml` defaults (see **`config/grading.toml`** below).

Modules:

- `config.rs`: `GradingConfig` (serde) loaded from `config/grading.toml` with
  full defaults when the file is absent. Sub-structs: `weights_project`,
  `ai_usage` (`strength`, `floor_keep`, `attribute_name`, `undeclared_model_m`,
  `undeclared_level_l`, `models: BTreeMap<String,f64>`, `levels:
  BTreeMap<String,f64>`), `penalty`, `gate`, `normalization`, `output`.
  `weights_version()` = sha256 of the canonically re-serialized TOML.
- `modulation.rs`: pure `keep(m, l, strength, floor_keep) -> f64` and the
  model/level→scalar resolution (explicit `config` map; an unmapped model logs a
  `warn` and uses `m = 1.0` — never an enum-order guess).
- `aggregate.rs`: project-level quality raw metrics (team grain) **and**
  per-student raw/effective points (apply `modulation` per task; undeclared →
  assumed `(m,l)`). Each quality axis yields `(raw: Option<f64>, present: bool)`;
  a missing axis renormalizes out, never scoring 0. Aggregation spans
  `sprint_ids_up_to_current`, DONE non-`USER_STORY` tasks, by `assignee_id`.
- `normalize.rs`: the four quality axes → 0–10 using `[normalization]` anchors,
  computed at team grain, from the columns pinned in Wave 0:
  - documentation: `10 · CLAMP(doc_raw / doc_max, 0, 1)`, `doc_raw` = team mean of
    `pr_doc_evaluation.total_doc_score`.
  - code_quality: maintainability-anchored `10 · CLAMP((mi − mi_floor) /
    (mi_ceiling − mi_floor), 0, 1)` from `student_sprint_quality.avg_maintainability`,
    adjusted by the cc penalty (`cc_penalty` against `pct_methods_cc_over_10`)
    and a test bonus (`test_bonus`, capped at `test_cap`, from
    `pr_mutation.mutation_score`).
  - survival: `10 · CLAMP((surv − surv_floor) / (surv_ceiling − surv_floor),
    0, 1)`, `surv` = team mean of `student_sprint_survival.survival_rate_normalized`.
  - architecture: `10 − MIN(10, arch_density)` with `arch_density = (k_crit ·
    crit_count + k_warn · warn_count) / arch_norm`, counts from
    `load_attributed_findings_for_repo(conn, repo, RuleKind::Architecture)`.
  Contribution is **no longer** a normalized axis — it is the effective-points
  ratio in the student formula.
- `penalty.rs`: project penalty = `MIN(max_penalty_points, Σ CRITICAL SA +
  Σ CRITICAL CX via the loader, + security_extra per CRITICAL
  `category='security'` finding read directly)`; per-student behavioural penalty
  = `MIN(student_penalty_cap, Σ CRITICAL flags + student_artifact_flags)`,
  excluding synthetic `PROJECT_*` rows.
- `grade.rs`: assemble `Q`, `Q_pen`, `A`, `project_final`, per-student `base_u`,
  `student_penalty_u`, `final_u`, and the gates (`review_gate`: `NO_DELIVERY`,
  `PLAGIARISM`, `AI_REVIEW`).
- `persist.rs`: write `project_final_grade`, `student_final_grade`,
  `project_component_score`, and `student_component_score` (the last for
  **diagnostic** per-student quality axes — feedback only). `DELETE WHERE
  project_id = ?` before insert; idempotent re-persist yields byte-identical rows.

*Acceptance:* `cargo test -p sprint-grader-grading-xlsx` builds an in-memory DB
via `apply_schema`, seeds multi-sprint fixtures incl. `task_ai_usage`, and
asserts: per-task `keep` + effective points; `Q`/`Q_pen`/`A`/`project_final`;
per-student `base_u`/`final_u` (incl. the worked example to `decimals`); the
three gates; the `MISSING_AI_DECLARATION` assumed-discount path; and idempotent
re-persist. *Commit:* `feat(grading_xlsx): config + AI modulation +
project/student grades`.

### Wave 4 — `grading_xlsx`: self-recalculating workbook

`workbook.rs::write_workbook(projects, students, points, crit, llm_flags, cfg,
out)`. Sheets (input cells **bold** + unlocked; everything else live formulas):

1. **`Weights`** — `[weights.project]`, `[ai_usage]` (`α`, `floor_keep`, the
   model→`m` and level→`l` tables, undeclared assumed `(m,l)`), penalty caps,
   normalization anchors. Defined names (`w_doc`, `ai_strength`, …) via
   `Workbook::define_name`; weight/anchor cells `set_unlocked(true)` +
   `DataValidation` decimal `Between`.
2. `ProjectGrades` — per team: four quality sub-scores, `Q`, `project_penalty`,
   `Q_pen`, `ai_factor A`, `final` (`=Q_pen*A`), `team_size`, `review_gate`.
3. `StudentGrades` — per student: `raw_points`, `effective_points`,
   `ai_keep_factor`, `contribution_ratio`, `base` (`=Q_pen*eff/mean_raw` via
   `INDEX`/`MATCH`), `student_penalty`, `final` (`=MEDIAN(0, base−penalty, 10)`),
   `review_gate`.
4. `AI_Usage` — per task: model, level, `m`, `l`, `keep`, `raw_pt`,
   `effective_pt`; per-team `Σeff` / `Σraw` / `A` / `mean_raw`. Drives the points
   columns the grade sheets pull.
5. `Docs` / `Quality` / `Survival` / `Architecture` — team-grain raw +
   `score_0_10` + `present`; `ProjectGrades` pulls them.
6. `CritFlags` — one row per attributed critical finding + `penalty_contribution`;
   penalty cells `SUMIFS` over it, capped via `MIN`.
7. `Flags`, `AI_Detect` — diagnostic context driving the gates.
8. `LLM_Flags` (Track B; header-only until then) and `Methodology` (legend; the
   "quality compares across teams / AI-discounted points split inside a team"
   explanation; the team-AI-cancellation note; `generated_at`;
   `weights_version`; "LLM flags are advisory, never grade inputs").

Mechanics: `Worksheet::write_formula` (US commas, English function names); cache
results with `Formula::set_result` so the file shows values before any
recalculation; scalar functions only (`SUMPRODUCT`, `SUMIFS`, `IF`, `MEDIAN`,
`MIN`, `MAX`, `ROUND`, `MROUND`, `INDEX`, `MATCH`); `CLAMP(x)` ⇒ `MEDIAN(0, x,
10)` uniformly; guard zero denominators with `IF`; protect every computed sheet
and unlock only input cells; apply `MROUND` only when `output.quantize_final > 0`.

*Acceptance:* a golden test over `Workbook::save_to_buffer`; a hand-checked
student **and** team that recompute (Rust side) to the persisted grade to
`output.decimals`; an assertion that the expected defined names exist. *Commit:*
`feat(grading_xlsx): self-recalculating workbook writer`.

### Wave 5 — CLI wiring (single owner of `main.rs`)

Add `Command::GradingSheet { projects, out: Option<PathBuf>, import_weights:
Option<PathBuf> }` and `Command::QualityFlags { projects, max_holistic:
Option<usize>, resume: bool }` to the `Command` enum (cli/src/main.rs:210).
Resolve projects via `parse_project_filter` + `resolve_all_sprint_tuples(&db,
&today, filter.as_deref())`. Dispatch `GradingSheet` →
`grading_xlsx::run(&db, &config_dir, &opts)` where `RunOpts.out` defaults to
`entregues_dir.join("grading_sheet.xlsx")` and `RunOpts.today` is the resolved
`today`. `--import-weights <path>` reads an edited `Weights` sheet back into
`config/grading.toml` and exits without grading. `QualityFlags` →
`quality_llm::run` (Track B). `grading-sheet` stays fully deterministic and does
**not** trigger the LLM pass.

Add `sprint-grader-grading-xlsx` (and later `sprint-grader-quality-llm`) as a
dependency in `crates/cli/Cargo.toml` so `main.rs` can call into the new crates.

*Acceptance:* `sprint-grader --help` lists both subcommands; a run writes the
workbook and populates the four grade tables; `diff-db --derived-only` is
unaffected. *Commit:* `feat(cli): wire grading-sheet and quality-flags`.

### Wave 6 — docs

Update `README.md`: the two new subcommands; `config/grading.toml` (incl.
`[ai_usage]`) versus the unrelated `course.toml [grading]`; the grade model
(project quality × AI-discounted contribution, with the team-AI-cancellation
note); that the grade + AI tables are intentionally gate-exempt from the parity
contract; and that `task_ai_usage` requires a fresh `collect` to populate.

*Commit:* `docs(grading_xlsx): grading model + AI modulation + gate semantics`.

### Track B — feedback-only LLM quality flags (layered after Wave 6)

Each sub-step keeps the four-command gate and is independently committed.

- **PA** — add a `[quality_llm]` config block to `core::config` (backend,
  pinned cheap `model_id`, `prompt_version`, holistic cap, batching/pre-filter
  knobs); leave the crate building.
- **PB** — create `crates/quality_llm` (workspace member) with a
  `ClaudeCliClient` bulk pass over delivered files, a deterministic pre-filter,
  resumable progress, writing `llm_quality_flag` rows. Reuse
  `evaluate::llm_eval::extract_json_object` for parsing.
- **PC** — add a holistic per-project tier behind `max_holistic`.
- **PD** — add `CursorCliClient` and `evaluate_local::OllamaClient` backends
  selectable by the `[quality_llm]` backend knob.
- **PE** — populate the `LLM_Flags` sheet (additive edit to
  `grading_xlsx/workbook.rs`) and ship `config/quality-llm-rubric.md` + README.
  The grade never reads `llm_quality_flag`.

*Acceptance:* stub-CLI tests for `quality_llm::run`; the workbook golden updated
to include a populated `LLM_Flags` sheet.

## Data structures and API surface

New/changed DDL (appended to `crates/core/src/schema.sql`):

```sql
-- Declared AI usage per task, from TrackDev "Ús de IA" ENUM_PAIR attribute.
CREATE TABLE IF NOT EXISTS task_ai_usage (
    task_id     INTEGER PRIMARY KEY,
    model_value TEXT,            -- slot 1 (value); NULL when undeclared
    level_value TEXT,            -- slot 2 (valueB); NULL when undeclared
    declared    INTEGER NOT NULL DEFAULT 0,  -- 1 iff both slots present
    captured_at TEXT);
CREATE TABLE IF NOT EXISTS ai_usage_enum_domain (
    slot INTEGER NOT NULL,       -- 1 = model, 2 = level
    value TEXT NOT NULL, description TEXT, ord INTEGER,
    PRIMARY KEY (slot, value));

CREATE TABLE IF NOT EXISTS project_final_grade (
    project_id INTEGER PRIMARY KEY,
    quality_grade REAL NOT NULL, project_penalty REAL NOT NULL DEFAULT 0,
    quality_penalized REAL NOT NULL, ai_factor REAL NOT NULL DEFAULT 1,
    final_grade REAL NOT NULL, team_size INTEGER NOT NULL,
    review_gate TEXT, ai_strength REAL, weights_version TEXT, generated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS student_final_grade (
    student_id TEXT NOT NULL, project_id INTEGER NOT NULL,
    raw_points REAL NOT NULL DEFAULT 0, effective_points REAL NOT NULL DEFAULT 0,
    ai_keep_factor REAL, contribution_ratio REAL, base_grade REAL NOT NULL,
    student_penalty REAL NOT NULL DEFAULT 0, final_grade REAL NOT NULL,
    review_gate TEXT, weights_version TEXT, generated_at TEXT NOT NULL,
    PRIMARY KEY (student_id, project_id));
CREATE TABLE IF NOT EXISTS project_component_score (
    project_id INTEGER NOT NULL, component_key TEXT NOT NULL,
    raw_value REAL, score_0_10 REAL, present INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (project_id, component_key));
CREATE TABLE IF NOT EXISTS student_component_score (  -- DIAGNOSTIC ONLY (feedback, not a grade input)
    student_id TEXT NOT NULL, project_id INTEGER NOT NULL, component_key TEXT NOT NULL,
    raw_value REAL, score_0_10 REAL, present INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (student_id, project_id, component_key));
CREATE TABLE IF NOT EXISTS llm_quality_flag (
    id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, student_id TEXT, sprint_id INTEGER,
    scope TEXT NOT NULL, target_ref TEXT, category TEXT NOT NULL, severity TEXT NOT NULL,
    summary TEXT NOT NULL, detail TEXT, backend TEXT NOT NULL, model_id TEXT NOT NULL,
    prompt_version TEXT, generated_at TEXT NOT NULL);
```

`crates/core` additions:

```rust
pub const DEFAULT_AI_ATTRIBUTE_NAME: &str = "Ús de IA";
impl Database {
    pub fn upsert_task_ai_usage(&self, task_id: i64, model_value: Option<&str>,
        level_value: Option<&str>, declared: bool, captured_at: &str) -> anyhow::Result<()>;
    pub fn upsert_ai_usage_enum_value(&self, slot: i64, value: &str,
        description: Option<&str>, ord: Option<i64>) -> anyhow::Result<()>;
}
```

`crates/grading_xlsx` public surface:

```rust
pub struct GradingConfig { weights_project, ai_usage, penalty, gate, normalization, output }
impl GradingConfig { pub fn load(cfg_dir:&Path)->anyhow::Result<Self>; pub fn weights_version(&self)->String; }
pub struct AiUsageConfig { strength:f64, floor_keep:f64, attribute_name:String,
    undeclared_model_m:f64, undeclared_level_l:f64, models:BTreeMap<String,f64>, levels:BTreeMap<String,f64> }
pub fn keep(m:f64, l:f64, strength:f64, floor_keep:f64) -> f64;          // modulation.rs
pub struct ComponentScore { key:&'static str, raw_value:Option<f64>, score_0_10:Option<f64>, present:bool }
pub struct ProjectGradeRow { project_id, name, components:Vec<ComponentScore>, quality_grade,
    project_penalty, quality_penalized, ai_factor, final_grade, team_size, review_gate:Option<String> }
pub struct StudentGradeRow { student_id, project_id, full_name, raw_points, effective_points,
    ai_keep_factor:Option<f64>, contribution_ratio:Option<f64>, base_grade, student_penalty,
    final_grade, review_gate:Option<String> }
pub struct RunOpts { project_filter:Option<Vec<String>>, out:Option<PathBuf>, import_weights:Option<PathBuf>, today:String }
pub fn run(db:&Database, cfg_dir:&Path, opts:&RunOpts) -> anyhow::Result<PathBuf>;
pub fn write_workbook(/* projects, students, points, crit, llm_flags, cfg, out */) -> anyhow::Result<()>;
pub fn import_weights(xlsx:&Path) -> anyhow::Result<GradingConfig>;
```

`crates/collect` change: `CollectOpts` gains `ai_attribute_name: Option<String>`
(defaults to `core::DEFAULT_AI_ATTRIBUTE_NAME`), consumed in
`collect_project_via_exports`.

### `config/grading.toml` (defaults the crate ships and reads)

```toml
[weights.project]            # the project quality composite (cross-comparable)
documentation = 0.25
code_quality  = 0.30
survival      = 0.20
architecture  = 0.25

[ai_usage]
strength            = 1.0    # α ∈ [0,1]; 0 = points as gathered, 1 = full grid
floor_keep          = 0.20   # min kept fraction (frontier×E at α=1) ⇒ span 0.80
attribute_name      = "Ús de IA"
undeclared_model_m  = 1.0    # assumed model scalar for undeclared tasks (frontier)
undeclared_level_l  = 0.50   # assumed level scalar for undeclared tasks (C)
[ai_usage.models]            # "Model IA" enum value -> m∈[0,1]. Enum ORDER ≠ capability;
                             # mapped explicitly. Unmapped models warn and default to m=1.0.
"Cap"           = 0.0
"Copilot-Auto"  = 0.70
"Cursor"        = 0.90
"Kimi-2.6"      = 0.85
"DeepSeek-v4"   = 0.85
"Sonnet-4.6"    = 0.90
"Gemini-3.1"    = 1.0
"Opus-4.6-4.7"  = 1.0
"GPT-5.5"       = 1.0
"GPT-5.4"       = 1.0
"GPT-5.3-codex" = 1.0
"GPT-5.2-codex" = 1.0
[ai_usage.levels]            # "Nivell IA" enum value -> l∈[0,1]
A = 0.0
B = 0.25
C = 0.50
D = 0.75
E = 1.0

[penalty]
mode                = "subtractive"
max_penalty_points  = 2.0
student_penalty_cap = 1.0
crit_sa_points = 0.50
crit_cx_points = 0.50
crit_flag_points = 0.75
security_extra = 0.50

[gate]
plagiarism_flag                   = "CROSS_TEAM_SIMILARITY"
ai_detect_risk_level              = "HIGH"
ai_detect_low_levels              = ["A", "B"]
ai_mismatch_auto_apply_worstcase  = false

[normalization]
doc_max = 6.0
mi_floor = 50.0
mi_ceiling = 85.0
cc_penalty = 2.0
test_bonus = 1.0
test_cap = 0.5
surv_floor = 0.50
surv_ceiling = 0.95
k_crit = 2.0
k_warn = 0.5
arch_norm = 4.0

[output]
quantize_final = 0.0         # 0 = continuous; 0.25 snaps to the PR grid via MROUND
decimals = 2
```

## Risks and mitigations

- **The crate and audit do not exist yet.** `crates/grading_xlsx/` and
  `SCHEMA_NOTES.md` are absent; Wave 0 creates the document and Wave 3 creates the
  crate + workspace entry. Do not assume any prior scaffolding.
- **Team-AI cancellation is non-obvious.** `A` cancels for individuals
  (`base_u = Q_pen·eff_u/mean_raw`); only the reported project grade shows team
  AI. *Mitigation:* document it in `Methodology`; the Wave 3 test pins the worked
  example; the `AI_Usage` sheet surfaces raw/effective points + keep.
- **Model→`m` is instance data and enum order ≠ capability.** `Cap` sits
  mid-list; frontier models are scattered, so enum order carries no capability
  signal. *Mitigation:* `[ai_usage.models]` is the sole authority (shipped with
  the 12 current values); an unmapped model `warn`s and falls back to `m = 1.0`.
- **Undeclared incentive.** Keeping 100% on undeclared tasks rewards
  non-disclosure. *Mitigation:* assumed `(m,l)` default frontier×C, the WARNING
  flag, and the detected-AI cross-check.
- **Re-collect required.** `task_ai_usage` is only populated by a fresh
  `collect`. *Mitigation:* Wave 2 ships the collector hook; the grader degrades to
  "all undeclared" (assumed discount + flags) when the table is empty.
- **`N` semantics (enrolled vs active).** Counting non-contributors in `N` lowers
  active members' grades. *Mitigation:* default enrolled with a clamp; switch to
  active-only if over-penalising after a dry run.
- **Double-implementation drift (Rust vs Excel).** *Mitigation:* shared anchor
  constants from `GradingConfig`; the Wave 4 recompute-parity test; `CLAMP` ⇒
  `MEDIAN(0,·,10)` everywhere; `Formula::set_result` caches the Rust-computed
  values into the file.
- **SA `category` not on `RuleFinding`** / **`architecture_violations` rowid
  join** / **missing-vs-zero axis** / **formula layer new to the workspace.**
  *Mitigation:* read `category` directly from `static_analysis_findings`; use
  `load_attributed_findings_for_repo` for the joins; treat a missing axis as
  `present = 0` and renormalize (never a 0 score); implement formulas, defined
  names, validation, and protection fresh on `rust_xlsxwriter 0.94`.
- **Non-determinism leaking into the parity contract.** The grade + AI tables
  stay out of `DERIVED_TABLES`/`COLLECTION_TABLES`; the Wave 1 exclusion test
  enforces it.

## Operator Decisions

- `N` = enrolled team members (default) vs active contributors only.
- `[ai_usage].strength` α (default 1.0) and `floor_keep` (default 0.20); per-model
  `m` and per-level `l` overrides in `config/grading.toml`.
- Undeclared assumed `(m,l)` (default frontier×C); set `undeclared_model_m = 0`
  or `undeclared_level_l = 0` for flag-only (no discount) behaviour.
- `[gate].ai_mismatch_auto_apply_worstcase` (default false) — cross-check flags
  only; `true` substitutes the worst-case assumed discount.
- Penalty caps `max_penalty_points` / `student_penalty_cap` (set 0 to disable a
  scope); `[penalty].mode` subtractive (default) vs multiplicative.
- `[output].quantize_final` (0 = continuous vs 0.25 = snap to the PR grid via
  `MROUND`); `[output].decimals`.
- `--import-weights` reader backend: `calamine` (read-only dependency on
  `grading_xlsx`, parses the edited `Weights` sheet) vs a plain `--import-weights
  weights.toml` round-trip with no new dependency. Default: accept a path and
  read the `Weights` sheet via `calamine`.
- Whether `grading-sheet` may trigger the LLM pass: default **no** — the two
  subcommands stay independent so the grade is deterministic.
- The `course.toml`-side attribute-name override (`CollectOpts.ai_attribute_name`)
  is only needed when a TrackDev instance renames "Ús de IA".

## Phase decomposition

| Phase | Owns (write) | Read-only deps | Mechanical pass/fail |
|---|---|---|---|
| P0 audit | `grading_xlsx/SCHEMA_NOTES.md` | schema, loaders, trackdev export DTOs | doc complete; columns pinned |
| P1 schema | `core/schema.sql`, `core` AI-attr constant, exclusion test in `orchestration` | `db_diff` lists | `apply_schema` fresh+existing; exclusion test green; `diff-db --derived-only` clean |
| P2 collect+flag | `collect/collector.rs`, `collect` `CollectOpts`, `core/db.rs`, `analyze/flags.rs`, `analyze/tests/flag_missing_ai_declaration.rs` | `pm_client`, tasks export, `tasks`/`task_ai_usage` | collector fixture test; flag test |
| P3 grade | `grading_xlsx/{Cargo.toml,src/{lib,config,modulation,aggregate,normalize,penalty,grade,persist}.rs,tests}`, `config/grading.toml`, root `Cargo.toml` | `core::{db,rule_attribution}`, `pr_authors`, schema | components/keep/penalty/gates/finals + idempotent re-persist |
| P4 workbook | `grading_xlsx/src/workbook.rs`, golden fixture | `rust_xlsxwriter 0.94`, `report/xlsx.rs` idioms, P3 structs | golden; defined-names; recompute parity |
| P5 CLI | `cli/src/main.rs`, `cli/Cargo.toml` | `grading_xlsx::run`, `quality_llm::run`, project/sprint resolvers | `--help`; run writes workbook+tables; `diff-db` clean |
| P6 docs | `README.md` | both crates | builds; links resolve |
| PA–PE LLM | `core/config.rs`, `quality_llm/*`, `grading_xlsx/workbook.rs` (LLM_Flags), `config/quality-llm-rubric.md` | `evaluate*`, `llm_quality_flag` | stub-CLI tests; golden incl. LLM_Flags |

**Critical path:** P0 → P1 → P2 → P3 → P4 → P5 → P6; Track B (PA → PE) layers
after, reopening `grading_xlsx/workbook.rs` only for the additive
`LLM_Flags`/`Methodology` edit. **Cross-phase entanglement:** `core` is written
in P1 (schema + constant + db helpers) and PA (config) — each edit must leave the
crate compiling. `cli/src/main.rs` is single-owner in P5. `grading_xlsx/workbook.rs`
is written in P4 and reopened in PE. `schema.sql` is single-owner in P1. These
shared files force a serialized order across the phases that touch them.

## Execution shape

Use option (a): one phased plan executed via `--prompt-file`, not N separate
invocations. `core` (P1 schema + db helpers, PA config), `cli/src/main.rs` (P5),
and `grading_xlsx/workbook.rs` (P4 + PE) are each shared across phases and would
race if dispatched as independent invocations. A single phased run serializes the
shared-file edits, keeps per-wave commits for bisectability, and runs the
four-command gate after every wave. Track A (P0–P6) is independently shippable;
Track B (PA–PE) layers on top behind the same gate. The tradeoff is throughput:
a phased plan is sequential and slower than parallel invocations, but the shared
ownership of `core`, `cli/src/main.rs`, and `grading_xlsx/workbook.rs` makes
parallelism unsafe here, so sequencing is the correct choice.
