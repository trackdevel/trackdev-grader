//! Map team-grain raw quality metrics to 0–10 sub-scores.

use sprint_grader_core::finding::{RuleKind, Severity};
use sprint_grader_core::rule_attribution::load_attributed_findings_for_repo;

use rusqlite::{params, Connection};

use crate::config::{GradingConfig, NormalizationConfig};

#[derive(Debug, Clone, PartialEq)]
pub struct AxisRaw {
    pub raw_value: Option<f64>,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AxisScore {
    pub key: &'static str,
    pub raw_value: Option<f64>,
    pub score_0_10: Option<f64>,
    pub present: bool,
}

pub fn clamp_0_10(x: f64) -> f64 {
    x.clamp(0.0, 10.0)
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Team mean of `pr_doc_evaluation.total_doc_score` over in-scope sprints.
pub fn documentation_raw(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<AxisRaw> {
    if sprint_ids.is_empty() {
        return Ok(AxisRaw {
            raw_value: None,
            present: false,
        });
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT AVG(pde.total_doc_score)
         FROM pr_doc_evaluation pde
         JOIN pull_requests pr ON pr.id = pde.pr_id
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         WHERE s.team_project_id = ?
           AND pde.sprint_id IN ({placeholders})
           AND pde.total_doc_score IS NOT NULL"
    );
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let avg: Option<f64> =
        conn.query_row(&sql, rusqlite::params_from_iter(params), |r| r.get(0))?;
    Ok(match avg {
        Some(v) => AxisRaw {
            raw_value: Some(v),
            present: true,
        },
        None => AxisRaw {
            raw_value: None,
            present: false,
        },
    })
}

pub fn score_documentation(raw: &AxisRaw, norm: &NormalizationConfig) -> AxisScore {
    let score = raw
        .raw_value
        .map(|doc_raw| 10.0 * clamp01(doc_raw / norm.doc_max));
    AxisScore {
        key: "documentation",
        raw_value: raw.raw_value,
        score_0_10: if raw.present { score } else { None },
        present: raw.present,
    }
}

/// Team mean of maintainability with cc penalty and mutation test bonus.
pub fn code_quality_raw(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<(AxisRaw, Option<f64>, Option<f64>)> {
    if sprint_ids.is_empty() {
        return Ok((
            AxisRaw {
                raw_value: None,
                present: false,
            },
            None,
            None,
        ));
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT AVG(ssq.avg_maintainability), AVG(ssq.pct_methods_cc_over_10)
         FROM student_sprint_quality ssq
         JOIN students s ON s.id = ssq.student_id
         WHERE s.team_project_id = ?
           AND ssq.sprint_id IN ({placeholders})
           AND ssq.avg_maintainability IS NOT NULL"
    );
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let (mi, cc): (Option<f64>, Option<f64>) =
        conn.query_row(&sql, rusqlite::params_from_iter(params), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;

    let mutation_sql = format!(
        "SELECT AVG(pm.mutation_score)
         FROM pr_mutation pm
         JOIN pull_requests pr ON pr.id = pm.pr_id
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         WHERE s.team_project_id = ?
           AND pm.sprint_id IN ({placeholders})
           AND pm.mutation_score IS NOT NULL"
    );
    let mut mparams: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        mparams.push((*sid).into());
    }
    let mutation: Option<f64> =
        conn.query_row(&mutation_sql, rusqlite::params_from_iter(mparams), |r| {
            r.get(0)
        })?;

    Ok((
        match mi {
            Some(v) => AxisRaw {
                raw_value: Some(v),
                present: true,
            },
            None => AxisRaw {
                raw_value: None,
                present: false,
            },
        },
        cc,
        mutation,
    ))
}

pub fn score_code_quality(
    raw: &AxisRaw,
    team_cc_pct: Option<f64>,
    mutation_score: Option<f64>,
    norm: &NormalizationConfig,
) -> AxisScore {
    let score = raw.raw_value.map(|mi| {
        let base = 10.0 * clamp01((mi - norm.mi_floor) / (norm.mi_ceiling - norm.mi_floor));
        let cc_adj = team_cc_pct
            .map(|pct| norm.cc_penalty * (pct / 100.0))
            .unwrap_or(0.0);
        let test_adj = mutation_score
            .map(|ms| (norm.test_bonus * ms).min(norm.test_cap))
            .unwrap_or(0.0);
        clamp_0_10(base - cc_adj + test_adj)
    });
    AxisScore {
        key: "code_quality",
        raw_value: raw.raw_value,
        score_0_10: if raw.present { score } else { None },
        present: raw.present,
    }
}

/// Points-weighted team mean of `survival_rate_normalized`.
pub fn survival_raw(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<AxisRaw> {
    if sprint_ids.is_empty() {
        return Ok(AxisRaw {
            raw_value: None,
            present: false,
        });
    }
    let placeholders = sprint_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT sss.survival_rate_normalized, sss.estimation_points_total
         FROM student_sprint_survival sss
         JOIN students s ON s.id = sss.student_id
         WHERE s.team_project_id = ?
           AND sss.sprint_id IN ({placeholders})
           AND sss.survival_rate_normalized IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<rusqlite::types::Value> = vec![project_id.into()];
    for sid in sprint_ids {
        params.push((*sid).into());
    }
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        Ok((r.get::<_, f64>(0)?, r.get::<_, Option<i64>>(1)?))
    })?;
    let mut weighted_sum = 0.0;
    let mut weight_total = 0.0;
    let mut any = false;
    for row in rows {
        let (rate, pts) = row?;
        any = true;
        let w = pts.map(|p| p as f64).unwrap_or(1.0).max(0.0);
        if w > 0.0 {
            weighted_sum += rate * w;
            weight_total += w;
        }
    }
    if !any || weight_total <= 0.0 {
        return Ok(AxisRaw {
            raw_value: None,
            present: false,
        });
    }
    Ok(AxisRaw {
        raw_value: Some(weighted_sum / weight_total),
        present: true,
    })
}

pub fn score_survival(raw: &AxisRaw, norm: &NormalizationConfig) -> AxisScore {
    let score = raw.raw_value.map(|surv| {
        10.0 * clamp01((surv - norm.surv_floor) / (norm.surv_ceiling - norm.surv_floor))
    });
    AxisScore {
        key: "survival",
        raw_value: raw.raw_value,
        score_0_10: if raw.present { score } else { None },
        present: raw.present,
    }
}

pub fn score_architecture(
    conn: &Connection,
    project_id: i64,
    norm: &NormalizationConfig,
) -> rusqlite::Result<AxisScore> {
    let repos = project_repos(conn, project_id)?;
    let mut crit = 0u32;
    let mut warn = 0u32;
    for repo in &repos {
        let findings = load_attributed_findings_for_repo(conn, repo, RuleKind::Architecture)?;
        for af in findings {
            match af.finding.severity {
                Severity::Critical => crit += 1,
                Severity::Warning => warn += 1,
                Severity::Info => {}
            }
        }
    }
    if repos.is_empty() {
        return Ok(AxisScore {
            key: "architecture",
            raw_value: None,
            score_0_10: None,
            present: false,
        });
    }
    let arch_density = (norm.k_crit * crit as f64 + norm.k_warn * warn as f64) / norm.arch_norm;
    let score = clamp_0_10(10.0 - arch_density.min(10.0));
    Ok(AxisScore {
        key: "architecture",
        raw_value: Some(arch_density),
        score_0_10: Some(score),
        present: true,
    })
}

pub fn project_repos(conn: &Connection, project_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT pr.repo_full_name
         FROM pull_requests pr
         JOIN pr_authors pa ON pa.pr_id = pr.id
         JOIN students s ON s.id = pa.student_id
         WHERE s.team_project_id = ? AND pr.repo_full_name IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Present-renormalized weighted mean of the four quality axes → `Q`.
pub fn quality_composite(components: &[AxisScore], cfg: &GradingConfig) -> Option<f64> {
    let w = &cfg.weights_project;
    let entries: [(&AxisScore, f64); 4] = [
        (
            components
                .iter()
                .find(|c| c.key == "documentation")
                .expect("documentation axis"),
            w.documentation,
        ),
        (
            components
                .iter()
                .find(|c| c.key == "code_quality")
                .expect("code_quality axis"),
            w.code_quality,
        ),
        (
            components
                .iter()
                .find(|c| c.key == "survival")
                .expect("survival axis"),
            w.survival,
        ),
        (
            components
                .iter()
                .find(|c| c.key == "architecture")
                .expect("architecture axis"),
            w.architecture,
        ),
    ];
    let mut sum_w = 0.0;
    let mut sum_ws = 0.0;
    for (axis, weight) in entries {
        if axis.present {
            if let Some(s) = axis.score_0_10 {
                sum_w += weight;
                sum_ws += weight * s;
            }
        }
    }
    if sum_w > 0.0 {
        Some(sum_ws / sum_w)
    } else {
        None
    }
}

pub fn load_quality_axes(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
    cfg: &GradingConfig,
) -> rusqlite::Result<Vec<AxisScore>> {
    let doc_raw = documentation_raw(conn, project_id, sprint_ids)?;
    let doc = score_documentation(&doc_raw, &cfg.normalization);

    let (cq_raw, cc, mutation) = code_quality_raw(conn, project_id, sprint_ids)?;
    let cq = score_code_quality(&cq_raw, cc, mutation, &cfg.normalization);

    let surv_raw = survival_raw(conn, project_id, sprint_ids)?;
    let surv = score_survival(&surv_raw, &cfg.normalization);

    let arch = score_architecture(conn, project_id, &cfg.normalization)?;

    Ok(vec![doc, cq, surv, arch])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NormalizationConfig;

    /// Operator-confirmed: linear cc deduction + capped mutation bonus on MI anchor.
    #[test]
    fn code_quality_score_pins_linear_cc_and_capped_test_bonus() {
        let norm = NormalizationConfig::default();
        let raw = AxisRaw {
            raw_value: Some(67.5), // midpoint of [50, 85] → base 5.0
            present: true,
        };
        let score = score_code_quality(&raw, Some(25.0), Some(0.8), &norm);
        // base 5.0 − cc_penalty(2.0)·0.25 + min(0.5, 1.0·0.8) = 5.0 − 0.5 + 0.5 = 5.0
        let s = score.score_0_10.unwrap();
        assert!((s - 5.0).abs() < 1e-9, "got {s}");
    }
}
