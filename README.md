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
(`data/entregues/grading.db`). Every stage is a pure function of the DB plus
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
                       data/entregues/grading.db
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
| [`curriculum`](crates/curriculum) | knowledge base | Parses LaTeX slide decks to extract the set of concepts / imports taught in each sprint. Used downstream by `ai_detect` to flag code that uses material the team hasn't been taught yet. |
| [`repo_analysis`](crates/repo_analysis) | 6 | Clusters tasks by `(stack, layer, action)` with MAD-based outlier detection; classifies merged PRs into submission timing tiers (early / on-time / late / cramming). |
| [`ai_detect`](crates/ai_detect) | 7 | Behavioural signals (single-commit dumps, fix-up patterns, line-per-minute productivity), per-student stylometric baseline + deviation, curriculum violations, text-consistency score, and Bayesian fusion into per-PR / per-file / per-student AI-usage probabilities. |
| [`report`](crates/report) | 8 | Per-sprint Excel workbooks (one per team + cross-team summary) and a multi-sprint Markdown `REPORT.md` committed back into each team's Android repo with inline SVG sparklines. |
| [`orchestration`](crates/orchestration) | glue | The three full-pipeline variants (`run-all`, `go`, `go-quick`), parallel sprint execution via `rayon`, cache purge, the `diff-db` table-by-table dual-run checker, and the `sync-reports` publisher. |
| [`cli`](crates/cli) | binary | The `sprint-grader` clap CLI exposing every stage as its own subcommand plus the full-pipeline aggregates. |

### Pipeline variants

The orchestration crate exposes three top-level pipelines:

- **`run-all`** — fresh full run; survival failure is fatal; *does not* purge
  existing rows. Use for an additive build of the DB after `collect`.
- **`go`** — end-of-sprint evaluation. Purges existing rows for the targeted
  projects, re-collects, runs the full pipeline including AI detection, and
  is tolerant to survival errors so you can still get a partial report.
- **`go-quick`** — same as `go` but skips the (expensive) Claude code-review
  pass. Designed for iterative work mid-sprint.

All three use a `rayon` thread pool to fan sprints out across worker
connections (SQLite WAL mode allows concurrent readers + serialised writers).
Stages 5–7 (quality / process / repo_analysis / ai_detect) are each pure
functions of the DB, so re-running them is cheap.

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
ANTHROPIC_API_KEY=sk-ant-...      # optional
ANTHROPIC_MODEL=claude-opus-4-7   # optional, default
```

Edit [`config/course.toml`](config/course.toml) — the most important keys:

| Section | Key | Purpose |
|---|---|---|
| `[course]` | `name`, `num_sprints`, `pm_base_url`, `github_org`, `course_id` | Identifies which TrackDev course + GitHub org to pull from. |
| `[course]` | `claude_scripts_path` | Path to the Claude session library used by LLM evaluation. |
| `[thresholds]` | `cramming_hours`, `micro_pr_max_lines`, `low_doc_score`, `contribution_imbalance_stddev`, … | Tunables for the flag detector. |
| `[[build.profiles]]` | `repo_pattern`, `command`, `timeout_seconds` | Per-repo-type build command run by `compile`. The pattern is a regex against the repo directory name. |
| `[build]` | `max_parallel_builds`, `stderr_max_chars`, `skip_already_tested` | Compile-stage concurrency + caching behaviour. |
| `[curriculum]` | `slides_dir`, `extra_allowed_imports` | Where to find the LaTeX slides; imports always considered "taught". |
| `[regularity]` | `midpoint_hours`, `steepness`, band thresholds | Shape of the sigmoid that scores how early before the deadline a PR landed. |
| `[repo_analysis]` | `group_min_size`, `mad_k_threshold`, `temporal_*_hours` | MAD-based outlier detection + temporal-tier cutoffs. |

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
├── rubric.md                  # PR documentation rubric (sent to Claude)
├── boilerplate_patterns.txt   # SHA-256 fingerprints excluded from cross-team detection
└── user_mapping.csv           # optional pm_username → github_username mapping
```

## Data layout

```
data/
└── entregues/
    ├── grading.db                        # SQLite — every metric the pipeline produces
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
- **Curriculum** — `curriculum_concepts`, `curriculum_violations`.
- **Flags** — `flags` (the consolidated per-student / per-PR anomaly list).

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
| `run-all` | Additive full pipeline; no AI detection. |
| `go` | End-of-sprint: purge → re-collect → full pipeline + AI detection. |
| `go-quick` | Same as `go`, skipping the LLM code-review pass. |
| `sync-reports [--push]` | Regenerate `REPORT.md` for every sprint up to today; optionally commit + push to each team's `main`. |
| `purge-cache --line-metrics --survival --compilation --doc-eval` | Selectively drop derived rows so the next run recomputes them. |
| `debug-pr-lines` | Dump LAT/LAR/LS computation for individual PRs (diagnostics). |
| `diff-db DB_A DB_B [--tables …] [--derived-only] [--ignore-cols T:c1,c2] [--dump-diffs]` | Table-by-table checksum diff between two `grading.db` files; exits non-zero on mismatch. Used to verify pipeline changes don't drift. |

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
| `ANTHROPIC_MODEL` | `evaluate` | Defaults to `claude-opus-4-7`. |
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
cp data/entregues/grading.db /tmp/before.db
sprint-grader run-all
sprint-grader diff-db /tmp/before.db data/entregues/grading.db --derived-only
```

## License

MIT.
