//! `purge_projects` (full cascade) and `purge_cache` (targeted derived-row
//! deletion). Mirrors `Database.purge_projects` / `Database.purge_cache` in
//! `src/db/schema.py`.

use std::collections::BTreeMap;

use rusqlite::{params_from_iter, Connection};
use tracing::info;

fn placeholders(n: usize) -> String {
    std::iter::repeat("?").take(n).collect::<Vec<_>>().join(",")
}

/// Full cascade delete for a set of projects. Rows are removed leaves-first
/// so foreign-key constraints stay satisfied throughout.
pub fn purge_projects(conn: &Connection, project_ids: &[i64]) -> rusqlite::Result<()> {
    if project_ids.is_empty() {
        return Ok(());
    }
    let pp = placeholders(project_ids.len());

    // Resolve sprint_ids owned by these projects.
    let sql = format!("SELECT id FROM sprints WHERE project_id IN ({})", pp);
    let mut stmt = conn.prepare(&sql)?;
    let sprint_ids: Vec<i64> = stmt
        .query_map(params_from_iter(project_ids.iter()), |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    if sprint_ids.is_empty() {
        // No sprints yet — just drop students + projects and commit.
        let sql = format!("DELETE FROM students WHERE team_project_id IN ({})", pp);
        conn.execute(&sql, params_from_iter(project_ids.iter()))?;
        let sql = format!("DELETE FROM projects WHERE id IN ({})", pp);
        conn.execute(&sql, params_from_iter(project_ids.iter()))?;
        return Ok(());
    }
    let sp = placeholders(sprint_ids.len());

    // Resolve PR ids linked to tasks in these sprints.
    let sql = format!(
        "SELECT DISTINCT tpr.pr_id FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id IN ({})",
        sp
    );
    let mut stmt = conn.prepare(&sql)?;
    let pr_ids: Vec<String> = stmt
        .query_map(params_from_iter(sprint_ids.iter()), |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    // PR-level tables.
    if !pr_ids.is_empty() {
        let pr_ph = placeholders(pr_ids.len());
        let pr_tables = [
            "pr_commits",
            "pr_reviews",
            "pr_doc_evaluation",
            "pr_compilation",
            "pr_line_metrics",
            "pr_behavioral_signals",
            "pr_ai_probability",
            "pr_workflow_metrics",
            "pr_regularity",
            "pr_submission_tiers",
            "pr_survival",
        ];
        for table in pr_tables {
            let sql = format!("DELETE FROM {} WHERE pr_id IN ({})", table, pr_ph);
            let _ = conn.execute(&sql, params_from_iter(pr_ids.iter()));
        }
        let sql = format!("DELETE FROM pull_requests WHERE id IN ({})", pr_ph);
        conn.execute(&sql, params_from_iter(pr_ids.iter()))?;
    }

    // Task-level.
    let sql = format!(
        "DELETE FROM task_pull_requests WHERE task_id IN
         (SELECT id FROM tasks WHERE sprint_id IN ({}))",
        sp
    );
    conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;

    let sql = format!(
        "DELETE FROM task_description_evaluation WHERE sprint_id IN ({})",
        sp
    );
    let _ = conn.execute(&sql, params_from_iter(sprint_ids.iter()));

    for table in ["task_group_members", "task_similarity_groups"] {
        let sql = format!("DELETE FROM {} WHERE sprint_id IN ({})", table, sp);
        let _ = conn.execute(&sql, params_from_iter(sprint_ids.iter()));
    }
    let sql = format!("DELETE FROM tasks WHERE sprint_id IN ({})", sp);
    conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;

    // Sprint-scoped tables (keyed by sprint_id).
    let sprint_tables = [
        "fingerprints",
        "cosmetic_rewrites",
        "cross_team_matches",
        "student_sprint_survival",
        "student_sprint_metrics",
        "flags",
        "student_sprint_contribution",
        "method_metrics",
        "satd_items",
        "student_sprint_quality",
        "student_sprint_temporal",
        "student_sprint_regularity",
        "text_consistency_scores",
        "student_sprint_ai_usage",
        "file_style_features",
        "file_perplexity",
        "file_ai_probability",
        "llm_ai_assessment",
        "curriculum_violations",
    ];
    for table in sprint_tables {
        let sql = format!("DELETE FROM {} WHERE sprint_id IN ({})", table, sp);
        let _ = conn.execute(&sql, params_from_iter(sprint_ids.iter()));
    }

    // Project+sprint-scoped tables (keyed by project_id).
    let project_sprint_tables = [
        "team_sprint_inequality",
        "sprint_planning_quality",
        "team_sprint_collaboration",
        "compilation_failure_summary",
        "code_practices_evaluation",
    ];
    for table in project_sprint_tables {
        let sql = format!("DELETE FROM {} WHERE project_id IN ({})", table, pp);
        let _ = conn.execute(&sql, params_from_iter(project_ids.iter()));
    }

    // Project-scoped.
    for table in [
        "student_style_baseline",
        "student_text_profile",
        "student_trajectory",
    ] {
        let sql = format!("DELETE FROM {} WHERE project_id IN ({})", table, pp);
        let _ = conn.execute(&sql, params_from_iter(project_ids.iter()));
    }

    // Core entities.
    let sql = format!("DELETE FROM sprints WHERE project_id IN ({})", pp);
    conn.execute(&sql, params_from_iter(project_ids.iter()))?;
    let sql = format!("DELETE FROM students WHERE team_project_id IN ({})", pp);
    conn.execute(&sql, params_from_iter(project_ids.iter()))?;
    let sql = format!("DELETE FROM projects WHERE id IN ({})", pp);
    conn.execute(&sql, params_from_iter(project_ids.iter()))?;

    info!(
        projects = project_ids.len(),
        sprints = sprint_ids.len(),
        prs = pr_ids.len(),
        "purge_projects complete"
    );
    Ok(())
}

/// Target flags for `purge_cache`. Mirrors the Python CLI flag surface.
#[derive(Debug, Default, Clone, Copy)]
pub struct CacheTargets {
    pub line_metrics: bool,
    pub survival: bool,
    pub compilation: bool,
    pub doc_eval: bool,
}

impl CacheTargets {
    pub fn any(&self) -> bool {
        self.line_metrics || self.survival || self.compilation || self.doc_eval
    }
}

pub type PurgeCacheResult = BTreeMap<String, i64>;

/// Drop derived cache rows for the given sprints so they are recomputed on
/// the next pipeline run. When `project_ids` is `Some`, rows that are
/// per-PR / per-student / per-repo are additionally scoped to those projects;
/// otherwise the whole sprint is purged.
pub fn purge_cache(
    conn: &Connection,
    sprint_ids: &[i64],
    project_ids: Option<&[i64]>,
    targets: CacheTargets,
) -> rusqlite::Result<PurgeCacheResult> {
    let mut deleted: PurgeCacheResult = BTreeMap::new();
    if sprint_ids.is_empty() || !targets.any() {
        return Ok(deleted);
    }
    let sp = placeholders(sprint_ids.len());

    // Resolve the PR / repo scope when project_ids is set.
    let (pr_ids, repo_names): (Option<Vec<String>>, Option<Vec<String>>) =
        if let Some(pids) = project_ids {
            let pp = placeholders(pids.len());
            let sql = format!(
                "SELECT DISTINCT tpr.pr_id FROM task_pull_requests tpr
             JOIN tasks t ON t.id = tpr.task_id
             JOIN sprints s ON s.id = t.sprint_id
             WHERE t.sprint_id IN ({}) AND s.project_id IN ({})",
                sp, pp
            );
            let mut stmt = conn.prepare(&sql)?;
            let args: Vec<i64> = sprint_ids.iter().chain(pids.iter()).copied().collect();
            let pr_ids: Vec<String> = stmt
                .query_map(params_from_iter(args.iter()), |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);

            let sql = format!(
                "SELECT DISTINCT pr.repo_full_name FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             JOIN sprints s ON s.id = t.sprint_id
             WHERE t.sprint_id IN ({}) AND s.project_id IN ({})
               AND pr.repo_full_name IS NOT NULL",
                sp, pp
            );
            let mut stmt = conn.prepare(&sql)?;
            let repos: Vec<String> = stmt
                .query_map(params_from_iter(args.iter()), |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            (Some(pr_ids), Some(repos))
        } else {
            (None, None)
        };

    let record = |deleted: &mut PurgeCacheResult, table: &str, rowcount: i64| {
        *deleted.entry(table.to_string()).or_insert(0) += rowcount;
    };

    let del_by_pr_sprint = |deleted: &mut PurgeCacheResult, table: &str| -> rusqlite::Result<()> {
        if let Some(pr_ids) = &pr_ids {
            if pr_ids.is_empty() {
                deleted.entry(table.to_string()).or_insert(0);
                return Ok(());
            }
            let pr_ph = placeholders(pr_ids.len());
            let sql = format!(
                "DELETE FROM {} WHERE sprint_id IN ({}) AND pr_id IN ({})",
                table, sp, pr_ph
            );
            let args: Vec<Box<dyn rusqlite::ToSql>> = sprint_ids
                .iter()
                .map(|x| Box::new(*x) as Box<dyn rusqlite::ToSql>)
                .chain(
                    pr_ids
                        .iter()
                        .map(|x| Box::new(x.clone()) as Box<dyn rusqlite::ToSql>),
                )
                .collect();
            let count = conn.execute(&sql, params_from_iter(args.iter()))?;
            record(deleted, table, count as i64);
        } else {
            let sql = format!("DELETE FROM {} WHERE sprint_id IN ({})", table, sp);
            let count = conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;
            record(deleted, table, count as i64);
        }
        Ok(())
    };

    let del_by_repo_sprint =
        |deleted: &mut PurgeCacheResult, table: &str| -> rusqlite::Result<()> {
            if let Some(repos) = &repo_names {
                if repos.is_empty() {
                    deleted.entry(table.to_string()).or_insert(0);
                    return Ok(());
                }
                let rp = placeholders(repos.len());
                let sql = format!(
                    "DELETE FROM {} WHERE sprint_id IN ({}) AND repo_full_name IN ({})",
                    table, sp, rp
                );
                let args: Vec<Box<dyn rusqlite::ToSql>> = sprint_ids
                    .iter()
                    .map(|x| Box::new(*x) as Box<dyn rusqlite::ToSql>)
                    .chain(
                        repos
                            .iter()
                            .map(|x| Box::new(x.clone()) as Box<dyn rusqlite::ToSql>),
                    )
                    .collect();
                let count = conn.execute(&sql, params_from_iter(args.iter()))?;
                record(deleted, table, count as i64);
            } else {
                let sql = format!("DELETE FROM {} WHERE sprint_id IN ({})", table, sp);
                let count = conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;
                record(deleted, table, count as i64);
            }
            Ok(())
        };

    let del_by_sprint_project =
        |deleted: &mut PurgeCacheResult, table: &str| -> rusqlite::Result<()> {
            if let Some(pids) = project_ids {
                let pp = placeholders(pids.len());
                let sql = format!(
                    "DELETE FROM {} WHERE sprint_id IN ({}) AND project_id IN ({})",
                    table, sp, pp
                );
                let args: Vec<i64> = sprint_ids.iter().chain(pids.iter()).copied().collect();
                let count = conn.execute(&sql, params_from_iter(args.iter()))?;
                record(deleted, table, count as i64);
            } else {
                let sql = format!("DELETE FROM {} WHERE sprint_id IN ({})", table, sp);
                let count = conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;
                record(deleted, table, count as i64);
            }
            Ok(())
        };

    if targets.line_metrics {
        del_by_pr_sprint(&mut deleted, "pr_line_metrics")?;
    }

    if targets.survival {
        del_by_pr_sprint(&mut deleted, "pr_survival")?;
        del_by_repo_sprint(&mut deleted, "fingerprints")?;
        del_by_repo_sprint(&mut deleted, "cosmetic_rewrites")?;

        if let Some(pids) = project_ids {
            let pp = placeholders(pids.len());
            // cross_team_matches uses team_a_project_id / team_b_project_id
            let sql = format!(
                "DELETE FROM cross_team_matches
                 WHERE sprint_id IN ({}) AND (team_a_project_id IN ({}) OR team_b_project_id IN ({}))",
                sp, pp, pp
            );
            let args: Vec<i64> = sprint_ids
                .iter()
                .chain(pids.iter())
                .chain(pids.iter())
                .copied()
                .collect();
            let count = conn.execute(&sql, params_from_iter(args.iter()))?;
            record(&mut deleted, "cross_team_matches", count as i64);

            let sql = format!(
                "DELETE FROM student_sprint_survival WHERE sprint_id IN ({})
                 AND student_id IN (SELECT id FROM students WHERE team_project_id IN ({}))",
                sp, pp
            );
            let args: Vec<i64> = sprint_ids.iter().chain(pids.iter()).copied().collect();
            let count = conn.execute(&sql, params_from_iter(args.iter()))?;
            record(&mut deleted, "student_sprint_survival", count as i64);
        } else {
            let sql = format!("DELETE FROM cross_team_matches WHERE sprint_id IN ({})", sp);
            let count = conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;
            record(&mut deleted, "cross_team_matches", count as i64);
            let sql = format!(
                "DELETE FROM student_sprint_survival WHERE sprint_id IN ({})",
                sp
            );
            let count = conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;
            record(&mut deleted, "student_sprint_survival", count as i64);
        }
    }

    if targets.compilation {
        del_by_pr_sprint(&mut deleted, "pr_compilation")?;
        del_by_sprint_project(&mut deleted, "compilation_failure_summary")?;
    }

    if targets.doc_eval {
        del_by_pr_sprint(&mut deleted, "pr_doc_evaluation")?;
        if let Some(pids) = project_ids {
            let pp = placeholders(pids.len());
            let sql = format!(
                "DELETE FROM task_description_evaluation
                 WHERE sprint_id IN ({})
                   AND task_id IN (
                     SELECT t.id FROM tasks t
                     JOIN sprints s ON s.id = t.sprint_id
                     WHERE s.project_id IN ({})
                   )",
                sp, pp
            );
            let args: Vec<i64> = sprint_ids.iter().chain(pids.iter()).copied().collect();
            let count = conn.execute(&sql, params_from_iter(args.iter()))?;
            record(&mut deleted, "task_description_evaluation", count as i64);
        } else {
            let sql = format!(
                "DELETE FROM task_description_evaluation WHERE sprint_id IN ({})",
                sp
            );
            let count = conn.execute(&sql, params_from_iter(sprint_ids.iter()))?;
            record(&mut deleted, "task_description_evaluation", count as i64);
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE projects (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE sprints (id INTEGER PRIMARY KEY, project_id INTEGER,
                name TEXT, start_date TEXT, end_date TEXT);
             CREATE TABLE students (id TEXT PRIMARY KEY, full_name TEXT,
                github_login TEXT, team_project_id INTEGER, email TEXT);
             CREATE TABLE tasks (id INTEGER PRIMARY KEY, task_key TEXT, name TEXT,
                type TEXT, status TEXT, estimation_points INTEGER,
                assignee_id TEXT, sprint_id INTEGER, parent_task_id INTEGER);
             CREATE TABLE pull_requests (id TEXT PRIMARY KEY, pr_number INTEGER,
                repo_full_name TEXT, title TEXT, url TEXT, author_id TEXT,
                additions INTEGER, deletions INTEGER, changed_files INTEGER,
                created_at TEXT, merged INTEGER, merged_at TEXT, body TEXT);
             CREATE TABLE task_pull_requests (task_id INTEGER, pr_id TEXT,
                PRIMARY KEY (task_id, pr_id));
             CREATE TABLE pr_line_metrics (pr_id TEXT, sprint_id INTEGER,
                merge_sha TEXT, lat REAL, lar REAL, ls REAL,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE pr_survival (pr_id TEXT, sprint_id INTEGER,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE fingerprints (sprint_id INTEGER, repo_full_name TEXT,
                file_path TEXT, stmt_idx INTEGER, PRIMARY KEY
                (sprint_id, repo_full_name, file_path, stmt_idx));
             CREATE TABLE cosmetic_rewrites (sprint_id INTEGER, repo_full_name TEXT);
             CREATE TABLE cross_team_matches (sprint_id INTEGER,
                team_a_project_id INTEGER, team_b_project_id INTEGER);
             CREATE TABLE student_sprint_survival (student_id TEXT, sprint_id INTEGER,
                PRIMARY KEY (student_id, sprint_id));
             CREATE TABLE pr_compilation (pr_id TEXT, sprint_id INTEGER,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE compilation_failure_summary (project_id INTEGER,
                sprint_id INTEGER, PRIMARY KEY (project_id, sprint_id));
             CREATE TABLE pr_doc_evaluation (pr_id TEXT, sprint_id INTEGER,
                PRIMARY KEY (pr_id, sprint_id));
             CREATE TABLE task_description_evaluation (task_id INTEGER, sprint_id INTEGER,
                PRIMARY KEY (task_id, sprint_id));

             INSERT INTO projects VALUES (1, 'pds26-1a');
             INSERT INTO sprints VALUES (10, 1, 'Sprint 1', '2026-02-16', '2026-03-08');
             INSERT INTO tasks VALUES
                (100, 'T-1', 'Login', 'TASK', 'DONE', 3, 'u1', 10, NULL);
             INSERT INTO pull_requests
                (id, pr_number, repo_full_name, title, url, author_id,
                 additions, deletions, changed_files, created_at, merged, merged_at, body)
                VALUES ('pr-1', 42, 'udg-pds/spring-foo', 'Add', NULL, 'u1',
                        10, 0, 1, NULL, 1, NULL, NULL);
             INSERT INTO task_pull_requests VALUES (100, 'pr-1');
             INSERT INTO pr_line_metrics VALUES ('pr-1', 10, NULL, 10.0, 10.0, 10.0);
             INSERT INTO pr_survival VALUES ('pr-1', 10);
             INSERT INTO fingerprints VALUES (10, 'udg-pds/spring-foo', 'Foo.java', 0);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn cache_targets_any_reflects_flags() {
        let none = CacheTargets::default();
        assert!(!none.any());
        let some = CacheTargets {
            line_metrics: true,
            ..Default::default()
        };
        assert!(some.any());
    }

    #[test]
    fn purge_cache_line_metrics_removes_rows() {
        let conn = mk_conn();
        let targets = CacheTargets {
            line_metrics: true,
            ..Default::default()
        };
        let deleted = purge_cache(&conn, &[10], None, targets).unwrap();
        assert_eq!(deleted.get("pr_line_metrics"), Some(&1));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM pr_line_metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn purge_cache_survival_also_clears_fingerprints() {
        let conn = mk_conn();
        let targets = CacheTargets {
            survival: true,
            ..Default::default()
        };
        let deleted = purge_cache(&conn, &[10], None, targets).unwrap();
        assert_eq!(deleted.get("fingerprints"), Some(&1));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM fingerprints", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn purge_projects_cascades_cleanly() {
        let conn = mk_conn();
        purge_projects(&conn, &[1]).unwrap();
        let n_projects: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        let n_sprints: i64 = conn
            .query_row("SELECT COUNT(*) FROM sprints", [], |r| r.get(0))
            .unwrap();
        let n_prs: i64 = conn
            .query_row("SELECT COUNT(*) FROM pull_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_projects, 0);
        assert_eq!(n_sprints, 0);
        assert_eq!(n_prs, 0);
    }
}
