//! Diagnose PR-doc resume-loop on a live grading.db (ignored by default).
//!
//!   cargo test -p sprint-grader-evaluate diagnose_all_projects_pr_doc_resume -- --ignored --nocapture

use std::path::PathBuf;

use rusqlite::{params, Connection};

const STALE_BODY_LEN_THRESHOLD: i64 = 50;

fn db_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/grading.db")
}

fn pr_doc_row_present_and_fresh_fixed(
    conn: &Connection,
    pr_id: &str,
    sprint_id: i64,
) -> rusqlite::Result<bool> {
    let fresh: Option<i64> = conn
        .query_row(
            "SELECT 1
             FROM pr_doc_evaluation pde
             JOIN pull_requests pr ON pr.id = pde.pr_id
             WHERE pde.pr_id = ? AND pde.sprint_id = ?
               AND NOT (
                 pde.description_score = 0.0
                 AND length(coalesce(pr.body, '')) >= ?
                 AND coalesce(pde.scored_body_len, 0) < ?
               )
             LIMIT 1",
            params![
                pr_id,
                sprint_id,
                STALE_BODY_LEN_THRESHOLD,
                STALE_BODY_LEN_THRESHOLD
            ],
            |r| r.get(0),
        )
        .ok();
    Ok(fresh.is_some())
}

fn pr_doc_row_present_and_fresh_legacy(
    conn: &Connection,
    pr_id: &str,
    sprint_id: i64,
) -> rusqlite::Result<bool> {
    let row: Option<(f64, i64, i64)> = conn
        .query_row(
            "SELECT pde.description_score,
                    length(coalesce(pr.body, '')),
                    coalesce(pde.scored_body_len, 0)
             FROM pr_doc_evaluation pde
             JOIN pull_requests pr ON pr.id = pde.pr_id
             WHERE pde.pr_id = ? AND pde.sprint_id = ?",
            params![pr_id, sprint_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((desc_score, body_len, scored_body_len)) = row else {
        return Ok(false);
    };
    let is_stale = desc_score == 0.0
        && body_len >= STALE_BODY_LEN_THRESHOLD
        && scored_body_len < STALE_BODY_LEN_THRESHOLD;
    Ok(!is_stale)
}

struct SprintStats {
    total_prs: usize,
    need_eval_fixed: usize,
    need_eval_legacy: usize,
    no_row: usize,
    dup_rows: usize,
    legacy_false_retry: usize,
    truly_stale: usize,
}

fn stats_for_sprint(conn: &Connection, sprint_id: i64) -> SprintStats {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT pr.id, length(coalesce(pr.body, ''))
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
        )
        .expect("prepare");
    let prs: Vec<(String, i64)> = stmt
        .query_map([sprint_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");

    let mut stats = SprintStats {
        total_prs: prs.len(),
        need_eval_fixed: 0,
        need_eval_legacy: 0,
        no_row: 0,
        dup_rows: 0,
        legacy_false_retry: 0,
        truly_stale: 0,
    };

    for (pr_id, body_len) in &prs {
        let fixed = pr_doc_row_present_and_fresh_fixed(conn, pr_id, sprint_id).unwrap();
        let legacy = pr_doc_row_present_and_fresh_legacy(conn, pr_id, sprint_id).unwrap();
        if !fixed {
            stats.need_eval_fixed += 1;
        }
        if !legacy {
            stats.need_eval_legacy += 1;
        }
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
                params![pr_id, sprint_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if row_count == 0 {
            stats.no_row += 1;
        } else if row_count > 1 {
            stats.dup_rows += 1;
        }
        if fixed && !legacy {
            stats.legacy_false_retry += 1;
        }
        if !fixed && row_count > 0 {
            let best_scored_len: i64 = conn
                .query_row(
                    "SELECT coalesce(MAX(scored_body_len), 0)
                     FROM pr_doc_evaluation WHERE pr_id = ? AND sprint_id = ?",
                    params![pr_id, sprint_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if best_scored_len < STALE_BODY_LEN_THRESHOLD && *body_len >= STALE_BODY_LEN_THRESHOLD {
                stats.truly_stale += 1;
            }
        }
    }
    stats
}

#[test]
#[ignore = "cohort-wide PR doc resume diagnostic; run with --ignored --nocapture"]
fn diagnose_all_projects_pr_doc_resume() {
    let path = db_path();
    if !path.exists() {
        eprintln!("skip: {} missing", path.display());
        return;
    }
    let conn = Connection::open(&path).expect("open db");

    let total_dup_pairs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT pr_id, sprint_id FROM pr_doc_evaluation
                GROUP BY pr_id, sprint_id HAVING COUNT(*) > 1
             )",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let total_pde_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM pr_doc_evaluation", [], |r| r.get(0))
        .unwrap_or(0);
    let has_unique_idx: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='index' AND name='idx_pr_doc_evaluation_pr_sprint'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    println!("\n=== cohort PR doc resume diagnostic ===\n");
    println!("pr_doc_evaluation rows: {total_pde_rows}");
    println!("(pr_id,sprint_id) pairs with duplicate rows: {total_dup_pairs}");
    println!("unique index idx_pr_doc_evaluation_pr_sprint present: {has_unique_idx}");
    println!();

    let mut projects = conn
        .prepare(
            "SELECT p.id, p.name FROM projects p
             WHERE EXISTS (
               SELECT 1 FROM sprints s
               JOIN tasks t ON t.sprint_id = s.id
               JOIN task_pull_requests tpr ON tpr.task_id = t.id
               WHERE s.project_id = p.id AND t.type != 'USER_STORY'
             )
             ORDER BY p.name",
        )
        .expect("projects");
    let project_rows: Vec<(i64, String)> = projects
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("q")
        .filter_map(Result::ok)
        .collect();

    let mut cohort = SprintStats {
        total_prs: 0,
        need_eval_fixed: 0,
        need_eval_legacy: 0,
        no_row: 0,
        dup_rows: 0,
        legacy_false_retry: 0,
        truly_stale: 0,
    };

    println!(
        "{:<14} {:>5} {:>5} {:>5} {:>5} {:>5} {:>8} {:>5}",
        "project", "prs", "retry", "legcy", "norow", "dupPR", "leg→fix", "stale"
    );
    println!("{}", "-".repeat(68));

    for (pid, pname) in &project_rows {
        let mut sprints = conn
            .prepare("SELECT id FROM sprints WHERE project_id = ? ORDER BY id")
            .expect("sprints");
        let sprint_ids: Vec<i64> = sprints
            .query_map([pid], |r| r.get(0))
            .expect("q")
            .filter_map(Result::ok)
            .collect();

        let mut proj = SprintStats {
            total_prs: 0,
            need_eval_fixed: 0,
            need_eval_legacy: 0,
            no_row: 0,
            dup_rows: 0,
            legacy_false_retry: 0,
            truly_stale: 0,
        };
        for sid in sprint_ids {
            let s = stats_for_sprint(&conn, sid);
            proj.total_prs += s.total_prs;
            proj.need_eval_fixed += s.need_eval_fixed;
            proj.need_eval_legacy += s.need_eval_legacy;
            proj.no_row += s.no_row;
            proj.dup_rows += s.dup_rows;
            proj.legacy_false_retry += s.legacy_false_retry;
            proj.truly_stale += s.truly_stale;
        }

        if proj.need_eval_legacy > 0
            || proj.dup_rows > 0
            || proj.legacy_false_retry > 0
            || proj.need_eval_fixed > 0
        {
            println!(
                "{:<14} {:>5} {:>5} {:>5} {:>5} {:>5} {:>8} {:>5}",
                pname,
                proj.total_prs,
                proj.need_eval_fixed,
                proj.need_eval_legacy,
                proj.no_row,
                proj.dup_rows,
                proj.legacy_false_retry,
                proj.truly_stale
            );
        }

        cohort.total_prs += proj.total_prs;
        cohort.need_eval_fixed += proj.need_eval_fixed;
        cohort.need_eval_legacy += proj.need_eval_legacy;
        cohort.no_row += proj.no_row;
        cohort.dup_rows += proj.dup_rows;
        cohort.legacy_false_retry += proj.legacy_false_retry;
        cohort.truly_stale += proj.truly_stale;
    }

    println!("{}", "-".repeat(68));
    println!(
        "{:<14} {:>5} {:>5} {:>5} {:>5} {:>5} {:>8} {:>5}",
        "TOTAL",
        cohort.total_prs,
        cohort.need_eval_fixed,
        cohort.need_eval_legacy,
        cohort.no_row,
        cohort.dup_rows,
        cohort.legacy_false_retry,
        cohort.truly_stale
    );
    println!();
    println!("retry   = PRs needing eval with FIXED guard (current code after patch)");
    println!("legcy   = PRs needing eval with LEGACY guard (pre-patch bug)");
    println!("norow   = PRs with no pr_doc_evaluation row");
    println!("dupPR   = PRs with >1 pde row for same sprint");
    println!("leg→fix = fresh under fixed guard but legacy would retry (duplicate-row bug)");
    println!("stale   = legitimately stale (body filled, no row with scored_body_len>=50)");
}
