# SCHEMA_NOTES — column bindings for `grading-sheet` / `grading_xlsx`

Wave 0 audit (blocking). Every column this feature reads, pinned to its
table and **grain**, verified against `crates/core/src/schema.sql` and the
Rust loaders at the commit this file lands on. The grade is derived **only**
from columns listed here; nothing is read that is not pinned below.

## Grain legend

| Grain | Key | Examples |
|---|---|---|
| `artifact` | `repo_full_name` (no sprint, no project) | `architecture_violations`, `static_analysis_findings` |
| `artifact+project` | `repo_full_name` + `project_id` | `method_complexity_findings` |
| `student-sprint` | `(student_id, sprint_id)` | `student_sprint_*` |
| `pr-sprint` | `(pr_id, sprint_id)` | `pr_doc_evaluation` |
| `task-sprint` | `(task_id, sprint_id)` | `task_description_evaluation` |
| `sprint-flag` | `flags(student_id, sprint_id)` | `flags` |
| `project-flag` | `student_artifact_flags(student_id, project_id)` | `student_artifact_flags` |
| `student-sprint+project` | `(student_id, sprint_id)` carrying `project_id` | `student_sprint_ai_usage` |

`student_id` is `students.id` (**TEXT**) everywhere. `project_id` is
`projects.id` (**INTEGER**) and equals `students.team_project_id`.

## Identity / mapping columns

| Need | Column(s) | Grain | Notes |
|---|---|---|---|
| Student → project | `students.team_project_id` | — | INTEGER; NULL for unassigned students (skip them) |
| Student display name | `students.full_name` | — | for `StudentGradeRow.full_name` |
| Project display name | `projects.name`, `projects.slug` | — | `name` for rows; `slug` matches `--projects` filter |
| PR → student (TrackDev-scoped) | `pr_authors(pr_id, student_id, author_points, author_task_count)` | view | VIEW over `task_pull_requests → tasks.assignee_id`; **excludes `USER_STORY`**, excludes NULL assignees |
| Cumulative sprint set | `Database::sprint_ids_up_to_current(project_id: i64, today: &str) -> Result<Vec<i64>>` | — | `start_date <= today`, ASC; method on `Database`, not free `Connection` — see note ¹ |

¹ The Wave 3 dispatch passes `&db.conn` (a raw `Connection`). `sprint_ids_up_to_current`
is a method on `core::db::Database`. Wave 3 either threads a `&Database` or re-issues the
identical one-line `SELECT id FROM sprints WHERE project_id=? AND start_date<=? ORDER BY
start_date ASC` (the function body, db.rs:551). Prefer threading `&Database`.

### Repo → project mapping (robust to NULL `pull_requests.author_id`)

`architecture_violations` / `static_analysis_findings` are `artifact`-grain (keyed by
`repo_full_name` only). To attribute a repo's findings to a project, map through the PR
graph, **never** `pull_requests.author_id` (NULL for ~25% of PRs):

```sql
SELECT DISTINCT s.team_project_id
FROM pull_requests pr
JOIN pr_authors pa ON pa.pr_id = pr.id
JOIN students s ON s.id = pa.student_id
WHERE pr.repo_full_name = ? AND s.team_project_id IS NOT NULL;
```

Inverse (project → its repos) is the same join grouped the other way; a project typically
has two repos (`android-*`, `spring-*`).

## Positive axis source columns

The four quality axes are graded **once at team grain** for the project grade `Q`
(plan-v2 §"The grade model" + `normalize.rs`). Each row below names the **single
authoritative raw input** Wave 3 reads, with the `[normalization]` anchor it maps through.
Columns demoted to *diagnostic-only* feedback are listed under the table — they populate
`student_component_score`, never a grade input.

| Axis | Graded raw input | Table | Source grain | `[normalization]` anchor |
|---|---|---|---|---|
| Documentation | `total_doc_score` (team mean) | `pr_doc_evaluation` | pr-sprint → team | `doc_max` |
| Code quality | `avg_maintainability` (team mean) | `student_sprint_quality` | student-sprint → team | `mi_floor` / `mi_ceiling` |
| Code quality — cc penalty | `pct_methods_cc_over_10` | `student_sprint_quality` | student-sprint → team | `cc_penalty` |
| Code quality — test bonus | `mutation_score` | `pr_mutation` | pr-sprint → team | `test_bonus` / `test_cap` |
| Survival | `survival_rate_normalized` (team mean) | `student_sprint_survival` | student-sprint → team | `surv_floor` / `surv_ceiling` |
| Survival — points weight | `estimation_points_total` | `student_sprint_survival` | student-sprint | weight for the mean |
| Architecture | CRITICAL/WARNING density via loader (below) | `architecture_violations` | artifact → team | `k_crit` / `k_warn` / `arch_norm` |

**Diagnostic-only columns (feedback in `student_component_score`, never graded):**
`student_sprint_metrics.avg_doc_score` (student-grain documentation — derived from the *same*
PR evaluations as `total_doc_score`, so grading it would double-aggregate PR→student→team);
`task_description_evaluation.quality_score` (0–1, task-grain); `student_sprint_quality.{test_to_code_ratio,
satd_count}` (test *quantity* + SATD — superseded by `mutation_score` for the test signal);
`student_sprint_contribution.composite_score` (within-team differentiation now comes from the
AI-discounted effective-points ratio, not this axis).

**`pr_mutation` is gated and may be empty.** Mutation testing runs only when `[mutation] enabled`
AND a per-profile `mutation_command` is set; otherwise `pr_mutation` has no rows. The test term
is a **capped bonus** (`test_cap`), not a floor, so a missing `mutation_score` yields
`present = false` → **no bonus** (the axis renormalizes; never a 0 score). Do **not** silently
fall back to `test_to_code_ratio` — that is an operator opt-in, not a default.

**Scale caveat (verify in Wave 3, do not block Wave 0):** the `doc_max = 6` anchor assumes
`total_doc_score` tops out near 6 (`title_score` + `description_score`). Confirm against
`config/rubric.md` before trusting the documentation axis absolutely.

**Aggregation rule (Wave 3).** All four axes resolve at **team grain** over
`sprint_ids_up_to_current(project_id, today)`:

- *Student-sprint axes* (code quality, survival): fold each member's per-sprint rows —
  **sum** counts, **points-weighted mean** of rates (`estimation_points_total` as weight where
  present, else equal weight) — then mean across members.
- *PR-sprint axes* (documentation, mutation test-bonus): **team mean** directly over the team's
  PRs in the window (no intermediate per-student fold).

Every axis yields `(raw_value: Option<f64>, present: bool)`; a missing input is `present = false`
and renormalizes **out** of the weighted base — it is never a 0 score.

## Blame-weighted findings — single entry point

`core::rule_attribution::load_attributed_findings_for_repo(conn, repo_full_name, kind)
-> rusqlite::Result<Vec<AttributedFinding>>` is the **only** sanctioned reader for
attributed findings. It owns both join shapes so callers never hand-roll the `rowid` join:

| `RuleKind` | Parent table | Attribution table | Join key |
|---|---|---|---|
| `Architecture` | `architecture_violations` (**no surrogate `id`**) | `architecture_violation_attribution` | implicit `rowid` ↔ `violation_rowid` |
| `Complexity` | `method_complexity_findings` (`id` PK) | `method_complexity_attribution` | `id` ↔ `finding_id` |
| `StaticAnalysis` | `static_analysis_findings` (`id` PK) | `static_analysis_finding_attribution` | `id` ↔ `finding_id` |

`AttributedFinding { finding: RuleFinding, attributions: Vec<AuthorAttribution> }`.
`RuleFinding { rule_id, kind, severity: Severity{Critical|Warning|Info}, repo_full_name,
file_repo_relative, span, evidence, extra: Option<String> }`. `AuthorAttribution {
student_id, blame_share: f64 }` (the `weight` column, in `[0,1]`). Findings with no blame row
surface with an **empty** `attributions` vec (generated/copy-pasted files) — the renderer
skips them; the grader treats them as un-attributable, not as zero blame.

### `category` is NOT reachable from `AttributedFinding`  ← Wave 0 confirm

`RuleFinding` has **no `category` field**, and the SA loader sets `extra: None`
(`rule_attribution.rs:282`). The architecture loader puts `offending_import` in `extra`; the
complexity loader puts `"{measured} > {threshold}"`. **None expose
`static_analysis_findings.category`.** The `+security_extra` bump (CRITICAL SA finding with
`category == 'security'`) therefore requires a **targeted direct read** of
`static_analysis_findings`:

```sql
SELECT id, category, severity FROM static_analysis_findings
WHERE repo_full_name = ? AND severity = 'CRITICAL';
```

Recommended Wave 3 shape: for the SA penalty specifically, do **one direct join** of
`static_analysis_findings` (gives `id, category, severity`) with
`static_analysis_finding_attribution` (gives `student_id, weight`) so category and blame_share
arrive together in one query — avoids re-keying loader output back to category rows by the
fragile `(repo, file, start_line, rule_id)` tuple (`rule_id` is stored bare in the table but
the loader rewrites it to `"{analyzer}:{rule_id}"`). Architecture and complexity penalties
still flow through the loader. This is recorded as a Wave 3 design note + Risk; it does not
change Wave 0.

## Penalty source tables (CRITICAL only deducts)

| Source | Table | Grain | Severity filter |
|---|---|---|---|
| Static-analysis | `static_analysis_findings` ⋈ `static_analysis_finding_attribution` | artifact | `severity = 'CRITICAL'`; `+security_extra` when `category = 'security'` |
| Complexity | `method_complexity_findings` ⋈ `method_complexity_attribution` | artifact+project | `severity = 'CRITICAL'` |
| Behavioural (sprint) | `flags` | sprint-flag | `severity = 'CRITICAL'`, summed across the project's sprints, weight 1 |
| Behavioural (artifact) | `student_artifact_flags` | project-flag | `severity = 'CRITICAL'`, weight 1 |

Penalty is **capped and subtractive**: `MIN(max_penalty_points, Σ)`, default cap 2.0. Never a
linear grade term. WARNING/INFO are feedback-only.

**Do NOT** read `STATIC_ANALYSIS_HOTSPOT` / `COMPLEXITY_HOTSPOT` flag rows for the penalty:
the pipeline wires only `ARCHITECTURE_HOTSPOT` into the flag tables, so the other two are
frequently **unpopulated**. Read the base finding tables through the loader; a missing input is
`present = 0`, never a zero score.

**Architecture is a positive axis only** — its violations feed the architecture sub-score and
are **excluded** from the penalty (no double-count). Complexity intentionally appears twice
(code-quality axis via `avg_maintainability`; penalty via CRITICAL findings) — disable the
penalty side with `crit_cx_points = 0` if undesired.

## Gate flag storage  ← Wave 0 confirm (corrects the plan)

Gates are **review routes written to `review_gate`**, not silent zeros.

| Gate | Flag / signal | Where it actually lives | Grain | Scope of `student_id` |
|---|---|---|---|---|
| `NO_DELIVERY` | `ZERO_TASKS` | `flags` | sprint-flag | **real** `students.id`; **CRITICAL**; fires **per sprint** when the student has 0 DONE non-`USER_STORY` tasks that sprint (flags.rs:168) |
| `PLAGIARISM` | `CROSS_TEAM_SIMILARITY` | `flags` | sprint-flag | **synthetic** `student_id = 'PROJECT_<project_id>'`; **CRITICAL**; team-scoped, **not** a real student (flags.rs:1116) |
| `AI` | `risk_level = 'HIGH'` | `student_sprint_ai_usage` | student-sprint+project | real student; carries `project_id`; per sprint |

Routing consequences:

- `CROSS_TEAM_SIMILARITY` is recovered by `SELECT … FROM flags WHERE flag_type =
  'CROSS_TEAM_SIMILARITY' AND student_id = 'PROJECT_' || ?` (the synthetic id encodes the
  project id directly), or equivalently by `JOIN sprints ON sprints.id = flags.sprint_id WHERE
  sprints.project_id = ?`. It routes the **whole project** (and every member) to
  `review_gate = 'PLAGIARISM'`; it does **not** auto-zero.
- The CRITICAL-`flags` penalty sum (above) must **exclude synthetic `PROJECT_*` rows** —
  they are not real students and would otherwise be charged to no one (and
  `CROSS_TEAM_SIMILARITY` is a gate, not a penalty). Filter `student_id NOT LIKE 'PROJECT\_%'`
  or restrict to the project's real member ids.
- `ZERO_TASKS` is per-sprint; the cumulative-grade interpretation is an **OPEN DECISION**
  (see below) — a single empty sprint must not necessarily zero a term-long grade.

## Parity-contract exclusion (Wave 1 test)

`crates/orchestration/src/db_diff.rs` defines `DERIVED_TABLES` (40 tables) and
`COLLECTION_TABLES` (9 tables). Confirmed **already outside both**:
`architecture_violations`, `architecture_violation_attribution`, `static_analysis_findings`,
`static_analysis_finding_attribution`, `student_artifact_flags`. The five new tables
(`project_final_grade`, `student_final_grade`, `student_component_score`,
`project_component_score`, `llm_quality_flag`) must **stay out** of both lists so
`diff-db --derived-only` is unaffected. Wave 1 adds a unit test asserting their absence.

(Note: `flags`, `student_sprint_ai_usage`, `method_complexity_findings`,
`method_complexity_attribution`, `student_sprint_contribution` ARE in `DERIVED_TABLES` — we
only *read* them, never write, so parity is untouched.)

## Decisions resolved by the operator

1. **`ZERO_TASKS` cumulative-gate semantics → DECIDED: whole-window no-delivery.** The flag is
   per-sprint, but the `NO_DELIVERY` gate fires only when the student delivered nothing across
   **all** in-scope sprints. Wave 3 computes this from cumulative `author_task_count == 0` over
   `sprint_ids_up_to_current(project_id, today)` (robust, flag-timing-independent), **not** by
   OR-ing the per-sprint `ZERO_TASKS` flag. A single empty sprint flows through the components
   (survival / contribution / …) and lowers the grade without zeroing it.

## Open decisions surfaced by this audit (need confirmation before Wave 3)

2. **AI gate scope.** `risk_level` is per student-sprint. Cap the **student** when *that*
   student has HIGH in any sprint (clear). For the **project** grade, "any HIGH in the project"
   would cap the whole team off one member. *Recommendation:* student-level cap on the
   student's own HIGH; leave the project grade uncapped by AI unless the team is pervasively
   HIGH (defer a project-level AI rule until the first dry-run).
3. **`--import-weights` reader** (Wave 5): add read-only `calamine` to parse the `Weights`
   sheet, or accept a `weights.toml`/`weights.csv` export. *Recommendation:* `calamine`
   scoped to the one sheet — keeps the operator's edit loop entirely in Excel.

---

# v3 grade-model update — declared-AI usage + project×contribution

The grade model changed after this audit was first written. **The student grade is no
longer a weighted sum of that student's own quality axes.** Quality is graded **once at the
project (team) level**; each student's grade is the project grade redistributed by their
share of AI-discounted effective points. The column bindings above are unchanged — what
changed is *how they combine* and *at what grain*. See `plans/total_grading/claude-refine-plan-v2.md`
for the full model. Net effect on this audit:

- The four quality axes (documentation, code_quality, survival, architecture) are now
  aggregated at **team grain** for the project grade `Q`. `student_component_score` becomes
  **diagnostic/feedback only** (per-student axes shown, not graded).
- `student_sprint_contribution.composite_score` is **no longer** a grade axis. Within-team
  differentiation comes from the AI-discounted effective-points ratio instead.
- Penalty has **two scopes**: artifact CRITICAL static-analysis + complexity → subtract from
  project `Q` (cap `max_penalty_points`); per-student CRITICAL behavioural `flags` /
  `student_artifact_flags` → subtract from that student's final grade (cap
  `student_penalty_cap`). Architecture stays positive-axis-only.
- The **AI gate is a cross-check, not a cap** (see below). No-delivery is automatic (zero
  effective points → grade 0).

## Declared-AI usage bindings (TrackDev "Ús de IA")

Source of truth verified in the **`trackdev-spring`** backend (sibling checkout):

| Need | Binding | Grain |
|---|---|---|
| Declared (model, level) per task | `/export/tasks` → each entry's **sibling** `attributeValues[]` (not nested in `task`); element with `attributeName == "Ús de IA"`, `attributeType == "ENUM_PAIR"` | task |
| Model (slot 1) | `attributeValues[i].value` | task |
| Level (slot 2, A–E) | `attributeValues[i].valueB` | task |
| Model domain | `attributeValues[i].enumValues[] = {value, description}` (inline) | profile enum |
| Level domain | `attributeValues[i].enumValues2[]` (inline) | profile enum |
| Applied by | `attributeAppliedBy == "STUDENT"` (students self-declare) | — |

Backend confirmations: `AttributeType.ENUM_PAIR` (entity + migration
`V20__enum_pair_attribute_type.sql`); `TaskAttributeValue` stores slot 1 in `value`, slot 2
in `valueB`; `EnumValueEntryDTO = {value, description}`; `ProjectExportMapper:54` populates
`attributeValues` per task. The collector (`crates/collect/src/collector.rs:327`) currently
reads only `entry["task"]` and **discards** `attributeValues` — Wave 2 adds the hook.

New collected tables (Wave 1 DDL): `task_ai_usage(task_id PK, model_value, level_value,
declared, captured_at)` and `ai_usage_enum_domain(slot, value, description, ord)`.

**Live enum inventory (operator-supplied, 2026-06-07).** The ENUM_PAIR references two profile
enums:

- `"Model IA"` (slot 1, 12 values): `Copilot-Auto, Opus-4.6-4.7, GPT-5.3-codex, GPT-5.4,
  GPT-5.5, Cursor, Gemini-3.1, Kimi-2.6, DeepSeek-v4, Cap, Sonnet-4.6, GPT-5.2-codex`
  (confirm the list isn't truncated below the fold). **Enum order is NOT capability order**
  (`Cap` is the 10th entry) — so model→`m` **must** be an explicit `[ai_usage.models]` map,
  never an `ai_usage_enum_domain.ord` default; an unmapped model warns and falls back to
  `m = 1.0` (conservative).
- `"Nivell IA"` (slot 2, 5 values A–E, increasing AI): `A` Pràcticament no s'ha fet amb IA →
  `B` S'ha fet servir una mica → `C` Aproximadament la meitat → `D` Moltes coses fetes amb IA
  → `E` Gairebé tot s'ha fet amb IA. Maps to `l = {A:0, B:.25, C:.5, D:.75, E:1}`.

Shipped default `m`: `Cap`=0; `Copilot-Auto`=.70; `Cursor`/`Sonnet-4.6`=.90; `Kimi-2.6`/
`DeepSeek-v4`=.85; all `GPT-5.*`/`Opus-4.6-4.7`/`Gemini-3.1`=1.0. The middle tiers are
pedagogical judgment calls to tune.

Both AI tables are **collected inputs**; like the grade tables they stay **out of**
`DERIVED_TABLES` and `COLLECTION_TABLES` (deliberately outside the dual-run parity contract).

## Resolved AI / grade-model decisions (operator-confirmed)

1. **Modulation grid** — `keep = 1 − (1−floor_keep)·α·m·l` (default `1 − 0.8·α·m·l`);
   `l` = {A:0, B:.25, C:.5, D:.75, E:1}; `m` = Cap 0 … frontier 1; global strength `α`
   (`[ai_usage].strength`, default 1.0, [0,1]). Config-overridable grid.
2. **Undeclared task** — flag `MISSING_AI_DECLARATION` (**WARNING**, per assignee+sprint,
   lists undeclared task keys) **and** apply a configurable assumed `(m,l)` (default
   frontier×C), scaled by `α`. Not CRITICAL (no double-count with the discount).
3. **Integration / formula** — project quality `Q` × team AI factor `A = Σeff/Σraw`;
   student final `= CLAMP(Q_pen · eff_u/mean_raw − student_penalty_u, 0, 10)`. The team AI
   factor **cancels** for individuals (each student reflects their own AI, teammates
   shielded); reported project grade `= Q_pen·A =` team average. `N` = enrolled team size
   (default; clamp handles carry-inflation).
4. **Gates** — no-delivery automatic (zero eff → 0, label `NO_DELIVERY`); plagiarism
   (`CROSS_TEAM_SIMILARITY` synthetic `PROJECT_<id>`) → project+members `PLAGIARISM` review,
   no auto-zero; **detected-AI cross-check**: `student_sprint_ai_usage.risk_level='HIGH'` +
   low/absent declaration → `AI_DECLARATION_MISMATCH` + `AI_REVIEW`, no auto grade change
   (config `ai_mismatch_auto_apply_worstcase`).
