> Black-box / integration test plan for everything shipped in waves
> P0, P1 and P2. Goal: exercise the **outer surface** of the system —
> CLI subcommands, config files, env vars, and generated artefacts
> (`grading.db`, `REPORT.md`, sprint XLSX) — rather than calling
> internal functions. Existing `#[cfg(test)] mod tests` blocks already
> cover function-level correctness; this plan covers what a grader
> using the binary actually sees.

# Goals and non-goals

**Goals.**
- One test scenario per behavioural change shipped in P0/P1/P2.
- Hermetic: no real TrackDev / GitHub / Anthropic calls. The CLI
  must run to completion with `TRACKDEV_TOKEN` / `GITHUB_TOKEN` /
  `ANTHROPIC_API_KEY` unset.
- Assertions hit the artefacts a grader would look at — DB rows
  (canonical), markdown report (human-readable), XLSX (where the row
  is the contract).
- Reuse the T-P2.7 affordances (`Config::test_default()`,
  `db::apply_schema`, `crates/analyze/tests/common/`) wherever the new
  scenarios are detector-shaped.

**Non-goals.**
- Real network. Anything that requires `collect` to hit TrackDev or
  GitHub is staged via a pre-seeded `grading.db` instead.
- Running real Pitest / real Gradle. T-P2.4's mutation pipeline is
  exercised by feeding the builder a `mutation_command` that writes a
  fixed `mutations.xml` to the expected path — same surface, no JVM.
- Re-deriving function-level invariants. If a unit test in
  `crates/<x>/src/.../tests` already proves a numeric formula, this
  plan asserts only that the binary surfaces the value end-to-end.

# Test infrastructure (do this first)

## T-T0.1 — Black-box integration crate scaffolding

- **Type:** infra
- **Files:** new `crates/blackbox/Cargo.toml`,
  `crates/blackbox/tests/`, `Cargo.toml` (workspace member),
  optional dev-deps `assert_cmd = "2"`, `predicates = "3"`,
  `insta = { version = "1", features = ["filters"] }`,
  `tempfile`.

A new workspace crate with no `src/lib.rs` content (or a thin one
exposing the helpers below) and `tests/*.rs` integration tests. The
crate exists to:

- House the fixture builders (DB + filesystem layout) in one place so
  every scenario gets the same shape of `data/entregues/<project>/`.
- Build the `sprint-grader` binary once via `assert_cmd::Command::cargo_bin`
  rather than each scenario re-resolving the binary path.
- Centralise the `insta` filter rules that strip timestamps, durations,
  and tempdir paths from snapshots.

**Acceptance.** `cargo test -p sprint-grader-blackbox` runs and
finds zero failures (no scenarios yet).

## T-T0.2 — Fixture grading.db builder

- **Files:** `crates/blackbox/src/fixture.rs`.

A pure-Rust helper that builds a complete `grading.db` from a
parameter struct: N projects, M sprints per project, S students per
project, T tasks per sprint with assignees, P pull requests with
commits / reviews, optional fingerprints / pr_compilation /
pr_mutation rows. Default seed is "happy path team-01 with two
sprints"; scenarios pass tweaks via builder methods (`with_late_pr`,
`with_cosmetic_rewrite`, `with_squash_merge_pre_authors`, …).

**Acceptance.** A scenario can write `Fixture::default().build(&path)`
and run `analyze` against the resulting DB without panicking.

## T-T0.3 — Hermetic CLI runner

- **Files:** `crates/blackbox/src/runner.rs`.

A `Runner` struct that owns a `tempfile::TempDir`, lays out
`config/` + `data/entregues/`, copies `config/course.toml` from the
repo (or accepts an override via `with_config_text`), unsets the
three network env vars, and exposes `runner.run(&["go-quick",
"--today", "2026-02-15"])` returning a captured stdout/stderr +
exit code.

**Acceptance.** A scenario can invoke `go-quick` against a seeded
fixture and the run completes (exit 0 or expected non-zero) with no
real-network calls — verified by setting `HTTP_PROXY=http://0.0.0.0:1`
in the runner and asserting no connection error reaches stderr.

## T-T0.4 — Snapshot rules for REPORT.md

- **Files:** `crates/blackbox/src/snapshot.rs`.

A small wrapper around `insta::assert_snapshot!` that pre-filters
non-deterministic fields out of `REPORT.md`: ISO timestamps,
elapsed-seconds, tempdir absolute paths, the realised threshold
band (T-P2.6), and the `fitted_at` cell from the cumulative
summary. Snapshots live in `crates/blackbox/tests/snapshots/`.

**Acceptance.** Running the same fixture twice produces identical
snapshots after filters. Re-running with `INSTA_UPDATE=force` updates
in place.

# Wave P0 — Bug-fix correctness

## T-T1.1 — Stage ordering in orchestrated pipelines (P0.1)

GIVEN a fixture with two sprints and inequality-eligible students.
WHEN `go-quick --today <s2 end>` runs.
THEN the run completes with no per-stage error in the log AND
`team_inequality` flag rows in `flags` for sprint 2 (which require
`team_sprint_inequality` to be populated by an earlier stage).

**Acceptance.** Two consecutive `go-quick` invocations yield identical
flag rows (`diff-db --derived-only` reports zero drift).

## T-T1.2 — Doc score column non-NULL on go-quick (P0.2)

GIVEN a fixture with two PRs whose `body` is non-empty markdown.
WHEN `go-quick --today <s end>` runs without `ANTHROPIC_API_KEY`.
THEN every `student_sprint_metrics.avg_doc_score` for the sprint is
non-NULL (heuristic eval ran). The Markdown report's "Doc score"
column is populated.

## T-T1.3 — LOW_SURVIVAL_RATE absolute floor (P0.3)

GIVEN a team where all students have `survival_rate_normalized ≥ 0.95`
but one is `1.5σ` below the team mean.
WHEN `analyze flags --sprint <id>` runs.
THEN no `LOW_SURVIVAL_RATE` flag fires for that student.
GIVEN the same team but the outlier drops to `0.70`.
THEN the flag fires.

## T-T1.4 — Velocity CV filters zero-velocity sprints (P0.4)

GIVEN three sprints with team velocities `[0, 0, 12]`.
WHEN the pipeline runs.
THEN `team_sprint_planning.velocity_cv` for the third sprint is
computed over `[12]` only (CV = 0 with no flag), not over
`[0, 0, 12]`.

## T-T1.5 — Markdown-link-only PR descriptions are penalised (P0.5)

GIVEN a PR whose body is a sequence of markdown links and nothing
else (`[Trello](url)\n[Figma](url)`).
WHEN `evaluate run --sprint <id>` runs heuristically.
THEN the PR's `pr_doc_evaluation.score` is at the lowest tier (≤ 1)
and the markdown report's "Doc score" cell reflects this.

## T-T1.6 — Survival ignores whitespace and respects ignore-revs (P0.6)

GIVEN a small synthetic git repo with one commit that adds code and
a follow-up commit that re-indents the same lines (whitespace-only
diff). Provide a `.git-ignore-revs` file containing the second SHA.
WHEN `survive --sprint <id>` runs.
THEN every line is attributed to the first commit's author. Adding
or removing the ignore-revs file changes the attribution.

(Implementation note: this scenario needs a real git repo on disk;
use `Fixture::with_git_repo` to build one in the temp tree.)

## T-T1.7 — find_base_sha fallback writes attribution_errors (P0.7)

GIVEN a PR whose first-commit-parent is unknown to git locally.
WHEN `survive` runs.
THEN `pull_requests.attribution_errors` for that PR contains an entry
with `kind = "base_sha_fallback"`. The Markdown report renders the
`⚠ (base_sha_fallback)` glyph next to the PR number.

## T-T1.8 — Min-PR-count gate on REGULARITY_DECLINING (P0.8)

GIVEN two sprints where each has only `pr_count = 2` for student
Alice.
WHEN `analyze flags --sprint <s2>` runs.
THEN no `REGULARITY_DECLINING` flag fires for Alice (the gate
suppresses noise from low N), even when her sprint-2 PR is much
later than sprint-1's.
GIVEN both sprints inflated to `pr_count = 5`.
THEN the flag fires.

# Wave P1 — Medium improvements

## T-T2.1 — CRAMMING attributes to commit author, not task assignee (P1.1)

GIVEN a task assigned to Alice but with merged-PR commits authored by
Bob in the cramming window (last 24 h).
WHEN flag detection runs.
THEN the `CRAMMING` flag's `student_id` is `bob`, not `alice`. Re-running
on the same DB after editing one commit's author moves the flag to the
new author.

## T-T2.2 — COSMETIC_REWRITE produces VICTIM + ACTOR pair (P1.2)

GIVEN a fingerprint pair where Alice's original statements are
cosmetically rewritten by Bob.
WHEN `analyze flags --sprint <id>` runs.
THEN exactly two flags exist for that pair: `COSMETIC_REWRITE_VICTIM`
(INFO, `student_id = alice`) and `COSMETIC_REWRITE_ACTOR` (WARNING,
`student_id = bob`); their `details.counterpart_user_id` cross-reference
each other.

GIVEN a pre-T-P1.2 DB with the legacy `COSMETIC_REWRITE` row.
WHEN the markdown report runs.
THEN the legacy row still renders via the fallback path.

## T-T2.3 — Detector thresholds in course.toml are honoured (P1.3)

GIVEN a custom `course.toml` overriding `[detector_thresholds]
gini_warn = 0.10` (lower than default).
WHEN `go-quick` runs against a fixture with a Gini around 0.20.
THEN a `TEAM_INEQUALITY` flag with `severity = WARNING` appears.
GIVEN the same fixture but `gini_warn = 0.50`.
THEN no flag fires.

This scenario sweeps each of the 13 knobs T-P1.3 moved into config —
table-driven, one assertion per knob.

## T-T2.4 — pr_pre_squash_authors drives AUTHOR_MISMATCH (P1.4)

GIVEN a squash-merged PR whose `github_author_login = bob` but
`pr_pre_squash_authors = ["alice", "carol"]`.
WHEN flag detection runs.
THEN `AUTHOR_MISMATCH` fires citing the pre-squash authors. Removing
the `pr_pre_squash_authors` row and re-running falls back to
`pr_commits` — the flag still fires but with the fallback authors.

## T-T2.5 — attribution_errors accumulates from four trigger sites (P1.5)

GIVEN a fixture that triggers each of `base_sha_fallback`,
`no_base_candidate`, `null_author_login`, and `github_http_error`
on the same PR across two pipeline runs.
WHEN we read `pull_requests.attribution_errors` for that PR.
THEN it contains four entries (capped at 20), each with
`{kind, detail, observed_at}`. The markdown report renders
`⚠ (base_sha_fallback, no_base_candidate, …)` next to the PR.

## T-T2.6 — purge --dry-run prints what would be deleted (P1.6)

GIVEN a populated `grading.db`.
WHEN `purge --dry-run --tables flags,architecture_violations` runs.
THEN stdout lists the row counts that would be removed AND the DB is
unchanged. Without `--dry-run` the same invocation truncates those
tables.

## T-T2.7 — README parity check (P1.7)

GIVEN the current README's Subcommand reference table.
WHEN we walk every subcommand documented and run `<subcommand> --help`.
THEN the help text includes every flag named in the README, and vice
versa (no documented flag is missing, no undocumented flag exists).
This is a regression guard against the docs and the CLI drifting.

# Wave P2 — Architectural improvements

## T-T3.1 — Estimation bias table populated and flag fires (P2.1)

GIVEN a fixture where Alice's tasks all have inflated point values
(her per-task ratio is consistently +1 logit) and she has at least
5 estimated tasks.
WHEN `go-quick` runs.
THEN `student_estimation_bias` has a row for Alice with
`beta_mean > 0.5`; the `ESTIMATION_BIAS` flag fires WARNING with
`details.direction = "over"`; the cumulative student summary in
`REPORT.md` shows `▲ +<value>` next to her row.
GIVEN Bob with `n_tasks = 4` and the same inflation.
THEN no flag fires (small-sample mitigation).

## T-T3.2 — Architecture scan and ARCHITECTURE_DRIFT flag (P2.2)

GIVEN a fixture android-* repo on disk under `data/entregues/<team>/`
containing two Java files: one in `com.x.domain.user` importing a
class in `com.x.application`, and a `config/architecture.toml`
declaring `domain → may_depend_on = []`.
WHEN `go-quick` runs.
THEN `architecture_violations` has at least one row for the offending
import. With a prior sprint of zero violations, `ARCHITECTURE_DRIFT`
fires (project-attributed `PROJECT_<id>`). Removing
`config/architecture.toml` causes the scan to skip silently and the
table stays empty.

## T-T3.3 — Ownership table populated and treemap rendered (P2.3)

GIVEN a fixture with 5 students and pre-seeded `fingerprints` rows
where two students together own ≥ 95 % of statements.
WHEN `go-quick` runs.
THEN `team_sprint_ownership` has `truck_factor = 2` and a
two-element `owners_csv`. `REPORT.md` Section A includes an
`<svg>` block whose markup contains those two student names.

## T-T3.4 — Mutation testing end-to-end with a fake Pitest (P2.4)

GIVEN `[mutation] enabled = true` and a build profile whose
`mutation_command` is a shell line that writes a fixed
`mutations.xml` (5 mutants, 1 killed) to
`build/reports/pitest/mutations.xml` and exits 0.
GIVEN a fixture PR whose primary build succeeds (small synthetic
script).
WHEN `compile --sprint <id>` runs.
THEN `pr_mutation` has a row with `mutants_total = 5,
mutants_killed = 1, mutation_score = 0.20` (assuming no non-viable),
AND a subsequent `analyze flags --sprint <id>` produces a
`LOW_MUTATION_SCORE` flag at WARNING severity attributed to the PR
author.

GIVEN the same fixture with `[mutation] enabled = false`.
THEN `pr_mutation` is empty and no flag fires.

GIVEN the same fixture with `mutation_command` writing an empty
`<mutations></mutations>` (or only `NON_VIABLE` rows).
THEN `pr_mutation.mutation_score` is NULL and the flag is silent.

## T-T3.5 — Curriculum freeze is idempotent and snapshot wins (P2.5)

GIVEN a populated `curriculum_concepts` live table.
WHEN `freeze-curriculum --sprint <id>` runs.
THEN `curriculum_concepts_snapshot` for that sprint has rows mirroring
the live table. Running `freeze-curriculum` again is a no-op
(idempotent) and the snapshot row count is unchanged.

GIVEN the live table is then mutated.
WHEN AI detection's curriculum check runs for the frozen sprint.
THEN the analysis uses the snapshot's allowed concepts (proved by an
assertion on a synthesised flag count that depends on the older list),
not the live table.

## T-T3.6 — Threshold jitter reproducible by `--today` (P2.6)

GIVEN `[grading] hidden_thresholds = true, jitter_pct = 0.10` and a
fixture course_id.
WHEN `run-all --today 2026-04-26` runs twice.
THEN the two `pipeline_run` rows for those runs have identical
`thresholds_json` and `seed`, AND `diff-db --derived-only` between
the two output DBs is empty.
WHEN we re-run with `--today 2026-04-27`.
THEN `thresholds_json` differs (within the band) but `REPORT.md`
contains no realised numeric thresholds — only the published value
± band.

## T-T3.7 — Per-detector regression fixtures still pass (P2.7)

This is a meta-scenario: assert that `cargo test -p sprint-grader-analyze
--test 'flag_*'` runs and every test passes, with at least N tests
matching the glob (so renaming a fixture away cannot silently lose
coverage). N is the count at the time this plan lands; bump it when
new detectors are added.

# Cross-cutting scenarios

## T-T4.1 — Reproducibility (run-all twice = zero drift)

GIVEN a fully-seeded fixture.
WHEN `run-all --today <fixed>` runs twice into separate DB paths.
THEN `diff-db --derived-only <a> <b>` exits 0. (The plan's
"reproducibility check" from CLAUDE.md, but as an enforced test.)

## T-T4.2 — XLSX shape contract

GIVEN a fixture project with two sprints.
WHEN reports run.
THEN each sprint's XLSX exists at
`data/entregues/sprint_K/<team>.xlsx`, has the expected sheet names
(`Summary`, `PRs`, `Flags`, …) and the per-row column count matches
the renderer's header row. (Don't snapshot the bytes — XLSX is a zip
of XML; assert structure only.)

## T-T4.3 — Markdown report golden snapshot

GIVEN the canonical fixture.
WHEN `go-quick --today <fixed>` runs.
THEN `REPORT.md` matches the stored `insta` snapshot (after the
filters from T-T0.4).

This catches accidental visual regressions in any of the report
sections — Section A (team snapshot), Section B (per-student
dashboards), Section D (cumulative summary, β_u column from T-P2.1,
Architecture conformance subsection from T-P2.2, treemap from T-P2.3).

## T-T4.4 — Help text smoke

For every subcommand listed in `crates/cli/src/main.rs`'s `Command`
enum:

GIVEN `sprint-grader <subcommand> --help`.
THEN exit code is 0 and stdout is non-empty. Quick guard against a
cli refactor breaking discoverability.

## T-T4.5 — Missing-config behaviour

GIVEN no `config/architecture.toml`.
THEN `run-all` logs the absence at INFO and the architecture scan is
skipped silently — `architecture_violations` is empty and no error
surfaces.

GIVEN no `ANTHROPIC_API_KEY`.
THEN `evaluate` emits the heuristic path and `pr_doc_evaluation` is
populated with `provider = "heuristic"`.

GIVEN missing `course.toml`.
THEN the binary exits non-zero with a message naming the expected
path.

# Execution order

1. **T-T0.x first** (infrastructure). Without the fixture builder /
   runner / snapshot helpers, every scenario re-implements the same
   plumbing.
2. **T-T1.x and T-T2.x next** in any order — they share the fixture
   builder but are mutually independent.
3. **T-T3.x next** — the architecture / mutation / curriculum scenarios
   build small dedicated fixtures on top of the base.
4. **T-T4.x last** — cross-cutting; expects every prior scenario's
   artefacts to be settled.

# What this plan does NOT cover

- Real TrackDev / GitHub / Anthropic round-trips — out of scope, would
  require recorded HTTP fixtures and live tokens.
- LLM PR-doc evaluation against a real model — heuristic path is
  exercised; the LLM path is a stub today.
- Pitest against a real Java project — T-T3.4 uses a fake
  `mutation_command`; running the actual tool against a fixture Java
  project is a follow-up if mutation testing is rolled out for
  course use.
- Performance / load testing — the codebase has no SLO and no scenario
  here would catch regression beyond a doubling of runtime.
