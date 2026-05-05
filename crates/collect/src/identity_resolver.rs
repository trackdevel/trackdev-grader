//! Compute the trackdev_id ↔ github (login, email) mapping from per-PR
//! task-assignee evidence.
//!
//! For each PR `p` with linked tasks, each linked-task assignee `s` receives
//! weight `count(s in M(p)) / |M(p)|` where `M(p)` is the multiset of
//! assignees across `tasks(p)`. That assignee weight is multiplied by a
//! source weight (commits = 1.0, pre-squash = 1.0, PR submitter = 0.5) and
//! distributed across each github identity (login, email) seen in that PR.
//!
//! After sweeping every PR with `tasks(p) ≠ ∅` (orphan PRs contribute zero
//! evidence by design — they have no TrackDev authors), each identity
//! `(kind, value)` is accepted iff
//!   confidence = acc[(s*, kind, value)] / Σ_s acc[(s, kind, value)] ≥ τ
//! with τ = 0.7 by default. Rejected identities are written to
//! `identity_resolution_warnings` with kind `AMBIGUOUS_IDENTITY`.
//!
//! The mapping is rebuilt from scratch on each call (DELETE then INSERT).
//! Cross-sprint accumulation is implicit — every PR ever collected feeds
//! the same accumulator.

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::json;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy)]
pub struct IdentityResolverConfig {
    pub commit_weight: f64,
    pub pre_squash_weight: f64,
    pub submitter_weight: f64,
    pub confidence_threshold: f64,
}

impl Default for IdentityResolverConfig {
    fn default() -> Self {
        Self {
            commit_weight: 1.0,
            pre_squash_weight: 1.0,
            submitter_weight: 0.5,
            confidence_threshold: 0.7,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct IdentityResolverStats {
    pub identities_seen: usize,
    pub mappings_accepted: usize,
    pub ambiguous_logged: usize,
    pub prs_with_evidence: usize,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct IdentityKey {
    kind: &'static str, // "login" | "email"
    value: String,
}

struct PrSeen {
    first: String,
    last: String,
}

/// Run the resolver and persist the results. Always rebuilds the mapping
/// from scratch (cross-sprint accumulator).
pub fn resolve_identities(
    conn: &Connection,
    cfg: &IdentityResolverConfig,
) -> rusqlite::Result<IdentityResolverStats> {
    // 1. Build per-PR assignee weights from task_pull_requests.
    let mut pr_weights: HashMap<String, HashMap<String, f64>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT tpr.pr_id, t.assignee_id
             FROM task_pull_requests tpr
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.assignee_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut counts: HashMap<String, HashMap<String, u32>> = HashMap::new();
        for row in rows {
            let (pr_id, sid) = row?;
            *counts.entry(pr_id).or_default().entry(sid).or_insert(0) += 1;
        }
        for (pr_id, m) in counts {
            let total: u32 = m.values().copied().sum();
            if total == 0 {
                continue;
            }
            let normalised: HashMap<String, f64> = m
                .into_iter()
                .map(|(sid, c)| (sid, c as f64 / total as f64))
                .collect();
            pr_weights.insert(pr_id, normalised);
        }
    }

    // 2. Accumulator: (student_id, identity) → weight, plus first/last seen PR.
    let mut acc: HashMap<(String, IdentityKey), f64> = HashMap::new();
    let mut seen_pr: HashMap<IdentityKey, PrSeen> = HashMap::new();
    let mut prs_with_evidence: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    let push = |acc: &mut HashMap<(String, IdentityKey), f64>,
                seen: &mut HashMap<IdentityKey, PrSeen>,
                evidence_prs: &mut std::collections::HashSet<String>,
                pr_id: &str,
                weights: &HashMap<String, f64>,
                kind: &'static str,
                value_raw: &str,
                src_weight: f64| {
        let value = value_raw.trim().to_lowercase();
        if value.is_empty() {
            return;
        }
        let key = IdentityKey { kind, value };
        for (sid, w) in weights.iter() {
            let entry = acc
                .entry((sid.clone(), key.clone()))
                .or_insert(0.0);
            *entry += w * src_weight;
        }
        evidence_prs.insert(pr_id.to_string());
        seen.entry(key)
            .and_modify(|s| s.last = pr_id.to_string())
            .or_insert(PrSeen {
                first: pr_id.to_string(),
                last: pr_id.to_string(),
            });
    };

    // 3a. pr_commits — per-commit weight = commit_weight.
    {
        let mut stmt = conn.prepare(
            "SELECT pr_id, author_login, author_email FROM pr_commits",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (pr_id, login, email) = row?;
            let Some(weights) = pr_weights.get(&pr_id) else {
                continue;
            };
            if let Some(l) = login.as_deref() {
                push(
                    &mut acc,
                    &mut seen_pr,
                    &mut prs_with_evidence,
                    &pr_id,
                    weights,
                    "login",
                    l,
                    cfg.commit_weight,
                );
            }
            if let Some(e) = email.as_deref() {
                push(
                    &mut acc,
                    &mut seen_pr,
                    &mut prs_with_evidence,
                    &pr_id,
                    weights,
                    "email",
                    e,
                    cfg.commit_weight,
                );
            }
        }
    }

    // 3b. pr_pre_squash_authors — pre_squash_weight.
    {
        let mut stmt = conn.prepare(
            "SELECT pr_id, author_login, author_email FROM pr_pre_squash_authors",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (pr_id, login, email) = row?;
            let Some(weights) = pr_weights.get(&pr_id) else {
                continue;
            };
            if let Some(l) = login.as_deref() {
                push(
                    &mut acc,
                    &mut seen_pr,
                    &mut prs_with_evidence,
                    &pr_id,
                    weights,
                    "login",
                    l,
                    cfg.pre_squash_weight,
                );
            }
            if let Some(e) = email.as_deref() {
                push(
                    &mut acc,
                    &mut seen_pr,
                    &mut prs_with_evidence,
                    &pr_id,
                    weights,
                    "email",
                    e,
                    cfg.pre_squash_weight,
                );
            }
        }
    }

    // 3c. pull_requests submitter — submitter_weight.
    {
        let mut stmt = conn.prepare(
            "SELECT id, github_author_login, github_author_email FROM pull_requests",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (pr_id, login, email) = row?;
            let Some(weights) = pr_weights.get(&pr_id) else {
                continue;
            };
            if let Some(l) = login.as_deref() {
                push(
                    &mut acc,
                    &mut seen_pr,
                    &mut prs_with_evidence,
                    &pr_id,
                    weights,
                    "login",
                    l,
                    cfg.submitter_weight,
                );
            }
            if let Some(e) = email.as_deref() {
                push(
                    &mut acc,
                    &mut seen_pr,
                    &mut prs_with_evidence,
                    &pr_id,
                    weights,
                    "email",
                    e,
                    cfg.submitter_weight,
                );
            }
        }
    }

    // 4. Group accumulator by identity to find argmax + confidence.
    let mut by_identity: HashMap<IdentityKey, Vec<(String, f64)>> = HashMap::new();
    for ((sid, key), w) in acc.into_iter() {
        by_identity.entry(key).or_default().push((sid, w));
    }

    let now = Utc::now().to_rfc3339();
    conn.execute("DELETE FROM student_github_identity", [])?;
    conn.execute("DELETE FROM identity_resolution_warnings", [])?;

    let mut stats = IdentityResolverStats {
        identities_seen: by_identity.len(),
        mappings_accepted: 0,
        ambiguous_logged: 0,
        prs_with_evidence: prs_with_evidence.len(),
    };

    for (key, mut candidates) in by_identity {
        candidates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let total: f64 = candidates.iter().map(|c| c.1).sum();
        if total <= 0.0 {
            continue;
        }
        let (top_sid, top_w) = &candidates[0];
        let confidence = top_w / total;
        let pr_seen = seen_pr.get(&key);
        if confidence >= cfg.confidence_threshold {
            conn.execute(
                "INSERT INTO student_github_identity
                    (student_id, identity_kind, identity_value, weight, confidence,
                     first_seen_pr, last_seen_pr)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    top_sid,
                    key.kind,
                    key.value,
                    top_w,
                    confidence,
                    pr_seen.map(|p| p.first.as_str()),
                    pr_seen.map(|p| p.last.as_str()),
                ],
            )?;
            stats.mappings_accepted += 1;
        } else {
            let cands_json = json!(candidates
                .iter()
                .map(|(sid, w)| json!({
                    "student_id": sid,
                    "weight": w,
                    "share": w / total,
                }))
                .collect::<Vec<_>>());
            conn.execute(
                "INSERT INTO identity_resolution_warnings
                    (identity_kind, identity_value, kind, candidates, observed_at)
                 VALUES (?, ?, 'AMBIGUOUS_IDENTITY', ?, ?)",
                params![key.kind, key.value, cands_json.to_string(), now,],
            )?;
            stats.ambiguous_logged += 1;
        }
    }

    if stats.identities_seen == 0 {
        warn!(
            "identity resolver: no PRs with linked tasks found — mapping is empty"
        );
    } else {
        info!(
            identities = stats.identities_seen,
            accepted = stats.mappings_accepted,
            ambiguous = stats.ambiguous_logged,
            prs = stats.prs_with_evidence,
            "identity resolver complete"
        );
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_grader_core::Database;

    fn open_seeded() -> Database {
        let db = Database::open(std::path::Path::new(":memory:")).expect("db");
        db.create_tables().expect("schema");
        db
    }

    fn seed_minimal(db: &Database) {
        db.conn
            .execute("INSERT INTO projects (id, slug, name) VALUES (1, 'p', 'p')", [])
            .unwrap();
        for sid in ["s-alice", "s-bob", "s-carol"] {
            db.conn
                .execute(
                    "INSERT INTO students (id, full_name, team_project_id) VALUES (?, ?, 1)",
                    params![sid, sid],
                )
                .unwrap();
        }
    }

    #[test]
    fn single_assignee_pr_with_consistent_commits_is_accepted() {
        let db = open_seeded();
        seed_minimal(&db);
        // task → alice; PR → that task; commits all by alice@example.com / login alice
        db.conn
            .execute(
                "INSERT INTO tasks (id, type, status, assignee_id) VALUES (10, 'TASK', 'DONE', 's-alice')",
                [],
            )
            .unwrap();
        db.conn
            .execute("INSERT INTO pull_requests (id) VALUES ('pr-1')", [])
            .unwrap();
        db.conn
            .execute("INSERT INTO task_pull_requests (task_id, pr_id) VALUES (10, 'pr-1')", [])
            .unwrap();
        db.conn.execute(
            "INSERT INTO pr_commits (pr_id, sha, author_login, author_email) VALUES ('pr-1', 'a1', 'alice', 'alice@example.com')",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO pr_commits (pr_id, sha, author_login, author_email) VALUES ('pr-1', 'a2', 'alice', 'alice@example.com')",
            [],
        ).unwrap();

        let stats = resolve_identities(&db.conn, &IdentityResolverConfig::default()).unwrap();
        assert_eq!(stats.mappings_accepted, 2); // login + email
        assert_eq!(stats.ambiguous_logged, 0);

        let mapped: String = db
            .conn
            .query_row(
                "SELECT student_id FROM student_github_identity
                 WHERE identity_kind='email' AND identity_value='alice@example.com'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mapped, "s-alice");
    }

    #[test]
    fn split_evenly_across_two_assignees_logs_ambiguous() {
        let db = open_seeded();
        seed_minimal(&db);
        // PR closes one alice-task and one bob-task, weights 0.5/0.5;
        // single commit by some shared mystery@laptop, threshold 0.7.
        db.conn
            .execute(
                "INSERT INTO tasks (id, type, status, assignee_id) VALUES (10, 'TASK', 'DONE', 's-alice')",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO tasks (id, type, status, assignee_id) VALUES (11, 'TASK', 'DONE', 's-bob')",
                [],
            )
            .unwrap();
        db.conn
            .execute("INSERT INTO pull_requests (id) VALUES ('pr-2')", [])
            .unwrap();
        db.conn
            .execute("INSERT INTO task_pull_requests (task_id, pr_id) VALUES (10, 'pr-2')", [])
            .unwrap();
        db.conn
            .execute("INSERT INTO task_pull_requests (task_id, pr_id) VALUES (11, 'pr-2')", [])
            .unwrap();
        db.conn.execute(
            "INSERT INTO pr_commits (pr_id, sha, author_login, author_email) VALUES ('pr-2', 'b1', 'mystery', 'mystery@laptop')",
            [],
        ).unwrap();

        let stats = resolve_identities(&db.conn, &IdentityResolverConfig::default()).unwrap();
        assert_eq!(stats.mappings_accepted, 0);
        assert_eq!(stats.ambiguous_logged, 2); // login + email both ambiguous

        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM identity_resolution_warnings WHERE kind='AMBIGUOUS_IDENTITY'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn evidence_accumulates_across_prs_to_break_a_one_off_tie() {
        let db = open_seeded();
        seed_minimal(&db);
        // PR-A: alice solo with commit 'shared@laptop' (clean signal).
        // PR-B: alice + bob co-tasked with one commit by 'shared@laptop'
        //       (alice gets 0.5, bob gets 0.5 from this PR).
        // After PR-A (1.0 weight) + PR-B (0.5 each), shared@laptop maps to alice
        // with 1.5 / 2.0 = 0.75 confidence (≥ τ=0.7).
        db.conn
            .execute("INSERT INTO tasks (id,type,status,assignee_id) VALUES (1,'TASK','DONE','s-alice')", [])
            .unwrap();
        db.conn
            .execute("INSERT INTO tasks (id,type,status,assignee_id) VALUES (2,'TASK','DONE','s-alice')", [])
            .unwrap();
        db.conn
            .execute("INSERT INTO tasks (id,type,status,assignee_id) VALUES (3,'TASK','DONE','s-bob')", [])
            .unwrap();
        for prid in ["pr-A", "pr-B"] {
            db.conn
                .execute("INSERT INTO pull_requests (id) VALUES (?)", params![prid])
                .unwrap();
        }
        db.conn
            .execute("INSERT INTO task_pull_requests (task_id,pr_id) VALUES (1,'pr-A')", [])
            .unwrap();
        db.conn
            .execute("INSERT INTO task_pull_requests (task_id,pr_id) VALUES (2,'pr-B')", [])
            .unwrap();
        db.conn
            .execute("INSERT INTO task_pull_requests (task_id,pr_id) VALUES (3,'pr-B')", [])
            .unwrap();
        db.conn.execute(
            "INSERT INTO pr_commits (pr_id,sha,author_login,author_email) VALUES ('pr-A','x1',NULL,'shared@laptop')",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO pr_commits (pr_id,sha,author_login,author_email) VALUES ('pr-B','x2',NULL,'shared@laptop')",
            [],
        ).unwrap();

        let stats = resolve_identities(&db.conn, &IdentityResolverConfig::default()).unwrap();
        assert_eq!(stats.mappings_accepted, 1);
        assert_eq!(stats.ambiguous_logged, 0);

        let mapped: String = db
            .conn
            .query_row(
                "SELECT student_id FROM student_github_identity
                 WHERE identity_kind='email' AND identity_value='shared@laptop'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mapped, "s-alice");
    }

    #[test]
    fn orphan_pr_contributes_zero_evidence() {
        let db = open_seeded();
        seed_minimal(&db);
        db.conn
            .execute("INSERT INTO pull_requests (id) VALUES ('pr-orphan')", [])
            .unwrap();
        db.conn.execute(
            "INSERT INTO pr_commits (pr_id,sha,author_login,author_email) VALUES ('pr-orphan','o1','someone','someone@x')",
            [],
        ).unwrap();

        let stats = resolve_identities(&db.conn, &IdentityResolverConfig::default()).unwrap();
        assert_eq!(stats.mappings_accepted, 0);
        assert_eq!(stats.ambiguous_logged, 0);
        assert_eq!(stats.identities_seen, 0);
    }
}
