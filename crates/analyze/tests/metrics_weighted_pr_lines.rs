//! Tests for `compute_metrics_for_sprint_id` — weighted PR lines and
//! idempotency of the DELETE + INSERT transaction.

mod common;

use sprint_grader_analyze::compute_metrics_for_sprint_id;

/// Set up: project 1, sprint 10 (2026-02-01→2026-02-15), two students.
/// Student A has one DONE task (3 pts) linked to a PR with 100 adds / 50 dels.
/// Student B has one DONE task (1 pt) linked to the *same* PR.
/// Weight for A = 3/4; weight for B = 1/4.
/// Expected weighted_pr_lines: A = 150 * 0.75 = 112.5, B = 150 * 0.25 = 37.5.
#[test]
fn weighted_pr_lines_split_by_estimation_points() {
    let conn = common::make_db();
    common::seed_default_project(&conn);

    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");

    common::seed_task(
        &conn,
        1,
        common::SPRINT_ID,
        Some("alice"),
        Some(3),
        "DONE",
        "TASK",
    );
    common::seed_task(
        &conn,
        2,
        common::SPRINT_ID,
        Some("bob"),
        Some(1),
        "DONE",
        "TASK",
    );

    common::seed_pr(
        &conn,
        "pr-1",
        1,
        common::REPO_FULL_NAME,
        Some("alice"),
        Some("alice"),
        "closed",
        true,
        Some("2026-02-10T10:00:00Z"),
        Some(100),
        Some(50),
        None,
    );
    common::link_task_pr(&conn, 1, "pr-1");
    common::link_task_pr(&conn, 2, "pr-1");

    compute_metrics_for_sprint_id(&conn, common::SPRINT_ID, 24).unwrap();

    let rows: Vec<(String, f64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT student_id, weighted_pr_lines FROM student_sprint_metrics
                 WHERE sprint_id = ? ORDER BY student_id",
            )
            .unwrap();
        stmt.query_map([common::SPRINT_ID], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };

    assert_eq!(rows.len(), 2, "one row per student");

    let alice = rows.iter().find(|(id, _)| id == "alice").unwrap();
    let bob = rows.iter().find(|(id, _)| id == "bob").unwrap();

    // 150 total * 3/4 = 112.5
    assert!(
        (alice.1 - 112.5).abs() < 0.01,
        "alice weighted_pr_lines = {}, want 112.5",
        alice.1
    );
    // 150 total * 1/4 = 37.5
    assert!(
        (bob.1 - 37.5).abs() < 0.01,
        "bob weighted_pr_lines = {}, want 37.5",
        bob.1
    );
}

/// Calling compute_metrics_for_sprint_id twice must not double the rows
/// (the transactional DELETE + INSERT must stay idempotent).
#[test]
fn metrics_computation_is_idempotent() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "carol");
    common::seed_task(
        &conn,
        10,
        common::SPRINT_ID,
        Some("carol"),
        Some(2),
        "DONE",
        "TASK",
    );

    compute_metrics_for_sprint_id(&conn, common::SPRINT_ID, 24).unwrap();
    compute_metrics_for_sprint_id(&conn, common::SPRINT_ID, 24).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM student_sprint_metrics WHERE sprint_id = ?",
            [common::SPRINT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "re-running must not duplicate rows; got {count}");
}

/// A student with no tasks assigned in the sprint still gets a metrics row
/// (zero values), so the report LEFT JOIN always finds a row to COALESCE.
#[test]
fn student_with_no_tasks_gets_zero_metrics_row() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "diana");
    // No tasks seeded for diana.

    compute_metrics_for_sprint_id(&conn, common::SPRINT_ID, 24).unwrap();

    let lines: f64 = conn
        .query_row(
            "SELECT weighted_pr_lines FROM student_sprint_metrics
             WHERE sprint_id = ? AND student_id = 'diana'",
            [common::SPRINT_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lines, 0.0, "student with no tasks should have 0 PR lines");
}
