//! W2.T5 verification harness — render one project's report against
//! `data/entregues/grading.db` so the pre/post-refactor outputs can be
//! diffed.
//!
//! Usage:
//!
//! ```sh
//! DB=/tmp/pre-W2T5.db PROJECT=pds26-4c OUT=/tmp/pre.md \
//!     cargo run -p sprint-grader-report --example render_pds26
//! DB=data/entregues/grading.db PROJECT=pds26-4c OUT=/tmp/post.md \
//!     cargo run -p sprint-grader-report --example render_pds26
//! diff -u /tmp/pre.md /tmp/post.md
//! ```

use rusqlite::Connection;
use sprint_grader_report::{generate_markdown_report_multi_to_path_with_opts, MultiReportOptions};
use std::path::Path;

fn main() -> rusqlite::Result<()> {
    let db = std::env::var("DB").unwrap_or_else(|_| "data/entregues/grading.db".to_string());
    let project_slug = std::env::var("PROJECT").unwrap_or_else(|_| "pds26-4c".to_string());
    let out = std::env::var("OUT").unwrap_or_else(|_| "/tmp/report.md".to_string());

    let conn = Connection::open(&db)?;
    let (project_id, project_name): (i64, String) = conn.query_row(
        "SELECT id, name FROM projects WHERE name = ? OR slug = ?",
        [&project_slug, &project_slug],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut stmt =
        conn.prepare("SELECT id FROM sprints WHERE project_id = ? ORDER BY start_date")?;
    let sprint_ids: Vec<i64> = stmt
        .query_map([project_id], |r| r.get::<_, i64>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let opts = MultiReportOptions::instructor();
    generate_markdown_report_multi_to_path_with_opts(
        &conn,
        project_id,
        &project_name,
        &sprint_ids,
        Path::new(&out),
        opts,
    )?;
    eprintln!(
        "wrote {out}: project_id={project_id}, sprints={:?}",
        sprint_ids
    );
    Ok(())
}
