//! Smoke test for `core::rule_attribution::load_attributed_findings_for_repo`
//! against the live grading database in `data/entregues/grading.db`.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p sprint-grader-core --example smoke_rule_attribution
//! ```
//!
//! Prints the count of attributed findings per RuleKind for one repo,
//! along with a few cross-checks against SQL counts so divergence
//! between the new loader and the existing SELECTs surfaces immediately.

use rusqlite::Connection;
use sprint_grader_core::finding::RuleKind;
use sprint_grader_core::rule_attribution::load_attributed_findings_for_repo;

fn main() -> rusqlite::Result<()> {
    let path =
        std::env::var("GRADING_DB").unwrap_or_else(|_| "data/entregues/grading.db".to_string());
    let repo = std::env::var("REPO").unwrap_or_else(|_| "spring-pds26_1b".to_string());

    let conn = Connection::open(&path)?;
    println!("== smoke_rule_attribution ==");
    println!("DB: {path}");
    println!("Repo: {repo}\n");

    for kind in [
        RuleKind::Architecture,
        RuleKind::Complexity,
        RuleKind::StaticAnalysis,
    ] {
        let afs = load_attributed_findings_for_repo(&conn, &repo, kind)?;
        let attributed = afs.iter().filter(|a| !a.attributions.is_empty()).count();
        let unattributed = afs.iter().filter(|a| a.attributions.is_empty()).count();
        let total_attribution_rows: usize = afs.iter().map(|a| a.attributions.len()).sum();
        let absolute_paths = afs
            .iter()
            .filter(|a| a.finding.file_repo_relative.starts_with('/'))
            .count();
        println!(
            "{kind}: findings={n}, attributed={attributed}, unattributed={unattributed}, \
             total_attribution_rows={total_attribution_rows}, absolute_paths={absolute_paths}",
            n = afs.len(),
        );
        // Print one example row to eyeball the conversion shape.
        if let Some(af) = afs.first() {
            println!(
                "  first: rule_id={}, severity={}, file={}, span={:?}, evidence_len={}, \
                 extra={:?}, attributions={}",
                af.finding.rule_id,
                af.finding.severity,
                af.finding.file_repo_relative,
                af.finding.span,
                af.finding.evidence.len(),
                af.finding.extra,
                af.attributions.len()
            );
        }
    }
    Ok(())
}
