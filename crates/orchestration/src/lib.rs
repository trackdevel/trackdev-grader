//! Pipeline orchestration — `run-all`, `go`, `go-quick` variants, plus the
//! `purge-cache` / `debug-pr-lines` diagnostics. Mirrors the Python
//! `src/orchestration.py` + `src/parallel.py` pair, with `rayon` replacing
//! `ProcessPoolExecutor` for per-project parallelism.

pub mod db_diff;
pub mod debug_pr;
pub mod pipeline;
pub mod purge;
pub mod report_sync;

pub use db_diff::{
    checksum_table, diff_dbs, format_report, parse_ignore_cols, run_diff, DiffOptions, TableReport,
    TableStatus,
};
pub use debug_pr::debug_pr_lines;
pub use pipeline::{run_pipeline, PipelineVariant};
pub use purge::{
    ensure_clean_tree, purge_cache, purge_projects, CacheTargets, PurgeCacheResult, PurgeReport,
};
pub use report_sync::{
    android_repo_root, publish_report_updates, repo_has_report_changes,
    sync_reports_through_sprint, SyncReportsOptions, SyncReportsResult,
};
