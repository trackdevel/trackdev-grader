//! Synthetic DB seeds for reference fixture generation.

use rusqlite::{params, Connection};

pub fn seed_all_fixtures(conn: &Connection) {
    seed_rich_example(conn);
    seed_one_absent_axis(conn);
    seed_zero_delivery(conn);
    seed_security_flags(conn);
}

fn seed_rich_example(conn: &Connection) {
    let project_id = 1i64;
    let sprint_id = 10i64;
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, 'team-01', 'Team 01')",
        params![project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
        params![sprint_id, project_id],
    )
    .unwrap();
    for (id, name) in [("alice", "Alice"), ("bob", "Bob")] {
        conn.execute(
            "INSERT INTO students (id, username, github_login, full_name, team_project_id)
             VALUES (?, ?, ?, ?, ?)",
            params![id, id, id, name, project_id],
        )
        .unwrap();
    }
    let task = |id: i64, key: &str, pts: i64, who: &str| {
        conn.execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
             VALUES (?, ?, ?, 'TASK', 'DONE', ?, ?, ?)",
            params![id, key, key, pts, who, sprint_id],
        )
        .unwrap();
    };
    let ai = |task_id: i64, model: &str, level: Option<&str>| {
        conn.execute(
            "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
             VALUES (?, ?, ?, 1, '2026-01-01')",
            params![task_id, model, level],
        )
        .unwrap();
    };
    task(1, "T-1", 10, "alice");
    ai(1, "Cap", Some("A"));
    task(2, "T-2", 10, "bob");
    ai(2, "GPT-5.5", Some("E"));
    task(3, "T-3", 5, "alice");
    ai(3, "Cursor", None);

    conn.execute(
        "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
         VALUES ('pr1', 1, 'spring-api', 'http://x', 't', 'MERGED', 1)",
        [],
    )
    .unwrap();
    for (key, val) in [
        ("endpoint_count", 8.0),
        ("controller_count", 4.0),
        ("entity_count", 3.0),
        ("repository_count", 2.0),
        ("production_loc", 4000.0),
        ("custom_query_count", 2.0),
        ("avg_cc_per_controller", 3.5),
    ] {
        conn.execute(
            "INSERT INTO repo_structural_metrics (repo_full_name, metric_key, value)
             VALUES ('spring-api', ?, ?)",
            rusqlite::params![key, val],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
         VALUES ('pr2', 2, 'android-app', 'http://y', 't2', 'MERGED', 1)",
        [],
    )
    .unwrap();
    for (key, val) in [
        ("fragment_count", 5.0),
        ("activity_count", 2.0),
        ("viewmodel_count", 4.0),
        ("production_loc", 6000.0),
        ("reactive_wiring_density", 1.2),
        ("nav_dispatch_density", 0.8),
        ("avg_cc_per_fragment", 4.0),
    ] {
        conn.execute(
            "INSERT INTO repo_structural_metrics (repo_full_name, metric_key, value)
             VALUES ('android-app', ?, ?)",
            rusqlite::params![key, val],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (1, 'pr1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pr_doc_evaluation (pr_id, sprint_id, total_doc_score) VALUES ('pr1', ?, 4.0)",
        params![sprint_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO flags (student_id, sprint_id, flag_type, severity, details)
         VALUES ('bob', ?, 'SOME_FLAG', 'CRITICAL', NULL)",
        params![sprint_id],
    )
    .unwrap();
}

fn seed_one_absent_axis(conn: &Connection) {
    let project_id = 2i64;
    let sprint_id = 20i64;
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, 'team-02', 'Team 02')",
        params![project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
        params![sprint_id, project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES ('carol', 'carol', 'carol', 'Carol', ?)",
        params![project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (10, 'T-10', 'T-10', 'TASK', 'DONE', 10, 'carol', ?)",
        params![sprint_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
         VALUES (10, 'Cap', 'A', 1, '2026-01-01')",
        [],
    )
    .unwrap();
}

fn seed_zero_delivery(conn: &Connection) {
    let project_id = 3i64;
    let sprint_id = 30i64;
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, 'team-03', 'Team 03')",
        params![project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
        params![sprint_id, project_id],
    )
    .unwrap();
    for (id, name) in [("dave", "Dave"), ("eve", "Eve")] {
        conn.execute(
            "INSERT INTO students (id, username, github_login, full_name, team_project_id)
             VALUES (?, ?, ?, ?, ?)",
            params![id, id, id, name, project_id],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (20, 'T-20', 'T-20', 'TASK', 'DONE', 8, 'dave', ?)",
        params![sprint_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
         VALUES (20, 'Cap', 'A', 1, '2026-01-01')",
        [],
    )
    .unwrap();
}

fn seed_security_flags(conn: &Connection) {
    let project_id = 4i64;
    let sprint_id = 40i64;
    conn.execute(
        "INSERT INTO projects (id, slug, name) VALUES (?, 'team-04', 'Team 04')",
        params![project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sprints (id, project_id, name, start_date, end_date)
         VALUES (?, ?, 'S1', '2026-01-01', '2026-01-15')",
        params![sprint_id, project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO students (id, username, github_login, full_name, team_project_id)
         VALUES ('frank', 'frank', 'frank', 'Frank', ?)",
        params![project_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, task_key, name, type, status, estimation_points, assignee_id, sprint_id)
         VALUES (30, 'T-30', 'T-30', 'TASK', 'DONE', 10, 'frank', ?)",
        params![sprint_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_ai_usage (task_id, model_value, level_value, declared, captured_at)
         VALUES (30, 'Cap', 'A', 1, '2026-01-01')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pull_requests (id, pr_number, repo_full_name, url, title, state, merged)
         VALUES ('pr4', 1, 'org/sec-repo', 'http://x', 't', 'MERGED', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (30, 'pr4')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO pr_doc_evaluation (pr_id, sprint_id, total_doc_score) VALUES ('pr4', ?, 5.0)",
        params![sprint_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO static_analysis_findings
         (repo_full_name, analyzer, rule_id, severity, category, file_path, message, fingerprint)
         VALUES ('org/sec-repo', 'pmd', 'R1', 'CRITICAL', 'security', 'Foo.java', 'x', 'fp-sec-1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO static_analysis_findings
         (repo_full_name, analyzer, rule_id, severity, category, file_path, message, fingerprint)
         VALUES ('org/sec-repo', 'pmd', 'R2', 'CRITICAL', 'style', 'Bar.java', 'y', 'fp-sec-2')",
        [],
    )
    .unwrap();
}
