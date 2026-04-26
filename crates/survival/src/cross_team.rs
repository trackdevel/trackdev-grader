//! Cross-team method-level fingerprint matching.
//! Mirrors `src/survival/cross_team.py`.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rusqlite::{params, Connection};
use tracing::info;

/// Read `config/boilerplate_patterns.txt` — one fingerprint per line, ignoring
/// blank lines and `#` comments.
pub fn load_boilerplate(config_dir: &Path) -> HashSet<String> {
    let bp = config_dir.join("boilerplate_patterns.txt");
    let content = match std::fs::read_to_string(&bp) {
        Ok(s) => s,
        Err(_) => return HashSet::new(),
    };
    let mut out: HashSet<String> = HashSet::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.insert(line.to_string());
    }
    info!(count = out.len(), "Loaded boilerplate fingerprints");
    out
}

pub fn detect_cross_team_similarity(
    conn: &Connection,
    sprint_ids: &[i64],
    config_dir: &Path,
) -> rusqlite::Result<()> {
    // Clear old matches for these sprints.
    for sid in sprint_ids {
        conn.execute("DELETE FROM cross_team_matches WHERE sprint_id = ?", [sid])?;
    }

    let boilerplate = load_boilerplate(config_dir);

    // fingerprint → [(project_id, file_path, method_name, sprint_id)]
    let mut fp_to_locations: BTreeMap<String, Vec<(i64, String, String, i64)>> = BTreeMap::new();

    for sid in sprint_ids {
        let project_id: Option<i64> = conn
            .query_row("SELECT project_id FROM sprints WHERE id = ?", [*sid], |r| {
                r.get(0)
            })
            .ok();
        let project_id = match project_id {
            Some(p) => p,
            None => continue,
        };

        let mut stmt = conn.prepare(
            "SELECT DISTINCT method_fingerprint, file_path, method_name
             FROM fingerprints
             WHERE sprint_id = ? AND method_fingerprint IS NOT NULL
                   AND method_fingerprint != ''",
        )?;
        let rows = stmt.query_map([*sid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?;
        for r in rows {
            let (fp, file_path, method_name) = r?;
            if boilerplate.contains(&fp) {
                continue;
            }
            fp_to_locations
                .entry(fp)
                .or_default()
                .push((project_id, file_path, method_name, *sid));
        }
    }

    let mut match_count: i64 = 0;
    for (fp, locations) in fp_to_locations {
        // Distinct project count.
        let projects: HashSet<i64> = locations.iter().map(|l| l.0).collect();
        if projects.len() < 2 {
            continue;
        }
        // Group by project, preserving insertion order within each project.
        let mut by_project: BTreeMap<i64, Vec<(String, String, i64)>> = BTreeMap::new();
        for (pid, file_path, method_name, sid) in &locations {
            by_project.entry(*pid).or_default().push((
                file_path.clone(),
                method_name.clone(),
                *sid,
            ));
        }
        let project_ids: Vec<i64> = by_project.keys().copied().collect();
        for i in 0..project_ids.len() {
            for j in (i + 1)..project_ids.len() {
                let pid_a = project_ids[i];
                let pid_b = project_ids[j];
                let rep_a = &by_project[&pid_a][0];
                let rep_b = &by_project[&pid_b][0];
                let sid = rep_a.2;
                let method_name = if !rep_a.1.is_empty() {
                    &rep_a.1
                } else {
                    &rep_b.1
                };
                conn.execute(
                    "INSERT INTO cross_team_matches
                     (sprint_id, team_a_project_id, team_b_project_id,
                      file_path_a, file_path_b, method_name, fingerprint)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![sid, pid_a, pid_b, rep_a.0, rep_b.0, method_name, fp,],
                )?;
                match_count += 1;
            }
        }
    }

    info!(match_count, "Found cross-team method matches");
    Ok(())
}
