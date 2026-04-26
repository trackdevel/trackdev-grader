//! `debug-pr-lines`: read-only diagnostic that dumps LAT/LAR/LS per merged PR
//! for the selected projects. Mirrors `src/cli.py::debug_pr_lines`.
//!
//! Useful when the line-metrics cache looks off and you want to re-compute
//! without touching the DB. This runs `git diff` under each repo clone and
//! prints the numbers to stdout.

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use sprint_grader_core::formatting::fmt_float;
use sprint_grader_core::Database;
use sprint_grader_survival::diff_lines::compute_metrics_for_pr;
use tracing::warn;

type PrLineMetricsRow = (String, Option<f64>, Option<f64>, Option<f64>, Option<f64>);

pub fn debug_pr_lines(
    db: &Database,
    data_dir: &Path,
    sprint_ids: &[i64],
    project_names: &[(i64, String)],
) -> Result<()> {
    let repo_map = sprint_grader_survival::survival::discover_repos(data_dir, db)?;

    for (sid, name) in project_names {
        if !sprint_ids.contains(sid) {
            continue;
        }
        println!("\n{}", "=".repeat(60));
        println!("Project: {}  Sprint ID: {}", name, sid);
        println!("{}", "=".repeat(60));

        // Stored cache
        let mut stmt = db.conn.prepare(
            "SELECT pr_id, lat, lar, ls, cosmetic_lines FROM pr_line_metrics WHERE sprint_id = ?",
        )?;
        let rows: Vec<PrLineMetricsRow> = stmt
            .query_map([sid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<f64>>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, Option<f64>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        println!("\nStored pr_line_metrics rows: {}", rows.len());
        for (pr_id, lat, lar, ls, cosmetic) in &rows {
            let head: String = pr_id.chars().take(25).collect();
            println!(
                "  {}  LAT={}  LAR={}  LS={}  cosmetic={}",
                head,
                fmt_float(lat, 1),
                fmt_float(lar, 1),
                fmt_float(ls, 1),
                fmt_float(cosmetic, 1)
            );
        }

        // Recompute for every merged PR linked to the sprint.
        let mut stmt = db.conn.prepare(
            "SELECT DISTINCT pr.id, pr.pr_number, pr.repo_full_name, pr.additions
             FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND pr.merged = 1
             ORDER BY pr.repo_full_name, pr.pr_number, pr.id",
        )?;
        let prs: Vec<(String, i64, Option<String>, Option<i64>)> = stmt
            .query_map([sid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        // Detect duplicates: same (repo, pr_number) appearing under multiple pr_id rows.
        let mut dup_key_counts: std::collections::HashMap<(String, i64), i64> =
            std::collections::HashMap::new();
        for (_, pn, repo, _) in &prs {
            let key = (repo.clone().unwrap_or_default(), *pn);
            *dup_key_counts.entry(key).or_insert(0) += 1;
        }
        let dup_keys: std::collections::HashSet<(String, i64)> = dup_key_counts
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(k, _)| k.clone())
            .collect();

        println!("\nMerged PRs matching query: {}", prs.len());
        if !dup_keys.is_empty() {
            println!(
                "  WARNING: {} (repo, pr_number) tuples appear more than once — duplicate TrackDev PRs",
                dup_keys.len()
            );
        }

        for (pr_id, pr_number, repo_full, additions) in &prs {
            let repo_full = repo_full.clone().unwrap_or_default();
            let dup_tag = if dup_keys.contains(&(repo_full.clone(), *pr_number)) {
                "  [DUPLICATE]"
            } else {
                ""
            };
            println!(
                "\n  PR #{} ({})  additions={}  id={}{}",
                pr_number,
                repo_full,
                additions.unwrap_or(0),
                pr_id,
                dup_tag
            );

            let repo_path: std::path::PathBuf = match repo_map.get(&repo_full) {
                Some(p) if p.is_dir() => p.clone(),
                _ => {
                    println!("    SKIP: repo not found locally");
                    continue;
                }
            };

            let shas = collect_commit_shas(&db.conn, pr_id)?;
            println!(
                "    Commits: {}  SHAs: {:?}",
                shas.len(),
                shas.iter()
                    .map(|s| &s[..s.len().min(12)])
                    .collect::<Vec<_>>()
            );
            if shas.is_empty() {
                println!("    SKIP: no commits");
                continue;
            }

            let default_branch = sprint_grader_survival::diff_lines::default_branch_for(&repo_path);
            match compute_metrics_for_pr(
                &repo_path,
                pr_id,
                &shas,
                &default_branch,
                &repo_full,
                None,
            ) {
                None => println!("    RESULT: None (git commands failed)"),
                Some(m) => {
                    println!(
                        "    RESULT: LAT={}  LAR={}  LS={}  cosmetic={}",
                        m.lat, m.lar, m.ls, m.cosmetic_lines
                    );
                    if !m.cosmetic_report.is_empty() {
                        println!("    Cosmetic report: {}", m.cosmetic_report);
                    }
                }
            }
        }
    }
    Ok(())
}

fn collect_commit_shas(conn: &Connection, pr_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT sha FROM pr_commits WHERE pr_id = ? ORDER BY timestamp")?;
    let shas: Vec<String> = stmt
        .query_map([pr_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    Ok(shas)
}

#[allow(dead_code)]
fn _silence_unused_warn() {
    let _: fn(&str, &str) = |_, _| warn!("placeholder");
}
