//! Structural inventory scan (Grading v2 Wave 1).
//!
//! Walks production Java sources and persists breadth + depth counters
//! used later for project size/complexity axes. Observability only in
//! Wave 1 — no grade formula changes.

mod inventory;

pub mod baseline;
pub mod catalog;
pub mod detect;
pub mod gradle;
pub mod metrics;

use std::path::Path;
use std::time::Instant;

use rusqlite::{params, Connection};
use sprint_grader_architecture::scanner::{scan_repo, ScannedFile};
use tracing::{info, warn};

pub use baseline::{InventoryBaseline, StackBaseline};
pub use catalog::{Stack, TechnologyCatalog, TechnologyEntry};
pub use detect::{detect_depth, DepthScan, FeatureFinding};
pub use gradle::scan_gradle_coords;
pub use inventory::{is_production_main_source, scan_files};
pub use metrics::ALL_KEYS;

const STATUS_OK: &str = "OK";
const STATUS_SKIPPED_HEAD_UNCHANGED: &str = "SKIPPED_HEAD_UNCHANGED";
const STATUS_SKIPPED_NO_SOURCES: &str = "SKIPPED_NO_SOURCES";
const STATUS_CRASHED: &str = "CRASHED";

/// Bumped whenever the set of emitted metric keys or detector semantics change,
/// so the HEAD-SHA cache is invalidated automatically (a repo whose `main` has
/// not moved is still re-scanned when this differs from the recorded run). Bump
/// this instead of relying on a one-time `force` from the pipeline.
const SCANNER_VERSION: &str = "extra_tech_v1";

/// Outcome of a single-repo inventory scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanSummary {
    pub metrics_written: usize,
    pub file_count: usize,
    pub skipped_unchanged: bool,
}

fn git_head_sha(repo_path: &Path) -> Option<String> {
    let path = repo_path.to_str()?;
    let out = std::process::Command::new("git")
        .args(["-C", path, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Last successful run's `(head_sha, scanner_version)` for cache invalidation.
fn cached_run(conn: &Connection, repo_full_name: &str) -> Option<(Option<String>, Option<String>)> {
    conn.query_row(
        "SELECT head_sha, scanner_version FROM project_inventory_runs
         WHERE repo_full_name = ? AND status = ?",
        params![repo_full_name, STATUS_OK],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        },
    )
    .ok()
}

struct RunRecord<'a> {
    repo_full_name: &'a str,
    project_id: i64,
    status: &'a str,
    metric_count: usize,
    file_count: usize,
    duration_ms: i64,
    head_sha: Option<&'a str>,
    diagnostics: Option<&'a str>,
}

fn record_run(conn: &Connection, run: RunRecord<'_>) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO project_inventory_runs
            (repo_full_name, project_id, status, metric_count, file_count,
             duration_ms, head_sha, diagnostics, scanned_at, scanner_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            run.repo_full_name,
            run.project_id,
            run.status,
            run.metric_count as i64,
            run.file_count as i64,
            run.duration_ms,
            run.head_sha,
            run.diagnostics,
            now,
            SCANNER_VERSION,
        ],
    )?;
    Ok(())
}

fn persist_metrics(
    conn: &Connection,
    repo_full_name: &str,
    head_sha: Option<&str>,
    metrics: &std::collections::BTreeMap<String, f64>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM repo_structural_metrics WHERE repo_full_name = ?",
        params![repo_full_name],
    )?;
    let mut written = 0usize;
    for (key, value) in metrics {
        conn.execute(
            "INSERT OR REPLACE INTO repo_structural_metrics
                (repo_full_name, metric_key, value, head_sha)
             VALUES (?, ?, ?, ?)",
            params![repo_full_name, key, value, head_sha],
        )?;
        written += 1;
    }
    Ok(written)
}

fn production_files(files: &[ScannedFile]) -> Vec<&ScannedFile> {
    files
        .iter()
        .filter(|f| inventory::is_production_main_source(&f.rel_path))
        .collect()
}

/// One itemized "extra technology" row for `repo_extra_technologies`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtraTechRow {
    pub technology: String,
    pub category: String,
    pub source: String,
    pub evidence: String,
    pub depth: f64,
}

const CURATED_CATEGORIES: &[&str] = &["fcm", "email", "av", "specifications", "graphics"];

/// Merge AST feature findings with new (non-baseline) Gradle coordinates into a
/// deduplicated `(technology, category)` set. A coordinate in a curated category
/// that already has an AST row upgrades that row's `source` to `both`; other new
/// coordinates become their own `gradle` rows (depth = number of coordinates).
fn build_extra_technologies(
    features: &[FeatureFinding],
    new_coords: &[String],
    catalog: &TechnologyCatalog,
) -> Vec<ExtraTechRow> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<(String, String), ExtraTechRow> = BTreeMap::new();
    for f in features {
        let key = (f.technology.clone(), f.category.clone());
        let row = map.entry(key).or_insert_with(|| ExtraTechRow {
            technology: f.technology.clone(),
            category: f.category.clone(),
            source: "ast".into(),
            evidence: f.evidence.clone(),
            depth: 0.0,
        });
        row.depth += f.depth;
        if row.evidence.is_empty() {
            row.evidence = f.evidence.clone();
        }
    }
    for coord in new_coords {
        let (name, cat) = catalog
            .classify(coord)
            .map(|(n, c)| (n.to_string(), c.to_string()))
            .unwrap_or_else(|| (coord.clone(), "dependency".to_string()));
        // A curated-category dependency corroborates an existing AST finding.
        if matches!(cat.as_str(), "fcm" | "email" | "av") {
            let mut corroborated = false;
            for row in map.values_mut() {
                if row.category == cat {
                    if row.source == "ast" {
                        row.source = "both".into();
                    }
                    corroborated = true;
                }
            }
            if corroborated {
                continue;
            }
        }
        let key = (name.clone(), cat.clone());
        let row = map.entry(key).or_insert_with(|| ExtraTechRow {
            technology: name,
            category: cat,
            source: "gradle".into(),
            evidence: coord.clone(),
            depth: 0.0,
        });
        row.depth += 1.0;
    }
    map.into_values().collect()
}

fn persist_extra_technologies(
    conn: &Connection,
    repo_full_name: &str,
    head_sha: Option<&str>,
    rows: &[ExtraTechRow],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM repo_extra_technologies WHERE repo_full_name = ?",
        params![repo_full_name],
    )?;
    for r in rows {
        conn.execute(
            "INSERT OR REPLACE INTO repo_extra_technologies
                (repo_full_name, technology, category, source, evidence, depth, head_sha)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                repo_full_name,
                r.technology,
                r.category,
                r.source,
                r.evidence,
                r.depth,
                head_sha
            ],
        )?;
    }
    Ok(())
}

/// Scan one cloned repo and persist structural metrics. Idempotent per
/// `(repo_full_name)`; skips when HEAD matches the last successful run
/// unless `force` is true (e.g. scanner added a new metric key).
pub fn scan_repo_to_db(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
    project_id: i64,
    catalog: &TechnologyCatalog,
    baseline: &InventoryBaseline,
    force: bool,
) -> rusqlite::Result<ScanSummary> {
    let started = Instant::now();
    let head = git_head_sha(repo_path);

    if !force {
        if let (Some(current), Some((cached_head, cached_ver))) =
            (head.as_deref(), cached_run(conn, repo_full_name))
        {
            if cached_head.as_deref() == Some(current)
                && cached_ver.as_deref() == Some(SCANNER_VERSION)
            {
                let kept: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM repo_structural_metrics WHERE repo_full_name = ?",
                        params![repo_full_name],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                record_run(
                    conn,
                    RunRecord {
                        repo_full_name,
                        project_id,
                        status: STATUS_SKIPPED_HEAD_UNCHANGED,
                        metric_count: kept as usize,
                        file_count: 0,
                        duration_ms: started.elapsed().as_millis() as i64,
                        head_sha: Some(current),
                        diagnostics: None,
                    },
                )?;
                info!(
                    repo = repo_full_name,
                    head = %current,
                    cached_metrics = kept,
                    "project inventory skipped (head unchanged)"
                );
                return Ok(ScanSummary {
                    metrics_written: 0,
                    file_count: 0,
                    skipped_unchanged: true,
                });
            }
        }
    }

    let files = scan_repo(repo_path);
    let prod = production_files(&files);
    if prod.is_empty() {
        record_run(
            conn,
            RunRecord {
                repo_full_name,
                project_id,
                status: STATUS_SKIPPED_NO_SOURCES,
                metric_count: 0,
                file_count: 0,
                duration_ms: started.elapsed().as_millis() as i64,
                head_sha: head.as_deref(),
                diagnostics: None,
            },
        )?;
        info!(
            repo = repo_full_name,
            "project inventory: no production sources"
        );
        return Ok(ScanSummary {
            metrics_written: 0,
            file_count: 0,
            skipped_unchanged: false,
        });
    }

    let stack = Stack::from_repo_name(repo_full_name);
    let stack_base = baseline.for_stack(stack);

    // Existing structural metrics (21 keys).
    let mut metrics = inventory::scan_files(&files);

    // EXTRA_TECH depth keys, baseline-subtracted (extra = max(0, student - base)).
    let depth = detect::detect_depth(&files, stack);
    for (k, v) in depth.metrics {
        let extra = (v - stack_base.feature(&k)).max(0.0);
        metrics.insert(k, extra);
    }

    // EXTRA_TECH breadth: new (non-baseline) Gradle coordinates. `extra_dependency_count`
    // counts only generic (non-curated) extras; curated coords feed the depth keys.
    let coords = gradle::scan_gradle_coords(repo_path);
    let base_deps = stack_base.dep_set();
    let new_coords: Vec<String> = coords
        .into_iter()
        .filter(|c| !base_deps.contains(c))
        .collect();
    let dep_count = new_coords
        .iter()
        .filter(|c| {
            catalog
                .classify(c)
                .map(|(_, cat)| !CURATED_CATEGORIES.contains(&cat))
                .unwrap_or(true)
        })
        .count();
    metrics.insert(metrics::EXTRA_DEPENDENCY_COUNT.into(), dep_count as f64);

    let written = persist_metrics(conn, repo_full_name, head.as_deref(), &metrics)?;

    let techs = build_extra_technologies(&depth.features, &new_coords, catalog);
    persist_extra_technologies(conn, repo_full_name, head.as_deref(), &techs)?;

    record_run(
        conn,
        RunRecord {
            repo_full_name,
            project_id,
            status: STATUS_OK,
            metric_count: written,
            file_count: prod.len(),
            duration_ms: started.elapsed().as_millis() as i64,
            head_sha: head.as_deref(),
            diagnostics: None,
        },
    )?;
    info!(
        repo = repo_full_name,
        files = prod.len(),
        metrics = written,
        extra_techs = techs.len(),
        "project inventory scan complete"
    );
    Ok(ScanSummary {
        metrics_written: written,
        file_count: prod.len(),
        skipped_unchanged: false,
    })
}

fn resolve_qualified_repo_name(conn: &Connection, bare: &str) -> Option<String> {
    let like = format!("%/{}", bare);
    conn.query_row(
        "SELECT repo_full_name FROM pull_requests
         WHERE repo_full_name = ? OR repo_full_name LIKE ?
         ORDER BY (repo_full_name = ?) DESC, length(repo_full_name) DESC
         LIMIT 1",
        params![bare, like, bare],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|s| s.contains('/'))
}

/// Scan every repo directory under `project_root`. `catalog` + `baseline` drive
/// the EXTRA_TECH Layer-A diff (load once via `TechnologyCatalog::load` /
/// `InventoryBaseline::load`; both degrade gracefully when their TOML is absent).
pub fn scan_project_to_db(
    conn: &Connection,
    project_root: &Path,
    project_id: i64,
    catalog: &TechnologyCatalog,
    baseline: &InventoryBaseline,
    force: bool,
) -> rusqlite::Result<usize> {
    if !project_root.is_dir() {
        warn!(
            path = %project_root.display(),
            "project inventory: project dir missing"
        );
        return Ok(0);
    }
    let mut total = 0usize;
    let entries = match std::fs::read_dir(project_root) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let repo_path = entry.path();
        let bare = entry.file_name().to_string_lossy().into_owned();
        let repo_full_name = resolve_qualified_repo_name(conn, &bare).unwrap_or(bare);
        match scan_repo_to_db(
            conn,
            &repo_path,
            &repo_full_name,
            project_id,
            catalog,
            baseline,
            force,
        ) {
            Ok(summary) => total += summary.metrics_written,
            Err(e) => {
                warn!(repo = %repo_full_name, error = %e, "project inventory scan failed");
                let msg = e.to_string();
                let _ = record_run(
                    conn,
                    RunRecord {
                        repo_full_name: &repo_full_name,
                        project_id,
                        status: STATUS_CRASHED,
                        metric_count: 0,
                        file_count: 0,
                        duration_ms: 0,
                        head_sha: git_head_sha(&repo_path).as_deref(),
                        diagnostics: Some(&msg),
                    },
                );
            }
        }
    }
    Ok(total)
}

/// Scan the two reference starter repos and produce a baseline manifest, for the
/// `inventory-baseline` CLI to (re)generate `config/inventory_baseline.toml`.
/// The starter is constant per cohort, so this is run rarely and checked in.
pub fn generate_baseline(android_repo: &Path, spring_repo: &Path) -> InventoryBaseline {
    InventoryBaseline {
        android: scan_stack_baseline(android_repo, Stack::Android),
        spring: scan_stack_baseline(spring_repo, Stack::Spring),
    }
}

fn scan_stack_baseline(repo_path: &Path, stack: Stack) -> StackBaseline {
    let files = scan_repo(repo_path);
    let depth = detect::detect_depth(&files, stack);
    let dependencies: Vec<String> = gradle::scan_gradle_coords(repo_path).into_iter().collect();
    // `extra_dependency_count` is a diff output, not a baseline level — drop it.
    let feature_metrics = depth
        .metrics
        .into_iter()
        .filter(|(k, _)| k != metrics::EXTRA_DEPENDENCY_COUNT)
        .collect();
    StackBaseline {
        dependencies,
        source_commit: git_head_sha(repo_path),
        feature_metrics,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use sprint_grader_core::db::apply_schema;
    use tempfile::TempDir;

    use super::*;

    fn git_init_commit(dir: &Path) {
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .expect("git");
        };
        run(&["init"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        fs::write(dir.join("marker.txt"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);
    }

    #[test]
    fn scan_repo_persists_metrics_and_skips_unchanged_head() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("spring-demo");
        fs::create_dir_all(repo.join("src/main/java/com/x")).unwrap();
        fs::write(
            repo.join("src/main/java/com/x/App.java"),
            "package com.x;\nimport org.springframework.web.bind.annotation.*;\n\
             @RestController\npublic class App {\n\
             @GetMapping(\"/h\")\npublic String h() { return \"ok\"; }\n}\n",
        )
        .unwrap();
        git_init_commit(&repo);

        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO projects (id, slug, name) VALUES (1, 'team-01', 'Team 01')",
            [],
        )
        .unwrap();

        let cat = TechnologyCatalog::default_catalog();
        let base = InventoryBaseline::default();
        let s1 = scan_repo_to_db(&conn, &repo, "org/spring-demo", 1, &cat, &base, false).unwrap();
        assert!(!s1.skipped_unchanged);
        assert_eq!(s1.metrics_written, ALL_KEYS.len());

        let endpoints: f64 = conn
            .query_row(
                "SELECT value FROM repo_structural_metrics
                 WHERE repo_full_name = ? AND metric_key = ?",
                params!["org/spring-demo", crate::metrics::ENDPOINT_COUNT],
                |r| r.get(0),
            )
            .unwrap();
        assert!((endpoints - 1.0).abs() < 1e-9);

        let s2 = scan_repo_to_db(&conn, &repo, "org/spring-demo", 1, &cat, &base, false).unwrap();
        assert!(s2.skipped_unchanged);
        assert_eq!(s2.metrics_written, 0);

        let s3 = scan_repo_to_db(&conn, &repo, "org/spring-demo", 1, &cat, &base, true).unwrap();
        assert!(!s3.skipped_unchanged);
        assert_eq!(s3.metrics_written, ALL_KEYS.len());
    }
}
