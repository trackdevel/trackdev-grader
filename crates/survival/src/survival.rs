//! Survival orchestration — parse → fingerprint → blame → store → aggregate.
//! Mirrors `src/survival/survival.py`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::params;
use tracing::{info, warn};

use sprint_grader_core::Database;

use crate::blame::{
    blame_file, blame_statement, build_commit_to_pr_map, build_email_to_student_map,
    resolve_blame_authors, CommitPrMap, EmailStudentMap,
};
use crate::diff_lines::compute_pr_line_metrics;
use crate::fingerprint::fingerprint_file;
use crate::parser::parse_file;

/// Opt-in: restrict fingerprint+blame to files touched by this sprint's PR
/// commits. Cuts per-repo work by ~40-70% on large repos, at the cost of
/// losing cross-team matches on untouched boilerplate. Default off to preserve
/// existing semantics; set `SURVIVAL_RESTRICT_TO_PR_FILES=1` to enable.
fn restrict_to_pr_files_enabled() -> bool {
    matches!(
        std::env::var("SURVIVAL_RESTRICT_TO_PR_FILES").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

// Directories skipped when scanning for source files (build output, IDE, git metadata).
static SKIP_DIRS: &[&str] = &[
    "build",
    "target",
    ".gradle",
    ".git",
    "node_modules",
    ".idea",
    ".settings",
    "bin",
    "gen",
    "out",
];

fn discover_source_files(repo_path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    walk_dir(repo_path, repo_path, &mut out);
    out.sort();
    out
}

fn walk_dir(repo_root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if SKIP_DIRS.contains(&name) {
                    continue;
                }
            }
            walk_dir(repo_root, &path, out);
        } else if path.is_file() {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("java") | Some("xml")) {
                if let Ok(rel) = path.strip_prefix(repo_root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
}

pub fn discover_repos(
    data_dir: &Path,
    db: &Database,
) -> rusqlite::Result<HashMap<String, PathBuf>> {
    let mut stmt = db.conn.prepare(
        "SELECT DISTINCT repo_full_name FROM pull_requests
         WHERE repo_full_name IS NOT NULL AND repo_full_name != ''",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let expected: BTreeSet<String> = rows.collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut result: HashMap<String, PathBuf> = HashMap::new();
    for subdir in ["entregues", "repos"] {
        let repos_base = data_dir.join(subdir);
        if !repos_base.exists() {
            continue;
        }
        for repo_full_name in expected.iter() {
            if result.contains_key(repo_full_name) {
                continue;
            }
            let repo_name = repo_full_name.rsplit('/').next().unwrap_or(repo_full_name);
            if let Ok(iter) = std::fs::read_dir(&repos_base) {
                for entry in iter.flatten() {
                    let pd = entry.path();
                    if !pd.is_dir() {
                        continue;
                    }
                    let candidate = pd.join(repo_name);
                    if candidate.exists() && candidate.join(".git").exists() {
                        result.insert(repo_full_name.clone(), candidate);
                        break;
                    }
                }
            }
        }
    }

    for m in expected.difference(&result.keys().cloned().collect::<BTreeSet<_>>()) {
        warn!(repo = %m, "Repo not found locally");
    }
    info!(
        found = result.len(),
        total = expected.len(),
        "Discovered repos locally"
    );
    Ok(result)
}

pub fn project_for_repo(db: &Database, repo_full_name: &str) -> Option<i64> {
    db.conn
        .query_row(
            "SELECT DISTINCT s.team_project_id
             FROM pull_requests pr
             JOIN students s ON s.id = pr.author_id
             WHERE pr.repo_full_name = ? AND s.team_project_id IS NOT NULL
             LIMIT 1",
            [repo_full_name],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
}

pub fn sprint_id_for_project(db: &Database, project_id: i64, ordinal: u32) -> Option<i64> {
    let mut stmt = db
        .conn
        .prepare("SELECT id FROM sprints WHERE project_id = ? ORDER BY start_date")
        .ok()?;
    let rows = stmt.query_map([project_id], |r| r.get::<_, i64>(0)).ok()?;
    let ids: Vec<i64> = rows.collect::<rusqlite::Result<_>>().ok()?;
    if ordinal == 0 || (ordinal as usize) > ids.len() {
        return None;
    }
    Some(ids[ordinal as usize - 1])
}

/// 1-based ordinal (by `start_date ASC`) of `sprint_id` within its project.
/// Returns `None` when the sprint doesn't exist or has no `project_id`.
pub fn ordinal_for_sprint_id(db: &Database, sprint_id: i64) -> Option<u32> {
    let project_id: i64 = db
        .conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()?;
    let mut stmt = db
        .conn
        .prepare(
            "SELECT id FROM sprints
             WHERE project_id = ? AND start_date IS NOT NULL AND start_date != ''
             ORDER BY start_date ASC",
        )
        .ok()?;
    let rows = stmt.query_map([project_id], |r| r.get::<_, i64>(0)).ok()?;
    for (idx, r) in rows.enumerate() {
        if let Ok(sid) = r {
            if sid == sprint_id {
                return Some((idx + 1) as u32);
            }
        }
    }
    None
}

pub fn all_sprint_ids_for_ordinal(db: &Database, ordinal: u32) -> Vec<i64> {
    let project_ids: Vec<i64> = {
        let mut stmt = match db.conn.prepare("SELECT id FROM projects") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| r.get::<_, i64>(0))
            .ok()
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    };
    project_ids
        .into_iter()
        .filter_map(|pid| sprint_id_for_project(db, pid, ordinal))
        .collect()
}

// ---- Fingerprint + blame a single repo ----

/// Return the set of files touched by this sprint's PR commits in this repo.
/// Uses `git show --name-only` in a single batched invocation.
fn pr_touched_files(
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    conn: &rusqlite::Connection,
) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pc.sha
         FROM pr_commits pc
         JOIN pull_requests pr ON pr.id = pc.pr_id
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND pr.repo_full_name = ?",
    )?;
    let shas: Vec<String> = stmt
        .query_map(params![sprint_id, repo_full_name], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut out: HashSet<String> = HashSet::new();
    if shas.is_empty() {
        return Ok(out);
    }
    // Chunk to keep the argv under typical OS limits on large sprints.
    for chunk in shas.chunks(256) {
        let mut args: Vec<String> = vec![
            "-C".into(),
            repo_path.to_string_lossy().into_owned(),
            "show".into(),
            "--no-patch".into(),
            "--format=".into(),
            "--name-only".into(),
        ];
        for sha in chunk {
            args.push(sha.clone());
        }
        let output = match Command::new("git").args(&args).output() {
            Ok(o) => o,
            Err(e) => {
                warn!(repo = repo_full_name, error = %e, "git show failed");
                continue;
            }
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let line = line.trim();
            if !line.is_empty() {
                out.insert(line.to_string());
            }
        }
    }
    Ok(out)
}

fn fingerprint_and_blame_repo(
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id: i64,
    db: &Database,
    commit_pr_map: &CommitPrMap,
    email_student_map: &EmailStudentMap,
) -> rusqlite::Result<i64> {
    let mut files = discover_source_files(repo_path);
    let total_discovered = files.len();
    if restrict_to_pr_files_enabled() {
        let touched = pr_touched_files(repo_path, repo_full_name, sprint_id, &db.conn)?;
        if !touched.is_empty() {
            files.retain(|f| touched.contains(f));
            info!(
                repo = repo_full_name,
                touched = touched.len(),
                kept = files.len(),
                discovered = total_discovered,
                "restricted to PR-touched files"
            );
        }
    }
    info!(
        repo = repo_full_name,
        files = files.len(),
        "discovered source files"
    );

    let mut count: i64 = 0;
    for file_path in &files {
        let full = repo_path.join(file_path);
        let source = match std::fs::read(&full) {
            Ok(b) => b,
            Err(e) => {
                warn!(repo = repo_full_name, file = %file_path, error = %e, "read failed");
                continue;
            }
        };
        let parse_result = match parse_file(&source, file_path) {
            Some(r) => r,
            None => continue,
        };
        let file_fps = fingerprint_file(&parse_result);
        let blame_lines = blame_file(repo_path, file_path);

        let mut method_fp_map: HashMap<String, String> = HashMap::new();
        for mfp in &file_fps.methods {
            method_fp_map.insert(mfp.method_name.clone(), mfp.method_fp.clone());
        }

        for sfp in &file_fps.statements {
            let mut sb_vec = blame_statement(&blame_lines, sfp.start_line, sfp.end_line)
                .map(|sb| vec![sb])
                .unwrap_or_default();
            resolve_blame_authors(&mut sb_vec, commit_pr_map, email_student_map);
            let sb = sb_vec.into_iter().next();
            let blame_commit = sb.as_ref().map(|s| s.commit_sha.clone());
            let blame_author = sb.as_ref().and_then(|s| s.author_login.clone());

            let method_fp = sfp
                .method_name
                .as_ref()
                .and_then(|n| method_fp_map.get(n))
                .cloned();

            db.conn.execute(
                "INSERT INTO fingerprints
                 (file_path, repo_full_name, statement_index, method_name,
                  raw_fingerprint, normalized_fingerprint, method_fingerprint,
                  blame_commit, blame_author_login, sprint_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    file_path,
                    repo_full_name,
                    sfp.statement_index as i64,
                    sfp.method_name,
                    sfp.raw_fp,
                    sfp.normalized_fp,
                    method_fp,
                    blame_commit,
                    blame_author,
                    sprint_id,
                ],
            )?;
            count += 1;
        }
    }
    Ok(count)
}

// ---- Per-PR survival ----

fn compute_pr_survival(db: &Database, sprint_id: i64) -> rusqlite::Result<()> {
    let prs = db
        .get_pull_requests_for_sprint(sprint_id)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    for pr in prs {
        let mut stmt = db
            .conn
            .prepare("SELECT sha FROM pr_commits WHERE pr_id = ?")?;
        let shas: Vec<String> = stmt
            .query_map([&pr.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        if shas.is_empty() {
            continue;
        }

        let placeholders: String = std::iter::repeat("?")
            .take(shas.len())
            .collect::<Vec<_>>()
            .join(",");

        let sql_raw = format!(
            "SELECT COUNT(*) FROM fingerprints
             WHERE sprint_id = ? AND blame_commit IN ({placeholders})"
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(1 + shas.len());
        params_vec.push(Box::new(sprint_id));
        for s in &shas {
            params_vec.push(Box::new(s.clone()));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|b| b.as_ref()).collect();
        let surviving_raw: i64 = db
            .conn
            .query_row(&sql_raw, &params_refs[..], |r| r.get(0))?;

        let sql_norm = format!(
            "SELECT COUNT(DISTINCT normalized_fingerprint) FROM fingerprints
             WHERE sprint_id = ? AND blame_commit IN ({placeholders})"
        );
        let surviving_norm: i64 = db
            .conn
            .query_row(&sql_norm, &params_refs[..], |r| r.get(0))?;

        let sql_meth = format!(
            "SELECT COUNT(DISTINCT method_fingerprint) FROM fingerprints
             WHERE sprint_id = ? AND blame_commit IN ({placeholders})
                 AND method_fingerprint IS NOT NULL"
        );
        let surviving_methods: i64 = db
            .conn
            .query_row(&sql_meth, &params_refs[..], |r| r.get(0))?;

        // Preserve the original "added" counts on re-runs.
        let existing: Option<Option<i64>> = db
            .conn
            .query_row(
                "SELECT statements_added_raw FROM pr_survival
                 WHERE pr_id = ? AND sprint_id = ?",
                params![&pr.id, sprint_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok();
        match existing {
            Some(Some(_)) => {
                db.conn.execute(
                    "UPDATE pr_survival
                     SET statements_surviving_raw = ?,
                         statements_surviving_normalized = ?,
                         methods_surviving = ?
                     WHERE pr_id = ? AND sprint_id = ?",
                    params![
                        surviving_raw,
                        surviving_norm,
                        surviving_methods,
                        &pr.id,
                        sprint_id,
                    ],
                )?;
            }
            _ => {
                db.conn.execute(
                    "INSERT OR REPLACE INTO pr_survival
                     (pr_id, sprint_id,
                      statements_added_raw, statements_surviving_raw,
                      statements_added_normalized, statements_surviving_normalized,
                      methods_added, methods_surviving)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        &pr.id,
                        sprint_id,
                        surviving_raw,
                        surviving_raw,
                        surviving_norm,
                        surviving_norm,
                        surviving_methods,
                        surviving_methods,
                    ],
                )?;
            }
        }
    }
    Ok(())
}

// ---- Per-student survival ----

fn compute_student_survival(db: &Database, sprint_id: i64) -> rusqlite::Result<()> {
    // `student_sprint_survival` has no PRIMARY KEY in the schema, so
    // `INSERT OR REPLACE` below degrades to plain INSERT and re-runs of
    // `run-all` / `survive` accumulate duplicate rows (3-way dups were
    // observed on the live DB after two re-runs). Wipe the sprint's rows
    // first — mirrors the same fix applied to `student_sprint_metrics`
    // and `flags`.
    db.conn.execute(
        "DELETE FROM student_sprint_survival WHERE sprint_id = ?",
        [sprint_id],
    )?;

    let mut stmt = db.conn.prepare(
        "SELECT DISTINCT s.id FROM students s
         JOIN tasks t ON t.assignee_id = s.id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    let student_ids: Vec<String> = stmt
        .query_map([sprint_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    for student_id in student_ids {
        let mut stmt = db.conn.prepare(
            "SELECT DISTINCT
                 ps.statements_added_raw,
                 ps.statements_surviving_raw,
                 ps.statements_added_normalized,
                 ps.statements_surviving_normalized,
                 ps.methods_added,
                 ps.methods_surviving
             FROM pr_survival ps
             JOIN pull_requests pr ON pr.id = ps.pr_id
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.assignee_id = ? AND t.type != 'USER_STORY'",
        )?;
        let rows = stmt.query_map(params![sprint_id, &student_id], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            ))
        })?;
        let mut total_raw: i64 = 0;
        let mut surv_raw: i64 = 0;
        let mut total_norm: i64 = 0;
        let mut surv_norm: i64 = 0;
        let mut total_meth: i64 = 0;
        let mut surv_meth: i64 = 0;
        for r in rows {
            let (tr, sr, tn, sn, tm, sm) = r?;
            total_raw += tr;
            surv_raw += sr;
            total_norm += tn;
            surv_norm += sn;
            total_meth += tm;
            surv_meth += sm;
        }
        drop(stmt);

        let rate_raw = if total_raw > 0 {
            surv_raw as f64 / total_raw as f64
        } else {
            0.0
        };
        let rate_norm = if total_norm > 0 {
            surv_norm as f64 / total_norm as f64
        } else {
            0.0
        };

        let est_points: i64 = db
            .conn
            .query_row(
                "SELECT COALESCE(SUM(estimation_points), 0) FROM tasks
                 WHERE sprint_id = ? AND assignee_id = ? AND status = 'DONE'
                     AND type != 'USER_STORY'",
                params![sprint_id, &student_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let est_density = if est_points > 0 {
            surv_norm as f64 / est_points as f64
        } else {
            0.0
        };

        db.conn.execute(
            "INSERT OR REPLACE INTO student_sprint_survival
             (student_id, sprint_id,
              total_stmts_raw, surviving_stmts_raw, survival_rate_raw,
              total_stmts_normalized, surviving_stmts_normalized, survival_rate_normalized,
              total_methods, surviving_methods,
              estimation_points_total, estimation_density)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                student_id,
                sprint_id,
                total_raw,
                surv_raw,
                rate_raw,
                total_norm,
                surv_norm,
                rate_norm,
                total_meth,
                surv_meth,
                est_points,
                est_density,
            ],
        )?;
    }
    Ok(())
}

// ---- Public entry point ----

pub fn compute_survival(
    db: &Database,
    sprint: u32,
    data_dir: &Path,
    sprint_ids: Option<Vec<i64>>,
) -> rusqlite::Result<()> {
    let commit_pr_map = build_commit_to_pr_map(&db.conn)?;
    let email_student_map = build_email_to_student_map(&db.conn)?;

    let repo_map = discover_repos(data_dir, db)?;
    if repo_map.is_empty() {
        warn!("No repos found locally. Run 'collect' first.");
        return Ok(());
    }

    let sprint_ids = sprint_ids.unwrap_or_else(|| all_sprint_ids_for_ordinal(db, sprint));
    if sprint_ids.is_empty() {
        warn!(sprint, "No matching sprints in DB. Run 'collect' first.");
        return Ok(());
    }

    // Clear old fingerprints for idempotent re-run.
    for sid in &sprint_ids {
        db.conn
            .execute("DELETE FROM fingerprints WHERE sprint_id = ?", [sid])?;
    }

    info!(
        repos = repo_map.len(),
        "Fingerprinting and blaming repos..."
    );
    let mut total_fps: i64 = 0;
    for (repo_full_name, repo_path) in &repo_map {
        let project_id = match project_for_repo(db, repo_full_name) {
            Some(p) => p,
            None => {
                warn!(repo = %repo_full_name, "Cannot determine project — skipping");
                continue;
            }
        };
        let sprint_id = match sprint_id_for_project(db, project_id, sprint) {
            Some(s) => s,
            None => {
                warn!(sprint, project_id, "No matching sprint — skipping");
                continue;
            }
        };
        total_fps += fingerprint_and_blame_repo(
            repo_path,
            repo_full_name,
            sprint_id,
            db,
            &commit_pr_map,
            &email_student_map,
        )?;
    }
    info!(total = total_fps, "Stored fingerprints across all repos");

    info!("Computing per-PR survival...");
    for sid in &sprint_ids {
        compute_pr_survival(db, *sid)?;
    }
    info!("Computing per-student survival...");
    for sid in &sprint_ids {
        compute_student_survival(db, *sid)?;
    }

    info!("Computing per-PR line metrics (LAT/LAR/LS)...");
    for sid in &sprint_ids {
        compute_pr_line_metrics(&db.conn, *sid, &repo_map, 10, false)?;
    }

    info!("Survival computation complete");
    Ok(())
}
