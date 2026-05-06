//! One-off helper: populate the new P2 derived tables for a project
//! whose `grading.db` predates those features. Lets us regenerate a
//! REPORT.md with the ownership treemap and the architecture
//! conformance subsection actually filled in.
//!
//! Usage:
//!   cargo run --release --example populate_p2 -- <db_path> <project_slug>

use std::path::PathBuf;

use rusqlite::Connection;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let db_path = PathBuf::from(args.next().expect("usage: populate_p2 <db> <project_slug>"));
    let project_slug = args.next().expect("usage: populate_p2 <db> <project_slug>");

    let conn = Connection::open(&db_path)?;
    let (project_id, project_name): (i64, String) = conn.query_row(
        "SELECT id, name FROM projects WHERE slug = ? OR name = ?",
        [&project_slug, &project_slug],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let sprint_ids: Vec<i64> = conn
        .prepare("SELECT id FROM sprints WHERE project_id = ? ORDER BY start_date")?
        .query_map([project_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    println!(
        "project {project_slug} (id={project_id}), {} sprints: {sprint_ids:?}",
        sprint_ids.len()
    );

    // Ownership (T-P2.3)
    for sid in &sprint_ids {
        let n = sprint_grader_repo_analysis::ownership::compute_team_ownership(&conn, *sid)?;
        println!("ownership sprint {sid}: wrote {n} project rows");
    }

    // Architecture conformance (T-P2.2). Walks the cloned repo dirs
    // under data/entregues/<slug>/ — a real implementation already
    // does this in the orchestration block; we replicate it here for
    // a one-off populate against an existing DB.
    let rules_path = std::path::Path::new("config/architecture.toml");
    if rules_path.is_file() {
        let rules = sprint_grader_architecture::ArchitectureRules::load(rules_path)?;
        let project_root = PathBuf::from("data/entregues").join(&project_name);
        // T-P3.4: artifact-shape — one scan per project per run; sprint_ids
        // are no longer involved.
        let _ = sprint_ids;
        match sprint_grader_architecture::scan_project_to_db(&conn, &project_root, &rules) {
            Ok(n) => println!("architecture: {n} violations"),
            Err(e) => eprintln!("architecture: {e}"),
        }
    } else {
        println!("architecture: config/architecture.toml not found, skipping");
    }

    Ok(())
}
