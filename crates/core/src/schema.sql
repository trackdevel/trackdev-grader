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

CREATE TABLE IF NOT EXISTS pr_doc_evaluation (
    pr_id TEXT,
    sprint_id INTEGER,
    title_score INTEGER,
    description_score INTEGER,
    total_doc_score INTEGER,
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

CREATE TABLE IF NOT EXISTS task_similarity_groups (
    group_id                INTEGER PRIMARY KEY AUTOINCREMENT,
    sprint_id               INTEGER NOT NULL,
    project_id              INTEGER,
    representative_task_id  INTEGER NOT NULL,
    group_label             TEXT,
    stack                   TEXT,
    layer                   TEXT,
    action                  TEXT,
    member_count            INTEGER,
    median_points           REAL,
    median_lar              REAL,
    median_ls               REAL,
    median_ls_per_point     REAL,
    FOREIGN KEY (sprint_id)              REFERENCES sprints(id),
    FOREIGN KEY (representative_task_id) REFERENCES tasks(id)
);

CREATE TABLE IF NOT EXISTS task_group_members (
    group_id                      INTEGER NOT NULL,
    task_id                       INTEGER NOT NULL,
    sprint_id                     INTEGER NOT NULL,
    is_outlier                    BOOLEAN DEFAULT 0,
    outlier_reason                TEXT,
    points_deviation              REAL,
    lar_deviation                 REAL,
    ls_deviation                  REAL,
    ls_per_point_deviation        REAL,
    PRIMARY KEY (group_id, task_id),
    FOREIGN KEY (group_id)  REFERENCES task_similarity_groups(group_id),
    FOREIGN KEY (task_id)   REFERENCES tasks(id),
    FOREIGN KEY (sprint_id) REFERENCES sprints(id)
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
