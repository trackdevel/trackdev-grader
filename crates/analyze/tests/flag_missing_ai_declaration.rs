//! MISSING_AI_DECLARATION — WARNING per team member with DONE, non-USER_STORY
//! tasks that carry no declared "Ús de IA" usage (no `task_ai_usage` row, or
//! `declared = 0`). Advisory only — never a grade penalty.

mod common;

use rusqlite::{params, Connection};
use sprint_grader_analyze::flags::detect_flags_for_sprint_id;
use sprint_grader_core::Config;

/// Mark a task as having a fully-declared AI usage (both slots present).
fn declare_ai(conn: &Connection, task_id: i64) {
    conn.execute(
        "INSERT OR REPLACE INTO task_ai_usage
            (task_id, model_value, level_value, declared, captured_at)
         VALUES (?, 'Cursor', 'C', 1, '2026-06-07')",
        params![task_id],
    )
    .unwrap();
}

/// Attach `task_id` to its parent USER_STORY (`seed_task` always sets NULL).
fn set_parent(conn: &Connection, task_id: i64, parent_id: i64) {
    conn.execute(
        "UPDATE tasks SET parent_task_id = ? WHERE id = ?",
        params![parent_id, task_id],
    )
    .unwrap();
}

#[test]
fn warns_for_undeclared_done_task() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_student(&conn, "bob");
    // Alice declared her DONE task; Bob left his undeclared.
    common::seed_task(
        &conn,
        100,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "TASK",
    );
    declare_ai(&conn, 100);
    common::seed_task(
        &conn,
        101,
        common::SPRINT_ID,
        Some("bob"),
        Some(5),
        "DONE",
        "TASK",
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "alice"),
        0
    );
    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "bob"),
        1
    );
    assert_eq!(
        common::flag_severity_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "bob")
            .as_deref(),
        Some("WARNING")
    );
    let details =
        common::flag_details_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "bob")
            .unwrap();
    assert_eq!(details["count"].as_i64(), Some(1));
    assert_eq!(details["task_keys"][0].as_str(), Some("T-101"));
}

#[test]
fn silent_when_declared() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    common::seed_task(
        &conn,
        200,
        common::SPRINT_ID,
        Some("alice"),
        Some(3),
        "DONE",
        "TASK",
    );
    declare_ai(&conn, 200);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "alice"),
        0
    );
}

#[test]
fn silent_when_parent_user_story_declares_ai() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // Parent USER_STORY (400) carries the declaration; the child TASK (401) has
    // none of its own but inherits it — so no flag.
    common::seed_task(
        &conn,
        400,
        common::SPRINT_ID,
        Some("alice"),
        None,
        "DONE",
        "USER_STORY",
    );
    declare_ai(&conn, 400);
    common::seed_task(
        &conn,
        401,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "TASK",
    );
    set_parent(&conn, 401, 400);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "alice"),
        0,
        "child inherits the parent story's declaration"
    );
}

#[test]
fn warns_when_neither_task_nor_parent_declares() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "bob");
    // Parent USER_STORY (500) is itself undeclared, and so is its child TASK
    // (501) — neither has the attribute, so the child flags.
    common::seed_task(
        &conn,
        500,
        common::SPRINT_ID,
        Some("bob"),
        None,
        "DONE",
        "USER_STORY",
    );
    common::seed_task(
        &conn,
        501,
        common::SPRINT_ID,
        Some("bob"),
        Some(5),
        "DONE",
        "TASK",
    );
    set_parent(&conn, 501, 500);

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "bob"),
        1,
        "neither own nor parent declared → flag"
    );
}

#[test]
fn user_story_is_ignored() {
    let conn = common::make_db();
    common::seed_default_project(&conn);
    common::seed_student(&conn, "alice");
    // Undeclared, but a USER_STORY is not a gradeable unit — must not flag.
    common::seed_task(
        &conn,
        300,
        common::SPRINT_ID,
        Some("alice"),
        Some(5),
        "DONE",
        "USER_STORY",
    );

    detect_flags_for_sprint_id(&conn, common::SPRINT_ID, &Config::test_default()).unwrap();

    assert_eq!(
        common::count_flags_for(&conn, common::SPRINT_ID, "MISSING_AI_DECLARATION", "alice"),
        0
    );
}
