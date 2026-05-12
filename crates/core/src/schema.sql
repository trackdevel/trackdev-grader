-- Schema for the grading database. Kept byte-identical to src/db/schema.py
-- in the Python reference so both implementations can share grading.db.

CREATE TABLE IF NOT EXISTS students (
    id TEXT PRIMARY KEY,
    username TEXT,
    github_login TEXT,
    full_name TEXT,
    email TEXT,
    team_project_id INTEGER
);

CREATE TABLE IF NOT EXISTS projects (
    id INTEGER PRIMARY KEY,
    slug TEXT,
    name TEXT
);

CREATE TABLE IF NOT EXISTS sprints (
    id INTEGER PRIMARY KEY,
    project_id INTEGER,
    name TEXT,
    start_date TEXT,
    end_date TEXT
);

CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY,
    task_key TEXT,
    name TEXT,
    type TEXT,
    status TEXT,
    estimation_points INTEGER,
    assignee_id TEXT,
    sprint_id INTEGER,
    parent_task_id INTEGER
);

CREATE TABLE IF NOT EXISTS pull_requests (
    id TEXT PRIMARY KEY,
    pr_number INTEGER,
    repo_full_name TEXT,
    url TEXT,
    title TEXT,
    body TEXT,
    state TEXT,
    merged BOOLEAN,
    author_id TEXT,
    github_author_login TEXT,
    github_author_email TEXT,
    merged_by_login TEXT,
    merged_by_email TEXT,
    additions INTEGER,
    deletions INTEGER,
    changed_files INTEGER,
    created_at TEXT,
    updated_at TEXT,
    merged_at TEXT,
    attribution_errors TEXT,
    last_github_fetch_updated_at TEXT
);

CREATE TABLE IF NOT EXISTS pr_github_etags (
    pr_id TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    etag TEXT NOT NULL,
    fetched_at TEXT,
    PRIMARY KEY (pr_id, endpoint)
);

CREATE TABLE IF NOT EXISTS task_pull_requests (
    task_id INTEGER,
    pr_id TEXT,
    UNIQUE(task_id, pr_id)
);

CREATE TABLE IF NOT EXISTS pr_commits (
    pr_id TEXT,
    sha TEXT,
    author_login TEXT,
    author_email TEXT,
    message TEXT,
    timestamp TEXT,
    additions INTEGER,
    deletions INTEGER
);

-- Pre-squash author capture (T-P1.4). When `/pulls/{n}/commits` returns the
-- per-commit history of a merged PR, we shadow it here so AUTHOR_MISMATCH
-- still works after a future force-push deletes the head ref. This table is
-- supplementary to pr_commits, never a replacement.
CREATE TABLE IF NOT EXISTS pr_pre_squash_authors (
    pr_id        TEXT NOT NULL,
    sha          TEXT NOT NULL,
    author_login TEXT,
    author_email TEXT,
    captured_at  TEXT,
    PRIMARY KEY (pr_id, sha)
);

CREATE TABLE IF NOT EXISTS pr_reviews (
    pr_id TEXT,
    reviewer_login TEXT,
    state TEXT,
    submitted_at TEXT
);

CREATE TABLE IF NOT EXISTS github_users (
    login TEXT PRIMARY KEY,
    name TEXT,
    email TEXT,
    student_id TEXT,
    fetched_at TEXT
);

CREATE TABLE IF NOT EXISTS fingerprints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path TEXT,
    repo_full_name TEXT,
    statement_index INTEGER,
    method_name TEXT,
    raw_fingerprint TEXT,
    normalized_fingerprint TEXT,
    method_fingerprint TEXT,
    blame_commit TEXT,
    blame_author_login TEXT,
    sprint_id INTEGER
);

CREATE TABLE IF NOT EXISTS pr_survival (
    pr_id TEXT,
    sprint_id INTEGER,
    statements_added_raw INTEGER,
    statements_surviving_raw INTEGER,
    statements_added_normalized INTEGER,
    statements_surviving_normalized INTEGER,
    methods_added INTEGER,
    methods_surviving INTEGER
);

CREATE TABLE IF NOT EXISTS pr_line_metrics (
    pr_id TEXT NOT NULL,
    sprint_id INTEGER NOT NULL,
    lat INTEGER,
    lar INTEGER,
    ls INTEGER,
    ld INTEGER,
    cosmetic_lines INTEGER,
    cosmetic_report TEXT,
    merge_sha TEXT,
    PRIMARY KEY (pr_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS student_sprint_survival (
    student_id TEXT,
    sprint_id INTEGER,
    total_stmts_raw INTEGER,
    surviving_stmts_raw INTEGER,
    survival_rate_raw REAL,
    total_stmts_normalized INTEGER,
    surviving_stmts_normalized INTEGER,
    survival_rate_normalized REAL,
    total_methods INTEGER,
    surviving_methods INTEGER,
    estimation_points_total INTEGER,
    estimation_density REAL
);

CREATE TABLE IF NOT EXISTS student_sprint_metrics (
    student_id TEXT,
    sprint_id INTEGER,
    points_delivered INTEGER,
    points_share REAL,
    weighted_pr_lines REAL,
    commit_count INTEGER,
    files_touched INTEGER,
    reviews_given INTEGER,
    temporal_spread TEXT, -- task-assignee-keyed JSON; for per-AUTHOR timing see student_sprint_temporal
    avg_doc_score REAL
);

CREATE TABLE IF NOT EXISTS flags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    student_id TEXT,
    sprint_id INTEGER,
    flag_type TEXT,
    severity TEXT,
    details TEXT
);

-- Per-student artifact-level flags (T-P3.4). Sibling to `flags` but
-- project-keyed instead of sprint-keyed. Flag detectors that classify
-- the *delivered code* (architecture / complexity / static analysis)
-- INSERT here; sprint-level *behavioural* detectors continue to use
-- the sprint-keyed `flags` table. The renderer reads this table for
-- the top-level "Code quality on main" section in REPORT.md.
-- Re-runs DELETE WHERE project_id = ? before re-populating, mirroring
-- the per-sprint `flags` idempotency idiom.
CREATE TABLE IF NOT EXISTS student_artifact_flags (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    student_id TEXT,
    project_id INTEGER,
    flag_type  TEXT,
    severity   TEXT,
    details    TEXT
);

CREATE TABLE IF NOT EXISTS pr_doc_evaluation (
    pr_id TEXT,
    sprint_id INTEGER,
    title_score REAL,
    description_score REAL,
    total_doc_score REAL,
    justification TEXT
);

CREATE TABLE IF NOT EXISTS cosmetic_rewrites (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sprint_id INTEGER,
    file_path TEXT,
    repo_full_name TEXT,
    original_author_id TEXT,
    rewriter_id TEXT,
    statements_affected INTEGER,
    change_type TEXT,
    details TEXT
);

CREATE TABLE IF NOT EXISTS cross_team_matches (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sprint_id INTEGER,
    team_a_project_id INTEGER,
    team_b_project_id INTEGER,
    file_path_a TEXT,
    file_path_b TEXT,
    method_name TEXT,
    fingerprint TEXT
);

CREATE TABLE IF NOT EXISTS team_sprint_inequality (
    project_id   INTEGER NOT NULL,
    sprint_id    INTEGER NOT NULL,
    metric_name  TEXT NOT NULL,
    gini         REAL,
    hoover       REAL,
    cv           REAL,
    max_min_ratio REAL,
    member_count INTEGER,
    PRIMARY KEY (project_id, sprint_id, metric_name)
);

CREATE TABLE IF NOT EXISTS student_sprint_contribution (
    student_id      TEXT NOT NULL,
    sprint_id       INTEGER NOT NULL,
    code_signal     REAL,
    review_signal   REAL,
    task_signal     REAL,
    process_signal  REAL,
    composite_score REAL,
    team_rank       INTEGER,
    z_score_from_mean REAL,
    PRIMARY KEY (student_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS student_trajectory (
    student_id       TEXT NOT NULL,
    project_id       INTEGER NOT NULL,
    trajectory_class TEXT NOT NULL,
    slope            REAL,
    r_squared        REAL,
    cv_across_sprints REAL,
    sprint_count     INTEGER,
    latest_sprint_id INTEGER,
    PRIMARY KEY (student_id)
);

CREATE TABLE IF NOT EXISTS method_metrics (
    file_path       TEXT NOT NULL,
    class_name      TEXT NOT NULL,
    method_name     TEXT NOT NULL,
    sprint_id       INTEGER NOT NULL,
    author_id       TEXT,
    loc             INTEGER,
    cyclomatic_complexity  INTEGER,
    cognitive_complexity   INTEGER,
    parameter_count        INTEGER,
    max_nesting_depth      INTEGER,
    return_count           INTEGER,
    halstead_volume        REAL,
    halstead_difficulty    REAL,
    halstead_effort        REAL,
    halstead_bugs          REAL,
    maintainability_index  REAL,
    -- T-CX: line range of the method declaration in the source file,
    -- 1-based, inclusive on both ends. NULL on rows produced before
    -- T-CX. Used by `crates/quality/src/testability.rs` to derive
    -- classic-axis findings (cyclomatic / cognitive / nesting / LOC /
    -- params) directly from the metrics cache without re-parsing
    -- source — and to anchor bad-line-weighted blame attribution
    -- without an AST round-trip.
    start_line      INTEGER,
    end_line        INTEGER,
    PRIMARY KEY (file_path, class_name, method_name, sprint_id)
);

CREATE TABLE IF NOT EXISTS satd_items (
    file_path   TEXT NOT NULL,
    line_number INTEGER NOT NULL,
    sprint_id   INTEGER NOT NULL,
    author_id   TEXT,
    category    TEXT NOT NULL,
    keyword     TEXT NOT NULL,
    comment_text TEXT NOT NULL,
    PRIMARY KEY (file_path, line_number, sprint_id)
);

CREATE TABLE IF NOT EXISTS student_sprint_quality (
    student_id              TEXT NOT NULL,
    sprint_id               INTEGER NOT NULL,
    avg_cc                  REAL,
    avg_cognitive_complexity REAL,
    avg_method_loc          REAL,
    pct_methods_cc_over_10  REAL,
    avg_maintainability     REAL,
    satd_count              INTEGER,
    satd_introduced         INTEGER,
    satd_removed            INTEGER,
    test_file_loc           INTEGER,
    test_to_code_ratio      REAL,
    delta_avg_cc            REAL,
    delta_avg_cognitive     REAL,
    delta_pct_cc_over_10    REAL,
    delta_maintainability   REAL,
    PRIMARY KEY (student_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS student_style_profile (
    student_id              TEXT NOT NULL,
    sprint_id               INTEGER NOT NULL,
    avg_identifier_length   REAL,
    camelcase_ratio         REAL,
    abbreviation_ratio      REAL,
    single_char_var_ratio   REAL,
    comment_density         REAL,
    avg_comment_length      REAL,
    inline_comment_ratio    REAL,
    avg_method_length       REAL,
    method_length_stddev    REAL,
    avg_parameter_count     REAL,
    blank_line_ratio        REAL,
    avg_catch_block_length  REAL,
    empty_catch_ratio       REAL,
    wildcard_import_ratio   REAL,
    avg_import_count        REAL,
    PRIMARY KEY (student_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS pr_behavioral_signals (
    pr_id               TEXT NOT NULL,
    student_id          TEXT NOT NULL,
    sprint_id           INTEGER NOT NULL,
    commit_count        INTEGER,
    single_commit_pr    BOOLEAN,
    max_lines_per_commit INTEGER,
    avg_minutes_between_commits REAL,
    has_fixup_pattern   BOOLEAN,
    lines_per_minute    REAL,
    productivity_anomaly BOOLEAN,
    has_test_adjustments BOOLEAN,
    has_intermediate_changes BOOLEAN,
    has_branch_merges   BOOLEAN,
    generic_message_ratio REAL,
    avg_message_length  REAL,
    PRIMARY KEY (pr_id)
);

CREATE TABLE IF NOT EXISTS pr_ai_probability (
    pr_id               TEXT NOT NULL,
    student_id          TEXT NOT NULL,
    sprint_id           INTEGER NOT NULL,
    stylometric_score   REAL,
    behavioral_score    REAL,
    coherence_score     REAL,
    heuristic_score     REAL,
    ai_probability      REAL,
    confidence          TEXT,
    risk_level          TEXT,
    top_signals         TEXT,
    PRIMARY KEY (pr_id)
);

CREATE TABLE IF NOT EXISTS sprint_planning_quality (
    project_id          INTEGER NOT NULL,
    sprint_id           INTEGER NOT NULL,
    planned_points      REAL,
    completed_points    REAL,
    commitment_reliability REAL,
    velocity            REAL,
    velocity_cv         REAL,
    sprint_accuracy_error REAL,
    unestimated_task_pct REAL,
    PRIMARY KEY (project_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS pr_workflow_metrics (
    pr_id               TEXT NOT NULL,
    student_id          TEXT,
    sprint_id           INTEGER,
    total_lines         INTEGER,
    size_category       TEXT,
    time_to_first_review_hours REAL,
    time_to_merge_hours REAL,
    review_rounds       INTEGER,
    self_merged         BOOLEAN,
    has_linked_task     BOOLEAN,
    has_description     BOOLEAN,
    reviewers_count     INTEGER,
    PRIMARY KEY (pr_id)
);

CREATE TABLE IF NOT EXISTS student_sprint_temporal (
    student_id      TEXT NOT NULL,
    sprint_id       INTEGER NOT NULL,
    commit_entropy  REAL,
    active_days     INTEGER,
    active_days_ratio REAL,
    cramming_ratio  REAL,
    weekend_ratio   REAL,
    night_ratio     REAL,
    longest_gap_days REAL,
    is_cramming     BOOLEAN,
    is_steady       BOOLEAN,
    PRIMARY KEY (student_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS team_sprint_collaboration (
    project_id      INTEGER NOT NULL,
    sprint_id       INTEGER NOT NULL,
    network_density REAL,
    reciprocity     REAL,
    centrality_json TEXT,
    has_isolated_member BOOLEAN,
    review_coverage REAL,
    PRIMARY KEY (project_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS pr_compilation (
    pr_id               TEXT NOT NULL,
    repo_name           TEXT NOT NULL,
    sprint_id           INTEGER NOT NULL,
    author_id           TEXT,
    reviewer_ids        TEXT,
    pr_number           INTEGER,
    merge_sha           TEXT,
    compiles            BOOLEAN NOT NULL,
    exit_code           INTEGER NOT NULL,
    stdout_text         TEXT,
    stderr_text         TEXT,
    duration_seconds    REAL,
    build_command       TEXT,
    working_dir         TEXT,
    timed_out           BOOLEAN DEFAULT FALSE,
    tested_at           TEXT NOT NULL,
    PRIMARY KEY (pr_id, repo_name)
);

CREATE TABLE IF NOT EXISTS compilation_failure_summary (
    sprint_id           INTEGER NOT NULL,
    project_id          INTEGER NOT NULL,
    total_prs           INTEGER,
    compiling_prs       INTEGER,
    failing_prs         INTEGER,
    compile_rate        REAL,
    author_breakdown    TEXT,
    reviewer_breakdown  TEXT,
    top_errors          TEXT,
    PRIMARY KEY (sprint_id, project_id)
);

CREATE TABLE IF NOT EXISTS pr_regularity (
    pr_id               TEXT NOT NULL,
    sprint_id           INTEGER NOT NULL,
    student_id          TEXT,
    merged_at           TEXT,
    sprint_end          TEXT,
    hours_before_deadline REAL,
    regularity_score    REAL NOT NULL,
    regularity_band     TEXT NOT NULL,
    PRIMARY KEY (pr_id)
);

CREATE TABLE IF NOT EXISTS student_sprint_regularity (
    student_id          TEXT NOT NULL,
    sprint_id           INTEGER NOT NULL,
    avg_regularity      REAL,
    min_regularity      REAL,
    pr_count            INTEGER,
    prs_in_last_24h     INTEGER,
    prs_in_last_3h      INTEGER,
    regularity_band     TEXT,
    PRIMARY KEY (student_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS curriculum_concepts (
    concept_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    category        TEXT NOT NULL,
    value           TEXT NOT NULL,
    source_file     TEXT,
    sprint_taught   INTEGER,
    UNIQUE(category, value)
);

-- Per-sprint frozen view of `curriculum_concepts` (T-P2.5). Once a sprint
-- ends, instructors freeze the curriculum-as-taught into this snapshot so
-- editing a future sprint's slide deck cannot silently re-grade past sprints.
-- Rows for a given `sprint_id` are written once and treated as immutable;
-- `freeze_curriculum_for_sprint` is a no-op on subsequent calls.
CREATE TABLE IF NOT EXISTS curriculum_concepts_snapshot (
    sprint_id     INTEGER NOT NULL,
    category      TEXT NOT NULL,
    value         TEXT NOT NULL,
    source_file   TEXT,
    sprint_taught INTEGER,
    PRIMARY KEY (sprint_id, category, value)
);

CREATE TABLE IF NOT EXISTS curriculum_violations (
    file_path       TEXT NOT NULL,
    repo_name       TEXT NOT NULL,
    project_id      INTEGER NOT NULL,
    sprint_id       INTEGER NOT NULL,
    violation_type  TEXT NOT NULL,
    value           TEXT NOT NULL,
    line_number     INTEGER,
    severity        TEXT NOT NULL,
    author_id       TEXT,
    commit_sha      TEXT,
    PRIMARY KEY (file_path, repo_name, sprint_id, violation_type, value)
);

CREATE TABLE IF NOT EXISTS file_style_features (
    file_path               TEXT NOT NULL,
    repo_name               TEXT NOT NULL,
    sprint_id               INTEGER NOT NULL,
    avg_identifier_length   REAL,
    identifier_length_stddev REAL,
    camelcase_ratio         REAL,
    screaming_snake_ratio   REAL,
    single_char_var_ratio   REAL,
    max_identifier_length   INTEGER,
    comment_density         REAL,
    avg_comment_length_chars REAL,
    inline_vs_block_ratio   REAL,
    javadoc_ratio           REAL,
    comment_to_code_ratio   REAL,
    avg_method_length       REAL,
    method_length_stddev    REAL,
    avg_parameter_count     REAL,
    avg_nesting_depth       REAL,
    max_nesting_depth       INTEGER,
    try_catch_density       REAL,
    empty_catch_ratio       REAL,
    avg_catch_body_lines    REAL,
    import_count            INTEGER,
    wildcard_import_ratio   REAL,
    import_alphabetized     BOOLEAN,
    blank_line_ratio        REAL,
    has_comprehensive_javadoc BOOLEAN,
    has_null_checks_everywhere BOOLEAN,
    uniform_formatting      BOOLEAN,
    PRIMARY KEY (file_path, repo_name, sprint_id)
);

CREATE TABLE IF NOT EXISTS student_style_baseline (
    student_id              TEXT NOT NULL,
    project_id              INTEGER NOT NULL,
    avg_identifier_length   REAL,
    identifier_length_stddev REAL,
    camelcase_ratio         REAL,
    comment_density         REAL,
    avg_method_length       REAL,
    method_length_stddev    REAL,
    avg_nesting_depth       REAL,
    try_catch_density       REAL,
    import_alphabetized_ratio REAL,
    feature_means           TEXT,
    feature_stds            TEXT,
    sample_file_count       INTEGER,
    PRIMARY KEY (student_id, project_id)
);

CREATE TABLE IF NOT EXISTS file_perplexity (
    file_path           TEXT NOT NULL,
    repo_name           TEXT NOT NULL,
    sprint_id           INTEGER NOT NULL,
    overall_perplexity  REAL,
    line_perplexity_std REAL,
    burstiness_score    REAL,
    min_line_perplexity REAL,
    max_line_perplexity REAL,
    ai_perplexity_score REAL,
    line_count          INTEGER,
    token_count         INTEGER,
    processing_seconds  REAL,
    PRIMARY KEY (file_path, repo_name, sprint_id)
);

CREATE TABLE IF NOT EXISTS llm_ai_assessment (
    file_path       TEXT NOT NULL,
    repo_name       TEXT NOT NULL,
    project_id      INTEGER NOT NULL,
    sprint_id       INTEGER NOT NULL,
    ai_probability  REAL,
    confidence      TEXT,
    reasoning       TEXT,
    evidence_tags   TEXT,
    session_id      TEXT,
    tokens_used     INTEGER,
    PRIMARY KEY (file_path, repo_name, sprint_id)
);

CREATE TABLE IF NOT EXISTS student_text_profile (
    student_id          TEXT NOT NULL,
    project_id          INTEGER NOT NULL,
    total_word_count    INTEGER,
    avg_sentence_length REAL,
    avg_word_length     REAL,
    vocabulary_richness REAL,
    formality_score     REAL,
    error_rate          REAL,
    uses_contractions   BOOLEAN,
    uses_emoji          BOOLEAN,
    uses_abbreviations  BOOLEAN,
    avg_pr_description_length REAL,
    PRIMARY KEY (student_id, project_id)
);

CREATE TABLE IF NOT EXISTS text_consistency_scores (
    student_id          TEXT NOT NULL,
    sprint_id           INTEGER NOT NULL,
    sprint_formality    REAL,
    sprint_avg_sentence_length REAL,
    sprint_vocabulary_richness REAL,
    sprint_avg_pr_desc_length REAL,
    formality_deviation REAL,
    vocabulary_deviation REAL,
    sentence_length_deviation REAL,
    text_consistency_score REAL,
    PRIMARY KEY (student_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS file_ai_probability (
    file_path           TEXT NOT NULL,
    repo_name           TEXT NOT NULL,
    project_id          INTEGER NOT NULL,
    sprint_id           INTEGER NOT NULL,
    curriculum_score    REAL,
    stylometry_score    REAL,
    perplexity_score    REAL,
    llm_judge_score     REAL,
    text_consistency_score REAL,
    behavioral_score    REAL,
    ai_probability      REAL NOT NULL,
    confidence          TEXT NOT NULL,
    risk_level          TEXT NOT NULL,
    top_signals         TEXT,
    PRIMARY KEY (file_path, repo_name, sprint_id)
);

CREATE TABLE IF NOT EXISTS task_description_evaluation (
    task_id     INTEGER NOT NULL,
    sprint_id   INTEGER NOT NULL,
    quality_score REAL,
    justification TEXT,
    PRIMARY KEY (task_id, sprint_id)
);

CREATE TABLE IF NOT EXISTS code_practices_evaluation (
    project_id  INTEGER NOT NULL,
    sprint_id   INTEGER NOT NULL,
    repo_type   TEXT NOT NULL,
    evaluation  TEXT,
    PRIMARY KEY (project_id, sprint_id, repo_type)
);

-- Peer-group analysis (project-scoped). Each row is a similarity bucket of
-- DONE TASK/BUG rows that share (stack, layer, action) across every sprint
-- in the project, where layer is derived from the *file paths* of the
-- linked PRs' surviving fingerprints (not from task-name keyword scans).
-- Rebuilt from scratch on each pipeline run; idempotency = DELETE WHERE
-- project_id = ? before INSERT.
CREATE TABLE IF NOT EXISTS task_similarity_groups (
    group_id                INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id              INTEGER NOT NULL,
    representative_task_id  INTEGER NOT NULL,
    group_label             TEXT,
    stack                   TEXT,
    layer                   TEXT,
    action                  TEXT,
    member_count            INTEGER,
    median_points           REAL,
    median_ls               REAL,
    median_ls_per_point     REAL,
    median_stmts_per_point  REAL,
    FOREIGN KEY (project_id)             REFERENCES projects(id),
    FOREIGN KEY (representative_task_id) REFERENCES tasks(id)
);

CREATE TABLE IF NOT EXISTS task_group_members (
    group_id                      INTEGER NOT NULL,
    task_id                       INTEGER NOT NULL,
    is_outlier                    BOOLEAN DEFAULT 0,
    outlier_reason                TEXT,
    -- (value − group median) / group MAD on stmts_normalized / points.
    -- Sole driver of `is_outlier`; |z| > mad_k_threshold → outlier.
    stmts_per_point_deviation     REAL,
    PRIMARY KEY (group_id, task_id),
    FOREIGN KEY (group_id)  REFERENCES task_similarity_groups(group_id),
    FOREIGN KEY (task_id)   REFERENCES tasks(id)
);

CREATE TABLE IF NOT EXISTS pr_submission_tiers (
    sprint_id             INTEGER NOT NULL,
    pr_id                 TEXT NOT NULL,
    merged_at             TEXT,
    hours_before_deadline REAL,
    tier                  TEXT NOT NULL,
    pr_kind               TEXT,
    PRIMARY KEY (sprint_id, pr_id),
    FOREIGN KEY (sprint_id) REFERENCES sprints(id),
    FOREIGN KEY (pr_id)     REFERENCES pull_requests(id)
);

CREATE INDEX IF NOT EXISTS idx_pull_requests_merged_at
    ON pull_requests(merged, merged_at);
CREATE INDEX IF NOT EXISTS idx_tasks_sprint_assignee
    ON tasks(sprint_id, assignee_id);
CREATE INDEX IF NOT EXISTS idx_pr_commits_pr_id
    ON pr_commits(pr_id);
CREATE INDEX IF NOT EXISTS idx_task_pull_requests_pr_id
    ON task_pull_requests(pr_id);
CREATE INDEX IF NOT EXISTS idx_pr_line_metrics_merge_sha
    ON pr_line_metrics(pr_id, merge_sha);

-- One row per `run_pipeline` invocation (T-P2.6). When `[grading]
-- hidden_thresholds = true` the threshold values used by detectors are
-- jittered ±jitter_pct from their published defaults, seeded by
-- (today, course_id) so the same `--today` reproduces. `thresholds_json`
-- is the realised value map for forensic comparison; reports show the
-- published threshold + a `±N%` notation, never the realised value.
CREATE TABLE IF NOT EXISTS pipeline_run (
    run_id          TEXT PRIMARY KEY,
    today           TEXT NOT NULL,
    course_id       INTEGER NOT NULL,
    jitter_pct      REAL,
    seed            INTEGER NOT NULL,
    thresholds_json TEXT,
    created_at      TEXT NOT NULL
);

-- Architecture conformance violations (T-P2.2 / T-P3.1 / T-P3.3 / T-P3.4).
-- Artifact-level rows: one per (repo, file, rule, offending_import,
-- start_line) describing a violation that survives in the team's main
-- branch. Sprint-free by design — we grade the code as delivered, not
-- the per-sprint trajectory. `introduced_sprint_id` records the
-- earliest sprint window that contains the minimum author-date among
-- the lines blamed for [start_line..=end_line]; NULL when no window
-- matches (e.g. commits before the course started). The T-P2.2
-- `violation_kind` column has been dropped — its value was always the
-- same as `rule_kind` (every writer wrote both columns with the same
-- string); `apply_additive_migrations` backfills `rule_kind` from the
-- legacy `violation_kind` for any pre-T-P3.1 row that still has NULL
-- before issuing `ALTER TABLE ... DROP COLUMN`.
CREATE TABLE IF NOT EXISTS architecture_violations (
    repo_full_name       TEXT NOT NULL,
    file_path            TEXT NOT NULL,
    rule_name            TEXT NOT NULL,
    offending_import     TEXT NOT NULL,
    severity             TEXT NOT NULL,
    -- 1-based inclusive line range. Part of the PK so the same rule
    -- firing on multiple ranges within one file produces distinct rows
    -- (no more `@L<n>` disambiguator suffix on `offending_import`).
    -- SQLite tolerates NULL in PK columns and treats NULLs as distinct,
    -- which preserves backwards compatibility with package-glob rows
    -- whose line was historically NULL on some inputs.
    start_line           INTEGER,
    end_line             INTEGER,
    -- "layer_dependency" / "forbidden_import" / "ast_*" / "llm".
    rule_kind            TEXT,
    -- T-P3.3: hash of the rubric/rule body that produced this row, so
    -- a rubric edit invalidates cached LLM judgements. NULL on
    -- AST/glob rows.
    rule_version         TEXT,
    -- Free-form explanation; primarily populated for LLM-judged rows.
    explanation          TEXT,
    -- T-P3.4: blame-derived earliest containing sprint window. NULL
    -- when no sprint window contains the minimum author-date among
    -- the offending lines.
    introduced_sprint_id INTEGER,
    PRIMARY KEY (repo_full_name, file_path, rule_name, offending_import, start_line)
);

-- Per-student attribution of `architecture_violations` rows (T-P3.1 / T-P3.4).
-- Computed by running `git blame -w --ignore-revs-file` over the violation's
-- (file, start_line..end_line) and tallying lines per student. `weight` is
-- `lines_authored / total_lines` in [0, 1] so the per-student WARNING
-- magnitude scales with how much of the offending code each student actually
-- wrote — a 1-line typo fix on a 30-line bad method gets ~3 % weight.
-- The join key is the parent row's implicit `rowid`; pre-existing
-- attribution for a given (repo) is deleted when the architecture scan
-- re-runs, mirroring the violation-table idempotency idiom. Sprint-free
-- by design (artifact-level): use `architecture_violations.introduced_sprint_id`
-- on the parent row when sprint provenance is needed for rendering.
CREATE TABLE IF NOT EXISTS architecture_violation_attribution (
    violation_rowid INTEGER NOT NULL,
    student_id      TEXT NOT NULL,
    lines_authored  INTEGER NOT NULL,
    total_lines     INTEGER NOT NULL,
    weight          REAL NOT NULL,
    PRIMARY KEY (violation_rowid, student_id)
);

-- LLM-judged architecture cache (T-P3.3). Keyed by `(file_sha,
-- rubric_version, model_id)` so the cache invalidates when the file
-- content changes, when the rubric edits change the version field or
-- the body hash, or when the model id changes. The cache stores the
-- model's raw `response_json` (already schema-validated at insert
-- time) so re-runs reproduce byte-identical `architecture_violations`
-- rows from the cached response without needing to re-call the API.
-- `evaluated_at` is ISO-8601 UTC; useful for forensic comparison and
-- the optional `architecture-rubric --show-cache-stats` subcommand.
CREATE TABLE IF NOT EXISTS architecture_llm_cache (
    file_sha       TEXT NOT NULL,
    rubric_version TEXT NOT NULL,
    model_id       TEXT NOT NULL,
    response_json  TEXT NOT NULL,
    evaluated_at   TEXT NOT NULL,
    PRIMARY KEY (file_sha, rubric_version, model_id)
);

-- Per-repo outcome row for the architecture scan (T-P3.4). Mirrors
-- `static_analysis_runs` and `method_complexity_runs` shape but
-- artifact-level (no sprint_id). `head_sha` lets `run-all` skip
-- re-scanning when the working tree hasn't moved since the last
-- successful run. `findings_count` is the number of
-- `architecture_violations` rows produced for this repo. Re-runs
-- INSERT OR REPLACE keyed on `repo_full_name`.
CREATE TABLE IF NOT EXISTS architecture_runs (
    repo_full_name TEXT    NOT NULL,
    status         TEXT    NOT NULL,           -- OK | SKIPPED_HEAD_UNCHANGED | SKIPPED_NO_SOURCES | CRASHED
    findings_count INTEGER NOT NULL DEFAULT 0,
    duration_ms    INTEGER,
    head_sha       TEXT,
    diagnostics    TEXT,
    ran_at         TEXT    NOT NULL,           -- ISO-8601 UTC
    PRIMARY KEY (repo_full_name)
);

-- Per-team ownership snapshot (T-P2.3). `truck_factor` is the smallest k
-- such that the top-k authors jointly own >=95% of statements attributed in
-- the project's fingerprints for this sprint. `owners_csv` lists those k
-- student_ids in descending share order. Both columns are NULL when the
-- project has no fingerprints yet (compile/survival did not produce data).
CREATE TABLE IF NOT EXISTS team_sprint_ownership (
    project_id   INTEGER NOT NULL,
    sprint_id    INTEGER NOT NULL,
    truck_factor INTEGER,
    owners_csv   TEXT,
    PRIMARY KEY (project_id, sprint_id)
);

-- Per-PR mutation-testing summary (T-P2.4). Populated by the
-- `compile_stage` builder when the matching `BuildProfile` sets
-- `mutation_command` (typically `./gradlew pitest --info` for the
-- Pitest Gradle plugin in `scmMutationCoverage` mode). One row per
-- (PR, repo); subsequent runs `INSERT OR REPLACE`. `mutation_score`
-- is `(killed + timed_out) / (total − non_viable)` in `[0, 1]` —
-- non-viable mutants don't compile so they're excluded from the
-- denominator. `duration_seconds` measures the mutation run only,
-- not the primary build.
CREATE TABLE IF NOT EXISTS pr_mutation (
    pr_id            TEXT NOT NULL,
    repo_name        TEXT NOT NULL,
    sprint_id        INTEGER,
    mutants_total    INTEGER,
    mutants_killed   INTEGER,
    mutation_score   REAL,
    duration_seconds REAL,
    PRIMARY KEY (pr_id, repo_name)
);

CREATE TABLE IF NOT EXISTS student_sprint_ai_usage (
    student_id              TEXT NOT NULL,
    sprint_id               INTEGER NOT NULL,
    project_id              INTEGER NOT NULL,
    total_authored_lines    INTEGER,
    ai_flagged_lines        INTEGER,
    ai_usage_ratio          REAL,
    weighted_ai_score       REAL,
    avg_curriculum_score    REAL,
    avg_stylometry_score    REAL,
    avg_perplexity_score    REAL,
    avg_llm_judge_score     REAL,
    text_consistency_score  REAL,
    avg_behavioral_score    REAL,
    risk_level              TEXT,
    confidence              TEXT,
    file_count_analyzed     INTEGER,
    file_count_flagged      INTEGER,
    PRIMARY KEY (student_id, sprint_id)
);

-- Java static-analysis findings (T-SA / phase 2). One row per
-- (repo, sprint, analyzer, rule, file, location). The natural
-- PK would be very wide (long rule ids, long messages), so we use a
-- surrogate `id` for the FK from `_attribution`, and dedup
-- across re-runs via `UNIQUE (repo_full_name, sprint_id, fingerprint)`.
-- `fingerprint` = sha1(analyzer|rule|file|start_line|message[..120]) — see
-- `static_analysis::adapter::Finding::compute_fingerprint`.
-- `head_sha` is the repo HEAD at scan time; `diff-db` uses it as a
-- reproducibility anchor when comparing runs.
CREATE TABLE IF NOT EXISTS static_analysis_findings (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_full_name       TEXT    NOT NULL,
    analyzer             TEXT    NOT NULL,        -- 'pmd' | 'checkstyle' | 'spotbugs'
    analyzer_version     TEXT,
    rule_id              TEXT    NOT NULL,
    category             TEXT,                    -- 'style' | 'bug' | 'security' | ...
    severity             TEXT    NOT NULL,        -- 'CRITICAL' | 'WARNING' | 'INFO'
    file_path            TEXT    NOT NULL,
    start_line           INTEGER,
    end_line             INTEGER,
    message              TEXT    NOT NULL,
    help_uri             TEXT,
    fingerprint          TEXT    NOT NULL,
    head_sha             TEXT,
    -- T-P3.4: blame-derived earliest containing sprint window. NULL
    -- when no sprint window contains the date.
    introduced_sprint_id INTEGER,
    UNIQUE (repo_full_name, fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_sa_findings_repo
    ON static_analysis_findings(repo_full_name);

-- Per-student blame attribution for `static_analysis_findings`
-- (T-SA / T-P3.4 — sprint-free, artifact-shape). weight =
-- lines_authored / total_lines computed via `git blame -w
-- --ignore-revs-file` over the finding's [start_line..=end_line], so a
-- 1-line typo fix in a 30-line offending block carries ~3% weight, not
-- 50%. Sprint provenance is on the parent finding's
-- introduced_sprint_id.
CREATE TABLE IF NOT EXISTS static_analysis_finding_attribution (
    finding_id     INTEGER NOT NULL,
    student_id     TEXT    NOT NULL,
    lines_authored INTEGER NOT NULL,
    total_lines    INTEGER NOT NULL,
    weight         REAL    NOT NULL,         -- in [0, 1]
    PRIMARY KEY (finding_id, student_id),
    FOREIGN KEY (finding_id) REFERENCES static_analysis_findings(id) ON DELETE CASCADE
);

-- Per-method complexity / testability findings (T-CX / T-P3.4). One row
-- per (method, rule_key) where the rule fired. Source: AST scan of the
-- team repos performed by `crates/quality/src/testability.rs`. Rules
-- are split in two families: classic complexity axes (cyclomatic,
-- cognitive, nesting, long-method, wide-signature) and targeted
-- testability rules (broad-catch, non-deterministic-call,
-- inline-collaborator, static-singleton, reflection). Only
-- `src/main/java/**` and `app/src/main/java/**` are scanned; tests and
-- generated sources are skipped at discovery time. Sprint-free
-- artifact-shape (T-P3.4): grading the code as delivered on main. The
-- `introduced_sprint_id` column records the earliest sprint window
-- containing any blamed commit's author-date for the offending range;
-- NULL when no sprint window matches. Idempotency: rows for the repo
-- are deleted before re-population.
CREATE TABLE IF NOT EXISTS method_complexity_findings (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id           INTEGER NOT NULL,
    repo_full_name       TEXT    NOT NULL,
    file_path            TEXT    NOT NULL,
    class_name           TEXT,
    method_name          TEXT    NOT NULL,
    start_line           INTEGER NOT NULL,
    end_line             INTEGER NOT NULL,
    rule_key             TEXT    NOT NULL,
    severity             TEXT    NOT NULL,           -- CRITICAL | WARNING | INFO
    measured_value       REAL,
    threshold            REAL,
    detail               TEXT,
    introduced_sprint_id INTEGER
);

CREATE INDEX IF NOT EXISTS idx_mcf_repo
    ON method_complexity_findings(repo_full_name);
CREATE INDEX IF NOT EXISTS idx_mcf_project
    ON method_complexity_findings(project_id);

-- Per-student bad-line-weighted attribution for `method_complexity_findings`
-- (T-CX / T-P3.4). `weighted_lines` is the raw badness sum (lines
-- inside the offending construct count 3x, control-flow lines count
-- 2x, plain method lines count 1x); `weight` is
-- `weighted_lines / total_weighted_lines`, summing to 1 across the
-- method's authors. `lines_attributed` is the raw line count (no
-- weighting) for transparency. Sprint-free: artifact-shape parent
-- carries the introduced_sprint_id for rendering provenance.
CREATE TABLE IF NOT EXISTS method_complexity_attribution (
    finding_id        INTEGER NOT NULL,
    student_id        TEXT    NOT NULL,
    lines_attributed  INTEGER NOT NULL,
    weighted_lines    REAL    NOT NULL,
    weight            REAL    NOT NULL,         -- in [0, 1]
    PRIMARY KEY (finding_id, student_id),
    FOREIGN KEY (finding_id) REFERENCES method_complexity_findings(id) ON DELETE CASCADE
);

-- Per-repo outcome row for the testability scan (T-CX / T-P3.4). Same
-- shape as `architecture_runs` — artifact-level (no sprint_id).
-- `head_sha` lets `run-all` skip re-scanning when the working tree
-- hasn't moved since the last successful run. Re-runs `INSERT OR
-- REPLACE` keyed on `repo_full_name`.
CREATE TABLE IF NOT EXISTS method_complexity_runs (
    repo_full_name TEXT    NOT NULL,
    status         TEXT    NOT NULL,           -- OK | SKIPPED_NO_SOURCES | SKIPPED_HEAD_UNCHANGED | CRASHED
    findings_count INTEGER NOT NULL DEFAULT 0,
    duration_ms    INTEGER,
    head_sha       TEXT,
    diagnostics    TEXT,
    ran_at         TEXT    NOT NULL,           -- ISO-8601 UTC
    PRIMARY KEY (repo_full_name)
);

-- Per-(analyzer, repo) outcome row so the report can render
-- "spotbugs: skipped — compile failed" honestly instead of a silent
-- absence, and so re-runs can decide whether to skip cheaply
-- (T-SA / T-P3.4: artifact-level, sprint-free).
CREATE TABLE IF NOT EXISTS static_analysis_runs (
    repo_full_name TEXT    NOT NULL,
    analyzer       TEXT    NOT NULL,
    status         TEXT    NOT NULL,         -- 'OK' | 'SKIPPED_NO_CLASSES' | 'CRASHED' | 'TIMED_OUT'
    findings_count INTEGER NOT NULL DEFAULT 0,
    duration_ms    INTEGER,
    head_sha       TEXT,
    diagnostics    TEXT,
    ran_at         TEXT    NOT NULL,         -- ISO-8601 UTC
    PRIMARY KEY (repo_full_name, analyzer)
);

-- Computed mapping from a github identity (login or commit email) to the
-- TrackDev student it most likely belongs to. Built by
-- collect::identity_resolver from per-PR task-assignee evidence:
-- for each PR p, each linked-task assignee s receives weight count(s in tasks(p))/total_tasks(p),
-- multiplied by source_weight (commits=1.0, pre_squash=1.0, pr_submitter=0.5),
-- accumulated across all PRs of every sprint. The mapping is accepted iff
-- confidence = acc[(s*,i)] / sum_s(acc[(s,i)]) >= 0.7. Rejected identities
-- are recorded in identity_resolution_warnings instead.
CREATE TABLE IF NOT EXISTS student_github_identity (
    student_id     TEXT NOT NULL,
    identity_kind  TEXT NOT NULL CHECK(identity_kind IN ('login','email')),
    identity_value TEXT NOT NULL,           -- lowercased
    weight         REAL NOT NULL,           -- accumulated evidence on (student, identity)
    confidence     REAL NOT NULL,           -- accepted-share at convergence
    first_seen_pr  TEXT,
    last_seen_pr   TEXT,
    PRIMARY KEY (student_id, identity_kind, identity_value)
);

CREATE INDEX IF NOT EXISTS idx_student_github_identity_value
    ON student_github_identity(identity_kind, identity_value);

-- Per-identity warnings for ambiguous resolution. One row per (kind, value)
-- whose top-candidate confidence stayed below the threshold.
CREATE TABLE IF NOT EXISTS identity_resolution_warnings (
    identity_kind  TEXT NOT NULL CHECK(identity_kind IN ('login','email')),
    identity_value TEXT NOT NULL,
    kind           TEXT NOT NULL,           -- e.g. 'AMBIGUOUS_IDENTITY'
    candidates     TEXT NOT NULL,           -- JSON: [{student_id, weight, share}, …]
    observed_at    TEXT NOT NULL,
    PRIMARY KEY (identity_kind, identity_value, kind)
);

-- Canonical TrackDev-scoped author set for every PR. Derives the SET of
-- student authors of a PR from task_pull_requests → tasks.assignee_id, NOT
-- from pull_requests.author_id (the latter is a git/github identity, used
-- only for blame and review-graph attribution). One row per (pr, assignee)
-- with the assignee's accumulated points and task count over that PR.
-- Status-agnostic: callers that want only DONE-task PRs filter at the
-- call site (e.g. `JOIN tasks t ON t.assignee_id = pa.student_id WHERE
-- t.status = 'DONE'`). USER_STORY parents are excluded — they aren't
-- gradeable units.
CREATE VIEW IF NOT EXISTS pr_authors AS
SELECT pr.id                                       AS pr_id,
       t.assignee_id                               AS student_id,
       SUM(COALESCE(t.estimation_points, 0))       AS author_points,
       COUNT(*)                                    AS author_task_count
FROM pull_requests pr
JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
JOIN tasks t ON t.id = tpr.task_id
WHERE t.type != 'USER_STORY' AND t.assignee_id IS NOT NULL
GROUP BY pr.id, t.assignee_id;
