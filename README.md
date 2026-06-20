# trackdev-grader

`trackdev-grader` is a Rust workspace that produces the `sprint-grader` CLI: an
automated grading pipeline for student SCRUM projects. It pulls sprint
deliverables from [TrackDev](https://trackdev.org) (the project-management
system) and GitHub, runs them through a multi-stage analysis pipeline, and
emits per-team Markdown and Excel reports plus a fully-populated SQLite
database of derived metrics.

It is the engine behind the grading workflow for an undergraduate Software
Engineering course where teams of 5–6 students build an Android client +
Spring Boot backend across four sprints.

## Philosophy

The tool is built on three convictions:

**1. Grade the process, not just the artefact.**
Final code can hide who actually wrote it, when it was written, and whether
the team functioned. The pipeline reconstructs *how* a sprint happened by
combining git blame, PR metadata, sprint timing, code-quality deltas, and
team-collaboration graphs — so a rushed merge of someone else's work looks
different from sustained, distributed effort even when both produce identical
final code.

**2. Detect anomalies; let the instructor decide.**
Every signal the pipeline computes — line-survival deviation, contribution
imbalance, AI-usage probability, cosmetic rewrites of teammates' code — is
surfaced as a *flag* with its underlying numbers, not a verdict. Reports are
designed for an instructor who will read them and make the call. Thresholds
are configurable in [`config/course.toml`](config/course.toml).

**3. Reproducible, deterministic, idempotent.**
All intermediate state lives in a single SQLite file
(`data/grading.db`). Every stage is a pure function of the DB plus
the cloned repos: re-running it produces the same rows. Stages cache, and
`purge-cache` lets you selectively invalidate. A built-in `diff-db` command
checksums tables across two runs to verify changes don't drift.

## Architecture

The workspace is split into focused crates that map onto pipeline stages.
Crates communicate exclusively through the SQLite database — no in-memory
hand-offs across stages, which is what makes each stage independently
runnable and testable.

```
                  ┌────────────────────────────────────────────────┐
                  │  TrackDev API   GitHub API   Anthropic API     │
                  └──────┬─────────────┬──────────────┬────────────┘
                         │             │              │
   collect ──────────────┴─────────────┘              │
   compile_stage  (build each PR in a worktree)       │
   survival       (fingerprint + blame statements)    │
   quality        (complexity / Halstead / SATD)      │
   process_stage  (planning / regularity / temporal)  │
   analyze        (per-student metrics + flags)       │
   evaluate       (LLM PR-doc rubric scoring) ────────┘
   repo_analysis  (task-similarity clusters, timing tiers)
   ai_detect      (behavioural + stylometric + curriculum + fusion)
   curriculum     (slide-derived concept allow-list)
   report         (xlsx workbooks + markdown REPORT.md)
   orchestration  (pipeline glue: run-all / go / go-quick)
                                │
                                ▼
                       data/grading.db
                       data/entregues/<project>/REPORT.md
                       data/entregues/sprint_K/<team>.xlsx
```

### Crate guide

| Crate | Stage | What it produces |
|---|---|---|
| [`core`](crates/core) | foundation | Loads `course.toml`, owns the SQLite schema (~41 tables), shared error / time / formatting helpers. |
| [`collect`](crates/collect) | 1 | Pulls projects, sprints, students, tasks from TrackDev; PRs / commits / reviews from GitHub; clones every team repo into `data/entregues/<project>/`. |
| [`compile_stage`](crates/compile_stage) | 1.5 | Checks each merged PR out into a temp worktree and runs the matching `[[build.profiles]]` command (e.g. `./gradlew assembleDebug`) under a hard timeout. Records pass/fail + truncated stderr. |
| [`survival`](crates/survival) | 2 | Parses Java/XML, fingerprints statements via AST-normalised hashes, runs `git blame` for each, and computes how much code authored in sprint *N* survives into sprint *N+1*. Also detects cross-team copies and cosmetic rewrites of other students' work. |
| [`analyze`](crates/analyze) | 3 | Per-student sprint metrics (points, weighted lines, commits, reviews, doc score), team-level inequality (Gini, Hoover, CV), composite contribution scores, and the **flag detector** (cramming, empty PRs, solo reviewer, low survival, …). |
| [`quality`](crates/quality) | 5a | Cyclomatic + cognitive complexity, Halstead volume / difficulty / effort, SATD (self-admitting technical debt) scanning, sprint-over-sprint deltas. |
| [`process_stage`](crates/process_stage) | 5b | Sprint planning quality (velocity, commitment accuracy), PR regularity scoring (sigmoid against deadline), temporal patterns (commit entropy, weekend / night work), team collaboration network (review reciprocity, density). |
| [`evaluate`](crates/evaluate) | 4 | Heuristic flags for empty / generic PR descriptions; optional Claude API call to rate title (0–2) and description (0–4) per [`config/rubric.md`](config/rubric.md). Falls back cleanly when no API key is configured. |
| [`evaluate_local`](crates/evaluate_local) | 4 (alt.) | `judge = "local-hybrid"` backend: BGE-M3 embedding + per-axis ridge regression + Salamandra-2B chat fallback for borderline PRs, all routed through a local ollama daemon over HTTP. Drops Claude Max quota consumption to ~0% in steady state. See [Local-hybrid PR doc evaluator](#local-hybrid-pr-doc-evaluator) below. |
| [`curriculum`](crates/curriculum) | knowledge base | Parses LaTeX slide decks to extract the set of concepts / imports taught in each sprint. Used downstream by `ai_detect` to flag code that uses material the team hasn't been taught yet. |
| [`repo_analysis`](crates/repo_analysis) | 6 | Clusters tasks by `(stack, layer, action)` with MAD-based outlier detection; classifies merged PRs into submission timing tiers (early / on-time / late / cramming). |
| [`ai_detect`](crates/ai_detect) | 7 | Behavioural signals (single-commit dumps, fix-up patterns, line-per-minute productivity), per-student stylometric baseline + deviation, curriculum violations, text-consistency score, and Bayesian fusion into per-PR / per-file / per-student AI-usage probabilities. |
| [`static_analysis`](crates/static_analysis) | 5c | Java static-analysis stage. Shells PMD / Checkstyle (T6 adds SpotBugs + FindSecBugs), parses SARIF 2.1.0, normalises severity per analyzer, and writes `static_analysis_findings` + `static_analysis_finding_attribution` (per-student blame weights). Gated on `config/static_analysis.toml`; absent file → silent skip. |
| [`report`](crates/report) | 8 | Per-sprint Excel workbooks (one per team + cross-team summary) and a multi-sprint Markdown `REPORT.md` committed back into each team's Android repo with inline SVG sparklines. |
| [`grading_xlsx`](crates/grading_xlsx) | sheet | Read-mostly `grading-sheet` command: computes 0–10 project + student grades from evidence already in `grading.db`, persists `project_final_grade` / `student_final_grade`, and writes a self-recalculating `grading_sheet.xlsx`. |
| [`grading_html`](crates/grading_html) | sheet | `grading-html` command: emits a single, offline, SQL-queryable `grading.html` over the same computed grades, with live knob tuning and an in-browser JS/Rust parity self-test. Presents only — shares `grading_xlsx`'s grade+persist path. |
| [`quality_llm`](crates/quality_llm) | sheet (Track B) | `quality-flags` command: file-tier + holistic LLM advisory flags into `llm_quality_flag`; never a grade input. |
| [`orchestration`](crates/orchestration) | glue | The three full-pipeline variants (`run-all`, `go`, `go-quick`), parallel sprint execution via `rayon`, cache purge, the `diff-db` table-by-table dual-run checker, and the `sync-reports` publisher. |
| [`cli`](crates/cli) | binary | The `sprint-grader` clap CLI exposing every stage as its own subcommand plus the full-pipeline aggregates. |

### Pipeline variants

The orchestration crate exposes four top-level pipelines:

- **`run-all`** — additive cumulative run. Incremental collection (per-PR
  watermark + GitHub ETag); per project, skips survival/compile/architecture
  when no new PRs/tasks were collected. No AI detection. Survival failure is
  fatal. *Does not* purge.
- **`iterate`** — same as `run-all`, plus a historical `--skip-arch-llm`
  flag from before the AST migration. Since Wave 4 the per-file LLM
  architecture rubric is off by default in `course.toml`, so this flag is
  a no-op for any course that hasn't opted back in via
  `[architecture] llm_review = true`.
- **`go-quick`** — *always* purges before collecting, then re-collects from
  scratch. PR doc eval forced to heuristic (no Claude calls); static analysis
  skipped by default. AI detection on. Tolerates survival errors. Designed
  for mid-sprint iteration.
- **`go`** — end-of-sprint full run. *Always* purges before collecting; LLM
  PR doc eval (when `ANTHROPIC_API_KEY` is set) and AI detection.
  Tolerates survival errors. Architecture conformance always runs but uses
  the AST rules in `config/architecture.toml`; the deprecated per-file LLM
  judge engages only when a course sets `llm_review = true`.

`--projects <slug,…>` is a **scope reducer** on every command: it never
changes what the command does, only how much of the DB it touches. For
`go`/`go-quick`, the purge always runs — with `--projects` it's scoped to
the listed projects; without it, every project in the DB is wiped. The
cascade clears `pr_github_etags` and the per-PR watermark, which is why
re-collection re-fetches every PR.

PR documentation evaluation by variant:

| Pipeline    | Heuristic doc eval | LLM doc eval                       |
|-------------|--------------------|------------------------------------|
| `run-all`   | ✓                  | ✓ if `ANTHROPIC_API_KEY` is set    |
| `iterate`   | ✓                  | ✓ if `ANTHROPIC_API_KEY` is set    |
| `go`        | ✓                  | ✓ if `ANTHROPIC_API_KEY` is set    |
| `go-quick`  | ✓                  | ✗ — heuristic only, even with key  |

`go-quick` previously skipped PR doc eval entirely; as of T-P0.2 it now
populates `student_sprint_metrics.avg_doc_score` from the heuristic scorer.

### Local-hybrid PR doc evaluator

For courses running on a Claude Max subscription, the per-PR `claude --print`
calls in the `claude-cli` backend consume the 5-hour session budget linearly.
`judge = "local-hybrid"` in `[evaluate]` routes scoring through a local
pipeline backed by an [ollama](https://ollama.com) daemon instead:

```
short-circuit detectors → BGE-M3 embedding (ollama HTTP)
                       → ridge regression (Rust dot product, JSON weights)
                       → triage: Snap | NeedsLlm | ShortCircuit
                       → (NeedsLlm) Salamandra-2B-Instruct (ollama chat)
                       → persist row + update avg_doc_score
```

Operator setup:

```bash
# 1. Pull the embedding + chat models into ollama.
ollama pull bge-m3
ollama pull hf.co/BSC-LT/salamandra-2b-instruct-GGUF:Q5_K_M

# 2. Cold start: produce ~150 labelled rows with the cloud judge once.
#    Set `[evaluate] judge = "claude-cli"` in course.toml.
sprint-grader run-all --today <YYYY-MM-DD>

# 3. Train the regressor on those labels (writes data/regressor/pr_*.json).
python tools/train_regressor/train.py \
    --db data/grading.db \
    --ollama http://127.0.0.1:11434 \
    --embed-model bge-m3 \
    --out data/regressor

# 4. Flip the judge and run normally — no Claude calls in steady state.
#    Set `[evaluate] judge = "local-hybrid"`.
sprint-grader run-all --today <YYYY-MM-DD>
```

After rubric changes (or new sprint labels), retrain the regressor and run
`sprint-grader reset-local-scores [--projects …]` to invalidate previously
persisted local rows; pre-existing Haiku-judged rows are untouched. The
invalidation discriminator is the `"local:"` prefix on
`pr_doc_evaluation.justification` — never edit a justification by hand to
drop that prefix.

See [`tools/train_regressor/README.md`](tools/train_regressor/README.md) for
the full retraining workflow + calibration thresholds. The Spring/Anthropic
fallback path is unchanged — `judge = "claude-cli"` / `"cursor-cli"` /
`"anthropic-api"` /
`"deepseek-api"` all still work and ignore the local pipeline entirely.

All variants use a `rayon` thread pool to fan sprints out across worker
connections (SQLite WAL mode allows concurrent readers + serialised writers).
Stages 5–7 (quality / process / repo_analysis / ai_detect) are each pure
functions of the DB, so re-running them is cheap.

`go` and `go-quick` accept `--dry-run` (preview the cascade purge step and
exit before the pipeline runs) and `--require-clean-tree` (refuse to start
if `git status --porcelain` is non-empty).

## Quick start

### Prerequisites

- Rust stable (`rust-toolchain.toml` pins `stable` with `rustfmt` + `clippy`).
- A TrackDev API token and a GitHub PAT with read access to the course org.
- Optional: an Anthropic API key for LLM-graded PR documentation and for the
  AI-detection LLM-judge signal.
- A local checkout of the course slides (LaTeX) if you want to (re)build the
  curriculum knowledge base.

### Build

```bash
cargo build --release
```

The binary lands at `target/release/sprint-grader`.

### Configure

Create a `.env` file in the project root:

```dotenv
TRACKDEV_TOKEN=...
GITHUB_TOKEN=ghp_...
ANTHROPIC_API_KEY=sk-ant-...               # optional
ANTHROPIC_MODEL=claude-haiku-4-5-20251001  # optional; overrides [evaluate].model_id
```

Edit [`config/course.toml`](config/course.toml) — the most important keys:

| Section | Key | Purpose |
|---|---|---|
| `[course]` | `name`, `num_sprints`, `pm_base_url`, `github_org`, `course_id` | Identifies which TrackDev course + GitHub org to pull from. |
| `[course]` | `claude_scripts_path` | Path to the Claude session library used by LLM evaluation. |
| `[thresholds]` | `cramming_hours`, `micro_pr_max_lines`, `low_doc_score`, `contribution_imbalance_stddev`, `low_survival_rate_stddev`, `low_survival_absolute_floor`, `raw_normalized_divergence_threshold`, … | Tunables for the flag detector. `low_survival_absolute_floor` (default `0.85`) is the absolute LS rate below which `LOW_SURVIVAL_RATE` may fire even when the relative-stddev guard would otherwise suppress it. |
| `[detector_thresholds]` | `gini_warn`, `gini_crit`, `composite_warn`, `composite_crit`, `late_regularity`, `team_inequality_outlier_deviation`, `trajectory_cv_low`, `trajectory_cv_high`, `trajectory_slope_p_value`, `regularity_declining_delta`, `cosmetic_rewrite_pct_of_lat`, `bulk_rename_adds_dels_ratio`, `bulk_rename_line_floor` | Detector-level knobs migrated out of Rust source (T-P1.3). All keys are optional — omit any and the binary falls back to the canonical default in `DetectorThresholdsConfig::default()` (defaults: `0.35 / 0.50 / 0.20 / 0.10 / 0.20 / 0.35 / 0.20 / 0.40 / 0.15 / -0.30 / 0.05 / 0.8 / 50`). |
| `[[build.profiles]]` | `repo_pattern`, `command`, `timeout_seconds` | Per-repo-type build command run by `compile`. The pattern is a regex against the repo directory name. |
| `[build]` | `max_parallel_builds`, `stderr_max_chars`, `skip_already_tested` | Compile-stage concurrency + caching behaviour. |
| `[curriculum]` | `slides_dir`, `extra_allowed_imports` | Where to find the LaTeX slides; imports always considered "taught". |
| `[regularity]` | `midpoint_hours`, `steepness`, band thresholds | Shape of the sigmoid that scores how early before the deadline a PR landed. |
| `[repo_analysis]` | `group_min_size`, `mad_k_threshold`, `temporal_*_hours` | MAD-based outlier detection + temporal-tier cutoffs. |
| `[curriculum]` | `freeze_after_sprint_end` (bool, default `false`) | When true, the orchestrator snapshots the curriculum-as-taught for any sprint whose `end_date` has passed (T-P2.5). Past sprints are then graded against the frozen `curriculum_concepts_snapshot`; the active sprint reads the live `curriculum_concepts` until you freeze. The CLI `sprint-grader freeze-curriculum --sprint <N>` is the explicit alternative. |
| `[grading]` in **`course.toml`** | `hidden_thresholds` (bool, default `false`), `jitter_pct` (float, default `0.0`) | **Not** the sprint grade model. Anti-gaming for flag detectors: when `hidden_thresholds = true`, every fractional detector knob is uniformly jittered by `± jitter_pct` once per pipeline run, seeded by `(today, course_id)`. Realised values land in `pipeline_run`. For 0–10 project/student grades see [`config/grading.toml`](#grading-sheet-project--student-grades) and `sprint-grader grading-sheet`. |

### Run

```bash
# fetch everything for sprints up to today
sprint-grader collect

# build every merged PR (fills pr_compilation)
sprint-grader compile

# run the full per-sprint analysis stack
sprint-grader survive
sprint-grader analyze
sprint-grader quality
sprint-grader process
sprint-grader inequality
sprint-grader evaluate

# AI usage detection
sprint-grader ai-detect

# write reports
sprint-grader report

# end-of-term grades (read-mostly; needs a fresh collect for declared AI)
sprint-grader grading-sheet
```

Or, equivalently, the orchestrated forms:

```bash
sprint-grader run-all              # additive full run, no AI detection
sprint-grader go-quick             # iterative, with AI detection, no LLM judge
sprint-grader go                   # end-of-sprint, full pipeline
```

Limit any command to specific teams with `--projects team-01,team-02`.
All commands accept `--today YYYY-MM-DD` (handy for re-running historical
sprints) and `--data-dir` (lets you point at a different DB / repo cache).

## Configuration files

```
config/
├── course.toml                # main course + threshold + build config
├── grading.toml               # grading-sheet weights, AI modulation, penalties (see below)
├── architecture.toml          # T-P2.2 layered/onion-model rules (optional; absent → scan skipped)
├── rubric.md                  # PR documentation rubric (sent to Claude)
├── boilerplate_patterns.txt   # SHA-256 fingerprints excluded from cross-team detection
└── user_mapping.csv           # optional pm_username → github_username mapping
```

`config/grading.toml` is consumed only by `sprint-grader grading-sheet`. It is
**unrelated** to the `[grading]` block in `course.toml`, which controls
detector-threshold jitter (anti-gaming), not the 0–10 grade arithmetic.

## Data layout

```
data/
├── grading.db                           # SQLite — every metric the pipeline produces (Dropbox-backed)
└── entregues/
    ├── grading_sheet.xlsx                # self-recalculating grade workbook (grading-sheet)
    ├── sprint_1/
    │   ├── team-01.xlsx                  # per-team workbook
    │   └── _summary.xlsx                 # cross-team comparison
    ├── sprint_2/…
    └── <project>/
        ├── android-…/                    # cloned Android repo (rewritten by sync-reports)
        │   └── REPORT.md                 # multi-sprint markdown report
        └── spring-…/                     # cloned Spring Boot repo
```

`data/` is `.gitignore`d. The `sprint-grader sync-reports --push` command
regenerates every sprint's report and commits the updated `REPORT.md` files
back to each team's `main` branch (use with care — see "Subcommand reference"
below).

## The grading database

The schema is defined and migrated in [`crates/core/src/db.rs`](crates/core/src/db.rs).
Tables fall into a few groups:

- **Raw entities** — `projects`, `sprints`, `students`, `tasks`,
  `pull_requests`, `pr_commits`, `pr_reviews`, `task_pull_requests`,
  `github_users`. Populated by `collect`.
- **Code authorship** — `fingerprints`, `pr_survival`, `pr_line_metrics`,
  `student_sprint_survival`, `cross_team_matches`, `cosmetic_rewrites`.
  Populated by `survive`.
- **Per-student / per-team metrics** — `student_sprint_metrics`,
  `team_sprint_inequality`, `student_sprint_contribution`,
  `student_trajectory`. Populated by `analyze` + `inequality`.
- **Quality** — `method_metrics`, `satd_items`, `student_sprint_quality`.
- **Process** — `pr_compilation`, `pr_workflow_metrics`, `pr_regularity`,
  `student_sprint_temporal`, `team_sprint_collaboration`,
  `sprint_planning_quality`, `pr_submission_tiers`.
- **AI detection** — `pr_behavioral_signals`, `pr_ai_probability`,
  `file_ai_probability`, `student_style_profile`, `student_style_baseline`,
  `student_text_profile`, `text_consistency_scores`,
  `student_sprint_ai_usage`.
- **Curriculum** — `curriculum_concepts`, `curriculum_concepts_snapshot`
  (T-P2.5 per-sprint freeze), `curriculum_violations`.
- **Ownership** — `team_sprint_ownership` (T-P2.3 truck factor + ranked owners).
- **Architecture** — `architecture_violations` (T-P2.2; one row per
  `(file, broken rule, offending import)` from the `architecture.toml`
  scan).
- **Mutation testing** — `pr_mutation` (T-P2.4; one row per (PR, repo)
  with `(mutants_total, mutants_killed, mutation_score, duration_seconds)`
  parsed from Pitest's `mutations.xml`; populated only when
  `[mutation] enabled = true` and the matching build profile has a
  `mutation_command`).
- **Audit** — `pipeline_run` (T-P2.6: one row per `run_pipeline` invocation;
  records the seed, jitter %, and the realised threshold map when
  `[grading] hidden_thresholds = true`).
- **Flags** — `flags` (the consolidated per-student / per-PR anomaly list).
- **Declared AI usage** — `task_ai_usage`, `ai_usage_enum_domain`. Populated by
  `collect` from the TrackDev "Ús de IA" ENUM_PAIR on each task. **Requires a
  fresh `collect`** after enabling this feature; older DBs treat tasks as
  undeclared (assumed discount + `MISSING_AI_DECLARATION` flag).
- **Final grades** — `project_final_grade`, `student_final_grade`,
  `project_component_score`, `student_component_score`. Written by
  `grading-sheet`; read-mostly report output, not pipeline inputs.
- **LLM quality flags (Track B)** — `llm_quality_flag`. Advisory context only;
  the grade pipeline never reads this table.

The seven grade + AI-usage tables above are **deliberately excluded** from the
`diff-db --derived-only` parity contract (`DERIVED_TABLES` /
`COLLECTION_TABLES`). Re-running `grading-sheet` may change them without
implying a pipeline regression.

`pull_requests.attribution_errors` carries an accumulating JSON array of
`{kind, detail, observed_at}` entries describing data-quality issues found
while populating that PR (T-P1.5). Recognised kinds:

- `base_sha_fallback` — survival's `find_base_sha` fell back to `first_sha^1`.
  LAT/LAR/LS may be overstated for rebased PRs.
- `no_base_candidate` — survival could find no base at all; metrics for this
  PR are zero.
- `null_author_login` — at least one commit returned by `/pulls/{n}/commits`
  had `author.login == null`, OR the resolution loop couldn't map the GitHub
  login to a student.
- `github_http_error` — a PR or commits fetch failed; details include the
  HTTP error.
- `stale_github_fetch` — reserved (analysis-time check; not yet emitted).

These are **observability signals, not grading penalties**: composite scores
ignore them. The Markdown report renders a `⚠ (kind1, kind2)` glyph next to
the PR number cell when the column is non-empty. The column is capped at 20
entries per PR and survives a normal collect refresh (cleared only by an
explicit purge).

## Flag types

The `flags` table is consumed by the report; full enumeration lives in
[`crates/analyze/src/flags.rs`](crates/analyze/src/flags.rs). A few flag
types changed behaviour during the P0/P1 wave and warrant calling out:

- **`COSMETIC_REWRITE_VICTIM`** (INFO) and **`COSMETIC_REWRITE_ACTOR`**
  (WARNING) replaced the single `COSMETIC_REWRITE` flag (T-P1.2). The
  *actor* (rewriter) accumulates the WARNING toward their totals; the
  *victim* (original author) is informed via INFO without a penalty. Both
  details JSON cross-reference via `counterpart_user_id`. Legacy
  `COSMETIC_REWRITE` rows from pre-T-P1.2 DBs still render via a
  fallback in `report::flag_details`.
- **`CRAMMING`** is now keyed on the **commit author** (per
  `student_sprint_temporal.cramming_ratio`) rather than the task assignee
  (T-P1.1). Re-runs against pre-T-P1.1 baselines will show CRAMMING flags
  *moving* from task-owner rows to actual late-night committers — that is
  a correction, not a regression.
- **`LOW_SURVIVAL_RATE`** requires both a relative drop (≥
  `low_survival_rate_stddev` below team mean) **and** an absolute drop
  (`survival_rate_normalized < low_survival_absolute_floor`, default 0.85)
  before firing (T-P0.3). Previously the relative gate alone could flag
  uniformly-high teams.
- **`REGULARITY_DECLINING`** requires `pr_count >= 3` in **both** the
  current and previous sprint (T-P0.8). Below that threshold a single late
  merge dominates and the comparison is noise.
- **`ARCHITECTURE_DRIFT`** (WARNING, project-attributed as
  `PROJECT_<id>`) fires when this sprint's count of `architecture_violations`
  rows is strictly higher than the prior sprint's (T-P2.2). It's
  enabled by dropping a `config/architecture.toml` rule file and
  describes layered/onion/hexagonal model violations:
  `[[layers]]` blocks declare each named layer's package globs and its
  `may_depend_on` allow-list, and `[[forbidden]]` blocks blacklist
  imports for matching files (e.g. keep Spring web annotations out of
  the domain layer). When `architecture.toml` is absent the scan is
  skipped silently. T-P3.1 added `[[ast_rule]]` blocks that look
  inside class bodies via tree-sitter-java. After the AST-rubric
  migration (Waves 1–5) the engine supports fourteen kinds covering the
  Spring v8 and Android v1 rubrics: `forbidden_field_type`,
  `forbidden_constructor_param`, `forbidden_method_call`,
  `forbidden_return_type`, `forbidden_method_param`, `forbidden_import`,
  `must_null_in_lifecycle`, `forbidden_call_source`,
  `class_has_forbidden_annotation`,
  `method_annotation_visibility_mismatch`,
  `forbidden_constructor_call`,
  `parameter_annotation_requires_companion`,
  `field_count_with_type_pattern`, `class_requires_annotation`, plus
  `max_method_statements`. AST violations carry `(start_line, end_line)`
  so blame attribution can weight per-student responsibility (see
  `ARCHITECTURE_HOTSPOT`).
- **`ARCHITECTURE_HOTSPOT`** (per-student, severity tracks the worst
  contributing rule) sums each student's blame-attribution weight
  across this sprint's `architecture_violations` rows and fires when
  the total reaches `[detector_thresholds]
  architecture_hotspot_min_weighted` (default 2.0, T-P3.1). Weight is
  `lines_authored / total_lines_in_violation_range` from `git blame -w
  --ignore-revs-file`, so a one-line typo fix on a 30-line offending
  method gets ~3 % weight rather than 50 %. The team-level
  `ARCHITECTURE_DRIFT` keeps the regression headline; this flag points
  at the people who actually wrote the offending code.
- **Architecture rubrics (`config/spring-boot-rubric.md`,
  `config/android-rubric.md`)** — one human-readable spec per stack
  documenting the AST rules wired into `config/architecture.toml`. Per
  Wave 4 of the AST-rubric migration these files are **no longer fed to
  an LLM**; the deterministic AST engine in
  `crates/architecture/src/ast_rules.rs` is authoritative. The rubrics
  remain the reference material for instructors and the golden source
  for the integration fixtures
  (`crates/architecture/tests/spring_v8_fixtures.rs`,
  `crates/architecture/tests/android_v1_fixtures.rs`). YAML frontmatter
  (`rubric_version: <N>`) lives in each file independently; bump it when
  the policy changes. The legacy `architecture-spring.md` /
  `architecture-android.md` files describe the layered model and remain
  in the repo for reference.
- **Architecture LLM judge (`[architecture] llm_review`, default `false`)** —
  T-P3.3, **deprecated in Wave 4**. The per-file LLM judge has been
  replaced by the AST rules above. The `architecture_llm` crate still
  compiles for emergency rollback (set `llm_review = true` and pin
  `model_id`); under the default config the pipeline logs
  `[architecture] LLM judge disabled — AST rules in architecture.toml
  are authoritative` and does not invoke any model. A future
  project-wide LLM **explanation** pass — annotating AST findings with
  prose, not detecting new ones — is scaffolded as a `// FUTURE:` block
  at the bottom of `crates/architecture_llm/src/lib.rs`.
- **`LOW_MUTATION_SCORE`** (per-PR, attributed to the PR author)
  surfaces PRs whose Pitest mutation score is below the configured
  thresholds: WARNING below `[mutation] warning_threshold` (default
  0.30) and INFO below `info_threshold` (default 0.50) (T-P2.4). PRs
  with no `pr_mutation` row (mutation testing disabled or the profile
  has no `mutation_command`) and PRs with a NULL `mutation_score`
  (every mutant non-viable, or the run timed out) are silently
  skipped — we don't grade what we couldn't measure. Enable with
  `[mutation] enabled = true` and per-profile
  `mutation_command = "./gradlew pitest --info"` (or your build
  tool's equivalent in `scmMutationCoverage` mode).

## Grading sheet (project + student grades)

`sprint-grader grading-sheet` reads `grading.db` and writes:

1. Four grade tables (`project_final_grade`, `student_final_grade`, and the
   two `*_component_score` diagnostics tables).
2. `data/entregues/grading_sheet.xlsx` — a self-recalculating workbook whose
   `Weights` sheet mirrors [`config/grading.toml`](config/grading.toml).

Configuration lives in **`config/grading.toml`** (not `course.toml`). Key
sections:

| Section | Purpose |
|---|---|
| `[weights.project]` | Cross-team quality composite: documentation, code_quality, survival, architecture (present-renormalized mean → `Q`). |
| `[ai_usage]` | Declared-AI modulation: per-model `m`, per-level `l`, global `strength` (α), `floor_keep`, and assumed `(m,l)` for undeclared tasks. |
| `[penalty]` | Subtractive caps for CRITICAL static-analysis / complexity (project) and behavioural flags (student). |
| `[gate]` | Review routes (`NO_DELIVERY`, `PLAGIARISM`, `AI_REVIEW`) — informational; most do not auto-zero. |
| `[normalization]` | Anchors mapping raw metrics to 0–10 sub-scores. |
| `[output]` | `decimals` for display; `quantize_final = 0` keeps scoring continuous. |

### Grade model (summary)

Quality is measured **once per team** from four axes (documentation, code
quality, survival, architecture), comparable across projects. Each student's
grade redistributes that team quality by their share of **AI-discounted
effective story points**:

- Per DONE task: `effective = raw_points × keep`, where
  `keep = 1 − (1 − floor_keep) × α × m × l` from the declared "Ús de IA"
  model/level (undeclared tasks get an assumed discount + warning flag).
- **Project grade:** `final = Q_pen × A`, with `A = Σeffective / Σraw` (team
  AI factor).
- **Student grade:** `final = CLAMP(Q_pen × eff_u / mean_raw − penalty_u, 0, 10)`.

**Team-AI cancellation:** `A` affects the reported project grade, but it
**cancels for individuals** — each student's grade reflects their own declared
AI usage, not teammates'. Algebraically `base_u = Q_pen × eff_u / mean_raw`, so
an honest student with full retention can recover `Q_pen` even when the team
average is dragged down by heavy AI use elsewhere.

Example: Alice (no AI, 10 raw → 10 eff) and Bob (frontier×E, 10 raw → 2 eff),
`Q_pen = 8`, team size 2 → project `= 8 × 12/20 = 4.8`; Alice `= 8`, Bob `= 1.6`.

### Gates

| Gate | Trigger | Effect on grade |
|---|---|---|
| `NO_DELIVERY` | Cumulative effective points = 0 | `final = 0` (formula already yields zero) |
| `PLAGIARISM` | `CROSS_TEAM_SIMILARITY` on synthetic `PROJECT_<id>` | Review route only — no auto-zero |
| `AI_REVIEW` | Per-student: detected AI risk HIGH + low/absent declaration | Review route only — no auto-zero by default; project row is not gated by teammates' detection |

### Incremental runs

Grades are persisted **per project** in `grading.db`. The workbook is rebuilt
from the **union** of all graded projects on every export; each included project
is recomputed and re-persisted so the DB and xlsx stay aligned.

```bash
# Grade team-01 first
sprint-grader grading-sheet --projects team-01

# Add team-02; xlsx contains both teams (team-01 refreshed from current evidence)
sprint-grader grading-sheet --projects team-02

# Refresh everyone after a pipeline re-run, without naming projects
sprint-grader grading-sheet --workbook-only

# Persist grades only (no xlsx write)
sprint-grader grading-sheet --projects team-01 --no-workbook
```

### Importing edited weights

```bash
sprint-grader grading-sheet --import-weights data/entregues/grading_sheet.xlsx
```

Reads the `Weights` sheet via calamine and overwrites `config/grading.toml`
without running a grading pass.

### Interactive grading (`grading-html`)

`sprint-grader grading-html` is a presentation sibling of `grading-sheet`: it
runs the **same** grade+persist path (so persisted grades are byte-identical and
running one after the other is a no-op for `diff-db --derived-only`) and emits
one double-clickable, fully offline `data/entregues/grading.html`.

```bash
# Whole cohort
sprint-grader grading-html

# One team — the snapshot is scoped to the named teams (safe for handouts)
sprint-grader grading-html --projects team-01

# Rebuild from all graded projects without a new grade pass
sprint-grader grading-html --workbook-only
```

What the page adds over the XLSX:

- **Live knob tuning.** All 25 scalar knobs (weights, AI modulation, penalty
  points, normalization anchors) plus the model/level maps and a `penalty_mode`
  selector recompute every project/student grade **in the browser**, instantly,
  with no pipeline re-run. The architecture knobs (`k_crit` / `k_warn` /
  `arch_norm`) are live too — the snapshot carries the raw crit/warn counts.
- **SQL-native exploration.** The page embeds a small denormalized SQLite
  snapshot (via sql.js) that it — and any reviewing agent — queries directly.
  New visualizations are added by appending one entry to the `VIEWS` registry
  (SQL + chart kind); the agent guide is `crates/grading_html/SCHEMA_NOTES.md`.
- **An always-on parity self-test.** On load (default knobs) the in-browser
  arithmetic must reproduce the Rust-computed grades within
  `0.5·10⁻ᵈᵉᶜⁱᵐᵃˡˢ`, or a red banner declares parity broken. Tuning a knob turns
  the banner neutral ("what-if, not the official grades"); **Reset knobs**
  restores defaults and re-verifies.

The file is **single-file, offline, no-network**: sql.js (wasm) and math.js are
vendored and base64-embedded, and the wasm runs via `wasmBinary` — nothing is
fetched, nothing is written to `localStorage`. The header shows `weights_version`
(a SHA-256 of the config) so you can tell which knob vector built a given file.

Treat it as **faculty-internal**: it embeds the whole cohort's grades, flags and
AI-detection signals behind a live SQL console. For per-team handouts use
`--projects`, which scopes the snapshot to the named teams. `grading-html` does
not implement `--import-weights`; if you edited knobs in the XLSX, run
`grading-sheet --import-weights …` first.

### `quality-flags` (Track B)

`sprint-grader quality-flags` populates advisory `llm_quality_flag` rows (file
tier, then optional holistic synthesis). `grading-sheet` exports them on the
`LLM_Flags` workbook sheet (`scope`, `target_ref`, `category`, `severity`,
`summary`, …). LLM output never feeds the grade model; `grading-sheet` does not
trigger any LLM pass.

Configuration lives in **`course.toml` `[quality_llm]`** (not `grading.toml`):

| Knob | Purpose |
|---|---|
| `backend` | `claude-cli` (default), `cursor-cli`, or `ollama` |
| `model_id` | **Required** when running quality-flags — pin a cheap model |
| `prompt_version` | Cache-bust tag for `--resume` |
| `rubric_path` | Markdown rubric (default `config/quality-llm-rubric.md`) |
| `max_holistic` | Holistic LLM calls per project (`0` = file tier only; `1` = team-wide synthesis; `≥2` = per-repo passes up to cap) |
| `max_files_per_project` | Pre-filter cap on file-tier calls |
| `skip_globs` | Skip generated/build paths before LLM |

**Incremental** (mirrors `grading-sheet`): `--projects` refreshes only the
listed teams' `llm_quality_flag` rows; other teams' flags are preserved.
`grading-sheet` always exports **all** flags on the `LLM_Flags` sheet.

```bash
sprint-grader quality-flags --projects team-01
sprint-grader quality-flags --projects team-02   # team-01 flags kept
sprint-grader grading-sheet --workbook-only        # LLM_Flags shows both teams
```

## Subcommand reference

Stage commands (each runs one analysis stage against sprints with
`start_date <= --today`):

| Command | Reads | Writes |
|---|---|---|
| `collect` | TrackDev API, GitHub API | raw entity tables, repo clones |
| `compile` | repo clones + `pull_requests` | `pr_compilation`, `compilation_failure_summary` |
| `survive` | repo clones + git history | `fingerprints`, `pr_survival`, `pr_line_metrics`, `student_sprint_survival` |
| `analyze` | metrics inputs | `student_sprint_metrics`, `flags` |
| `inequality` | `student_sprint_metrics` | `team_sprint_inequality`, `student_sprint_contribution`, `student_trajectory` |
| `quality` | repo clones | `method_metrics`, `satd_items`, `student_sprint_quality` |
| `static-analysis [--no-spotbugs]` | repo clones | `static_analysis_findings`, `static_analysis_finding_attribution`, `static_analysis_runs` (requires `config/static_analysis.toml`) |
| `process` | PR + commit data | `sprint_planning_quality`, `pr_regularity`, `student_sprint_temporal`, `team_sprint_collaboration` |
| `evaluate` | `pull_requests` | `pr_doc_evaluation`, `task_description_evaluation` (uses Claude API if available) |
| `task-similarity` | `tasks`, `pr_line_metrics` | `task_similarity_groups`, `task_group_members` |
| `temporal-analysis` | `pull_requests`, sprint dates | `pr_submission_tiers` |
| `ai-detect` | repos + most prior tables | the AI-detection table family |
| `curriculum --rebuild` | LaTeX slides | `curriculum_concepts` |
| `report` | the entire DB | `.xlsx` files + per-project `REPORT.md` |

Orchestration / utility:

| Command | Purpose |
|---|---|
| `run-all [--skip-static-analysis]` | Additive full pipeline; no AI detection. Incremental collection (watermark + ETag); skips survival/compile/architecture per project when no new PRs/tasks. Static-analysis stage runs when `config/static_analysis.toml` exists; pass the flag to bypass. |
| `iterate [--skip-static-analysis]` | Same as `run-all`. Carries a historical `--skip-arch-llm` flag from before Wave 4; with the LLM judge off by default it is now a no-op for any course that hasn't opted back in. AST-based architecture scan always runs. |
| `go [--dry-run] [--require-clean-tree] [--skip-static-analysis]` | End-of-sprint: **always** purges then re-collects → full pipeline + AI detection. `--projects` only narrows the purge scope (without it, every project in the DB is wiped). `--dry-run` previews the cascade per-table row counts and exits before any pipeline stage runs. `--require-clean-tree` refuses to start if `git status --porcelain` reports a dirty working tree. |
| `go-quick [--dry-run] [--require-clean-tree] [--run-static-analysis]` | Same purge/re-collect contract as `go`, but PR doc evaluation always runs heuristic-only (no Claude calls) and the static-analysis stage is skipped by default — pass `--run-static-analysis` to opt in. Same `--dry-run` / `--require-clean-tree` semantics as `go`. |
| `sync-reports [--push]` | Regenerate `REPORT.md` for every sprint up to today; optionally commit + push to each team's `main`. |
| `purge-cache --line-metrics --survival --compilation --doc-eval [--dry-run] [--require-clean-tree]` | Selectively drop derived rows so the next run recomputes them. `--dry-run` rewrites each `DELETE` as a `SELECT COUNT(*)` over the same predicate and prints projected row counts table-by-table without modifying the DB. `--require-clean-tree` is the same guard as on `go`. |
| `debug-pr-lines` | Dump LAT/LAR/LS computation for individual PRs (diagnostics). |
| `reset-local-scores [--projects …]` | Delete `pr_doc_evaluation` rows written by the local-hybrid judge (`justification LIKE 'local:%'`). Non-local rows (Haiku, heuristic) are preserved; project-scoped via `--projects`. Run after retraining the regressor to invalidate stale local scores. |
| `grading-sheet [--projects …] [--out PATH] [--import-weights XLSX] [--workbook-only] [--no-workbook]` | Compute 0–10 project + student grades from `grading.db`; persist grade tables; write merged `grading_sheet.xlsx` (default: `data/entregues/grading_sheet.xlsx`). `--projects` grades a subset; the workbook includes every project in `project_final_grade`, each refreshed on export. `--workbook-only` skips new grading and rebuilds the xlsx from existing grade rows. `--no-workbook` persists only. `--import-weights` updates `config/grading.toml` and exits. Deterministic — no LLM. Requires `collect` for `task_ai_usage`. |
| `quality-flags [--projects …] [--max-holistic N] [--resume]` | Feedback-only LLM quality flags: file tier + holistic synthesis; writes `llm_quality_flag`. Backend via `[quality_llm] backend`: `claude-cli` (default), `cursor-cli`, or `ollama`. `--max-holistic 0` skips holistic. Incremental `--projects` scope; never alters grades. |
| `diff-db DB_A DB_B [--tables …] [--derived-only] [--ignore-cols T:c1,c2] [--dump-diffs]` | Table-by-table checksum diff between two `grading.db` files; exits non-zero on mismatch. Used to verify pipeline changes don't drift. Grade + declared-AI tables are parity-exempt (see [Grading sheet](#grading-sheet-project--student-grades)). |

Global flags accepted by every command:

- `--today YYYY-MM-DD` — reference date; defaults to today (UTC).
- `--projects team-01,team-02` — restrict to a subset of teams.
- `--project-root PATH` — where `config/` and `.env` live.
- `--data-dir PATH` — where `grading.db` and repo clones live (defaults to
  `./data`).
- `-v / --verbose` — bumps `tracing` output to `debug`.

## Environment variables

| Variable | Used by | Notes |
|---|---|---|
| `TRACKDEV_TOKEN` | `collect` | TrackDev API auth. |
| `GITHUB_TOKEN` | `collect` | GitHub PAT with read access to the course org. |
| `ANTHROPIC_API_KEY` | `evaluate`, `ai-detect` | Optional. Without it, `evaluate` runs heuristic-only and the LLM-judge AI signal is skipped. |
| `CURSOR_API_KEY` | `evaluate` (`cursor-cli` judge) | Optional when the Cursor Agent CLI is logged in via `agent login`; otherwise set for headless runs. Keep `[evaluate] judge_workers = 1` — parallel `agent` startups race on `~/.cursor/cli-config.json` and fail with `ENOENT` on the temp-file rename. |
| `ANTHROPIC_MODEL` | `evaluate` (anthropic-api backend) | Overrides `[evaluate].model_id`. Pipeline default is `claude-haiku-4-5-20251001`; do not set this to an Opus id unless you want to burn Max quota. |
| `SURVIVAL_RESTRICT_TO_PR_FILES` | `survive` | If set, restricts fingerprinting to files touched by PRs (40–70% faster, default off). |
| `RUST_LOG` | all | Standard `tracing-subscriber` filter; overrides the `--verbose` shorthand. |

## Development

```bash
cargo fmt
cargo clippy --workspace --all-targets
cargo test --workspace
```

The workspace pins `rust-toolchain.toml` to stable. SQLite is bundled via
`rusqlite`'s `bundled` feature, so there's no system `libsqlite3` dependency.

For dual-run verification when refactoring an analysis stage:

```bash
cp data/grading.db /tmp/before.db
sprint-grader run-all
sprint-grader diff-db /tmp/before.db data/grading.db --derived-only
```

## License

MIT.
