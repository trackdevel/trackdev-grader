//! Per-team ownership analysis (T-P2.3).
//!
//! For each project at a given sprint, computes:
//! - Per-file dominant author (winner of the per-file blame majority).
//! - Project truck factor: the smallest set of authors who jointly own
//!   >=95% of statements (`OWNERSHIP_COVERAGE`). Higher = more bus-resilient.
//!
//! Aggregates land in `team_sprint_ownership`. Per-file ownership is recomputed
//! on demand by [`file_ownership_for_project`] (the markdown renderer calls
//! this to draw the treemap) — it's a small derived view of `fingerprints`,
//! not worth a second persisted table.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use tracing::{info, warn};

/// Top-k authors covering at least this share of statements define the
/// truck factor. 95% is the de facto Git-Truck convention.
const OWNERSHIP_COVERAGE: f64 = 0.95;

/// Resolve a `fingerprints.blame_author_login` to a `students.id` if the
/// login matches a known team member (lowercased). Falls back to the raw
/// login string so unknown contributors still surface (they'll show under
/// their login rather than vanish into "owner unknown" — UNKNOWN_CONTRIBUTOR
/// flags handle the alerting).
fn login_to_student_map(conn: &Connection) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut stmt = conn
        .prepare("SELECT LOWER(github_login), id FROM students WHERE github_login IS NOT NULL")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (login, sid) = row?;
        map.insert(login, sid);
    }
    drop(stmt);
    let mut stmt = conn.prepare(
        "SELECT LOWER(login), student_id FROM github_users WHERE student_id IS NOT NULL",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (login, sid) = row?;
        map.entry(login).or_insert(sid);
    }
    Ok(map)
}

/// Per-file ownership row used by the markdown treemap. `statements` is the
/// total count of attributed statements in the file (the size of the tile);
/// `dominant_author` is the student_id (or fallback login) that owns the
/// largest share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOwnership {
    pub file_path: String,
    pub repo_full_name: Option<String>,
    pub dominant_author: String,
    pub statements: i64,
}

/// Aggregate per-author statement counts across the entire project for one
/// sprint, plus the per-file ownership map. Visible to tests.
pub fn file_ownership_for_project(
    conn: &Connection,
    sprint_id: i64,
    project_id: i64,
) -> rusqlite::Result<Vec<FileOwnership>> {
    let resolver = login_to_student_map(conn)?;
    let mut stmt = conn.prepare(
        "SELECT f.file_path, f.repo_full_name, LOWER(f.blame_author_login) as login,
                COUNT(*) as n
         FROM fingerprints f
         JOIN sprints s ON s.id = f.sprint_id
         WHERE f.sprint_id = ? AND s.project_id = ?
               AND f.blame_author_login IS NOT NULL
         GROUP BY f.file_path, f.repo_full_name, LOWER(f.blame_author_login)",
    )?;
    let rows: Vec<(String, Option<String>, String, i64)> = stmt
        .query_map(params![sprint_id, project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    // Per-file: sum statement counts per author, pick the max.
    type FileKey = (String, Option<String>);
    let mut per_file: BTreeMap<FileKey, BTreeMap<String, i64>> = BTreeMap::new();
    for (file, repo, login, n) in rows {
        let author = resolver.get(&login).cloned().unwrap_or(login);
        *per_file
            .entry((file, repo))
            .or_default()
            .entry(author)
            .or_insert(0) += n;
    }

    let mut out = Vec::with_capacity(per_file.len());
    for ((file, repo), authors) in per_file {
        // Tie-break on author id ascending so the dominant author is
        // deterministic across runs.
        let total: i64 = authors.values().sum();
        let dominant = authors
            .iter()
            .max_by(|(la, va), (lb, vb)| va.cmp(vb).then_with(|| lb.cmp(la)))
            .map(|(a, _)| a.clone())
            .unwrap_or_default();
        out.push(FileOwnership {
            file_path: file,
            repo_full_name: repo,
            dominant_author: dominant,
            statements: total,
        });
    }
    Ok(out)
}

/// Smallest k such that the top-k authors (by `total_per_author`) jointly
/// hold at least `OWNERSHIP_COVERAGE` of the total. Returns the ordered
/// owner list as well so the caller can persist it.
fn truck_factor_from(totals: &BTreeMap<String, i64>) -> (i32, Vec<String>) {
    if totals.is_empty() {
        return (0, Vec::new());
    }
    let grand_total: i64 = totals.values().sum();
    if grand_total == 0 {
        return (0, Vec::new());
    }
    // Sort by share descending; tie-break by author id ascending for
    // determinism (so a re-run on the same DB writes the same owners_csv).
    let mut ranked: Vec<(&String, &i64)> = totals.iter().collect();
    ranked.sort_by(|(la, va), (lb, vb)| vb.cmp(va).then_with(|| la.cmp(lb)));
    let target = (grand_total as f64 * OWNERSHIP_COVERAGE).ceil() as i64;
    let mut acc: i64 = 0;
    let mut owners = Vec::new();
    for (author, count) in ranked {
        owners.push(author.clone());
        acc += *count;
        if acc >= target {
            return (owners.len() as i32, owners);
        }
    }
    (owners.len() as i32, owners)
}

/// Stage entry point. For every project that has fingerprints in the given
/// sprint, recomputes truck_factor and owners_csv idempotently.
pub fn compute_team_ownership(conn: &Connection, sprint_id: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM team_sprint_ownership WHERE sprint_id = ?",
        [sprint_id],
    )?;

    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.project_id
         FROM sprints s
         JOIN fingerprints f ON f.sprint_id = s.id
         WHERE s.id = ?",
    )?;
    let projects: Vec<i64> = stmt
        .query_map([sprint_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    if projects.is_empty() {
        info!(sprint_id, "ownership: no fingerprints, nothing to compute");
        return Ok(0);
    }

    let mut written = 0usize;
    for project_id in projects {
        let files = file_ownership_for_project(conn, sprint_id, project_id)?;
        if files.is_empty() {
            warn!(
                project_id,
                sprint_id, "ownership: project has no usable fingerprints"
            );
            continue;
        }
        let mut totals: BTreeMap<String, i64> = BTreeMap::new();
        for f in &files {
            *totals.entry(f.dominant_author.clone()).or_insert(0) += f.statements;
        }
        let (truck_factor, owners) = truck_factor_from(&totals);
        let owners_csv = owners.join(",");
        conn.execute(
            "INSERT INTO team_sprint_ownership (project_id, sprint_id, truck_factor, owners_csv)
             VALUES (?, ?, ?, ?)",
            params![project_id, sprint_id, truck_factor, owners_csv],
        )?;
        info!(
            project_id,
            sprint_id, truck_factor, owners = %owners_csv, "ownership computed"
        );
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO projects (id, slug, name) VALUES (1, 'team-1', 'Team 1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (10, 1, 'S1', '2026-01-01', '2026-01-15')",
            [],
        )
        .unwrap();
        conn
    }

    fn seed_student(conn: &Connection, id: &str, login: &str) {
        conn.execute(
            "INSERT INTO students (id, github_login, team_project_id) VALUES (?, ?, 1)",
            params![id, login],
        )
        .unwrap();
    }

    fn seed_fp(conn: &Connection, file: &str, login: &str, count: usize) {
        for i in 0..count {
            conn.execute(
                "INSERT INTO fingerprints
                    (file_path, repo_full_name, statement_index, raw_fingerprint,
                     normalized_fingerprint, blame_author_login, sprint_id)
                 VALUES (?, 'r', ?, ?, ?, ?, 10)",
                params![file, i as i64, format!("raw{i}"), format!("nrm{i}"), login],
            )
            .unwrap();
        }
    }

    #[test]
    fn truck_factor_one_when_solo_owner() {
        let conn = mk_db();
        seed_student(&conn, "alice", "alice");
        seed_student(&conn, "bob", "bob");
        seed_fp(&conn, "Big.java", "alice", 95);
        seed_fp(&conn, "Big.java", "bob", 5);
        let written = compute_team_ownership(&conn, 10).unwrap();
        assert_eq!(written, 1);
        let (tf, owners): (i32, String) = conn
            .query_row(
                "SELECT truck_factor, owners_csv FROM team_sprint_ownership WHERE project_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(tf, 1);
        assert_eq!(owners, "alice");
    }

    #[test]
    fn truck_factor_grows_with_balanced_team() {
        let conn = mk_db();
        for sid in ["a", "b", "c", "d"] {
            seed_student(&conn, sid, sid);
            seed_fp(&conn, &format!("F_{sid}.java"), sid, 25);
        }
        compute_team_ownership(&conn, 10).unwrap();
        let tf: i32 = conn
            .query_row(
                "SELECT truck_factor FROM team_sprint_ownership WHERE project_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Top-3 of 4 evenly-sized owners cover 75%; need a 4th to clear 95%.
        assert_eq!(tf, 4);
    }

    #[test]
    fn unknown_login_falls_back_to_raw_string() {
        let conn = mk_db();
        // No matching student row for "outsider".
        seed_fp(&conn, "X.java", "outsider", 10);
        compute_team_ownership(&conn, 10).unwrap();
        let owners: String = conn
            .query_row(
                "SELECT owners_csv FROM team_sprint_ownership WHERE project_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owners, "outsider");
    }

    #[test]
    fn dominant_author_picks_majority_per_file() {
        let conn = mk_db();
        seed_student(&conn, "alice", "alice");
        seed_student(&conn, "bob", "bob");
        // Alice owns A.java (60/100), Bob owns B.java (90/100).
        seed_fp(&conn, "A.java", "alice", 60);
        seed_fp(&conn, "A.java", "bob", 40);
        seed_fp(&conn, "B.java", "alice", 10);
        seed_fp(&conn, "B.java", "bob", 90);
        let files = file_ownership_for_project(&conn, 10, 1).unwrap();
        let by_path: BTreeMap<_, _> = files
            .into_iter()
            .map(|f| (f.file_path.clone(), f))
            .collect();
        assert_eq!(by_path["A.java"].dominant_author, "alice");
        assert_eq!(by_path["A.java"].statements, 100);
        assert_eq!(by_path["B.java"].dominant_author, "bob");
        assert_eq!(by_path["B.java"].statements, 100);
    }

    #[test]
    fn empty_fingerprints_writes_no_row() {
        let conn = mk_db();
        let written = compute_team_ownership(&conn, 10).unwrap();
        assert_eq!(written, 0);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM team_sprint_ownership", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
