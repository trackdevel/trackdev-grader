//! PR submission temporal categorization.
//! Mirrors `src/repo_analysis/temporal_analysis.py`.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use sprint_grader_core::config::RepoAnalysisConfig;
use sprint_grader_core::stats::round_half_even;
use sprint_grader_core::time::parse_iso;
use tracing::{info, warn};

use crate::keywords::is_fix_title;

pub const TIER_REGULAR: &str = "Regular";
pub const TIER_LATE: &str = "Late";
pub const TIER_CRITICAL: &str = "Critical";
pub const TIER_FIX: &str = "Fix";
pub const ALL_TIERS: &[&str] = &[TIER_REGULAR, TIER_LATE, TIER_CRITICAL, TIER_FIX];

pub const PR_KIND_FEATURE: &str = "feature";

type PrTemporalRow = (String, Option<String>, Option<String>, Option<String>);
pub const PR_KIND_FIX: &str = "fix";

pub fn classify_pr_kind(title: Option<&str>, linked_task_types: &[String]) -> &'static str {
    if is_fix_title(title) {
        return PR_KIND_FIX;
    }
    if linked_task_types.iter().any(|t| t == "BUG") {
        return PR_KIND_FIX;
    }
    PR_KIND_FEATURE
}

pub fn classify_tier(hours_before: f64, pr_kind: &str, cfg: &RepoAnalysisConfig) -> &'static str {
    if hours_before >= cfg.temporal_early_hours {
        return TIER_REGULAR;
    }
    if hours_before >= cfg.temporal_moderate_hours {
        return TIER_LATE;
    }
    if hours_before >= cfg.temporal_late_hours {
        return TIER_CRITICAL;
    }
    if pr_kind == PR_KIND_FIX {
        TIER_FIX
    } else {
        // Non-fix PRs submitted inside the final 48h are even riskier than
        // the 48-72h window; keep them in the red critical bucket rather than
        // inventing a fifth chart category.
        TIER_CRITICAL
    }
}

fn purge_sprint(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM pr_submission_tiers WHERE sprint_id = ?",
        [sprint_id],
    )?;
    Ok(())
}

pub struct TemporalSummary {
    pub skipped: bool,
    pub pr_count: usize,
    pub tier_counts: HashMap<&'static str, usize>,
    pub kind_counts: HashMap<&'static str, usize>,
}

fn empty_counts() -> (HashMap<&'static str, usize>, HashMap<&'static str, usize>) {
    let mut tiers = HashMap::new();
    for t in ALL_TIERS {
        tiers.insert(*t, 0);
    }
    let mut kinds = HashMap::new();
    kinds.insert(PR_KIND_FEATURE, 0);
    kinds.insert(PR_KIND_FIX, 0);
    (tiers, kinds)
}

pub fn compute_temporal_analysis(
    conn: &Connection,
    sprint_id: i64,
    cfg: &RepoAnalysisConfig,
) -> rusqlite::Result<TemporalSummary> {
    let (mut tier_counts, mut kind_counts) = empty_counts();

    if !cfg.enable_temporal_analysis {
        return Ok(TemporalSummary {
            skipped: true,
            pr_count: 0,
            tier_counts,
            kind_counts,
        });
    }

    let end_date: Option<String> = conn
        .query_row(
            "SELECT end_date FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let deadline = match end_date.as_deref().and_then(parse_iso) {
        Some(d) => d,
        None => {
            warn!(
                sprint_id,
                "no parseable end_date — skipping temporal analysis"
            );
            return Ok(TemporalSummary {
                skipped: true,
                pr_count: 0,
                tier_counts,
                kind_counts,
            });
        }
    };

    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.id, pr.merged_at, pr.created_at, pr.title
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ?
           AND t.type != 'USER_STORY'
           AND pr.merged = 1
           AND pr.merged_at IS NOT NULL",
    )?;
    let rows: Vec<PrTemporalRow> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut pr_task_types: HashMap<String, Vec<String>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT tpr.pr_id, t.type
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'",
    )?;
    for row in stmt
        .query_map([sprint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        pr_task_types.entry(row.0).or_default().push(row.1);
    }
    drop(stmt);

    purge_sprint(conn, sprint_id)?;

    let mut written = 0;
    for (pr_id, merged_at, created_at, title) in rows {
        let opened = created_at
            .as_deref()
            .and_then(parse_iso)
            .or_else(|| merged_at.as_deref().and_then(parse_iso));
        let Some(opened) = opened else { continue };
        let hours_before = (deadline - opened).num_seconds() as f64 / 3600.0;
        let types = pr_task_types.get(&pr_id).cloned().unwrap_or_default();
        let kind = classify_pr_kind(title.as_deref(), &types);
        let tier = classify_tier(hours_before, kind, cfg);
        let rounded = round_half_even(hours_before, 2);

        conn.execute(
            "INSERT OR REPLACE INTO pr_submission_tiers
             (sprint_id, pr_id, merged_at, hours_before_deadline, tier, pr_kind)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![sprint_id, pr_id, merged_at, rounded, tier, kind],
        )?;
        *tier_counts.entry(tier).or_insert(0) += 1;
        *kind_counts.entry(kind).or_insert(0) += 1;
        written += 1;
    }

    info!(sprint_id, pr_count = written, "temporal analysis");
    Ok(TemporalSummary {
        skipped: false,
        pr_count: written,
        tier_counts,
        kind_counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> RepoAnalysisConfig {
        RepoAnalysisConfig::default()
    }

    #[test]
    fn tier_thresholds_are_inclusive_on_upper_bound() {
        let cfg = default_cfg();
        assert_eq!(classify_tier(96.0, PR_KIND_FEATURE, &cfg), TIER_REGULAR);
        assert_eq!(classify_tier(95.9, PR_KIND_FEATURE, &cfg), TIER_LATE);
        assert_eq!(classify_tier(72.0, PR_KIND_FEATURE, &cfg), TIER_LATE);
        assert_eq!(classify_tier(71.9, PR_KIND_FEATURE, &cfg), TIER_CRITICAL);
        assert_eq!(classify_tier(48.0, PR_KIND_FEATURE, &cfg), TIER_CRITICAL);
        assert_eq!(classify_tier(47.9, PR_KIND_FEATURE, &cfg), TIER_CRITICAL);
        assert_eq!(classify_tier(47.9, PR_KIND_FIX, &cfg), TIER_FIX);
    }

    #[test]
    fn kind_fix_from_title_or_task_type() {
        assert_eq!(classify_pr_kind(Some("Fix login bug"), &[]), PR_KIND_FIX);
        assert_eq!(
            classify_pr_kind(Some("Add feature"), &["BUG".into()]),
            PR_KIND_FIX
        );
        assert_eq!(
            classify_pr_kind(Some("Add login"), &["TASK".into()]),
            PR_KIND_FEATURE
        );
    }
}
