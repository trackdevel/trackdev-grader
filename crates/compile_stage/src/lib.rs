//! Stage 1.5 — PR compilation testing.
//!
//! For each merged PR: create a temporary git worktree at the PR's merge SHA,
//! run the matching build profile's command with a hard process-kill timeout,
//! and record pass/fail + truncated stderr in the `pr_compilation` table.

pub mod builder;
pub mod failure_analysis;
pub mod pitest;

pub use builder::{
    check_compilations_parallel, check_sprint_compilations_parallel,
    load_build_profiles_from_config, match_profile, BuildProfileRe, BuildResult,
};
pub use failure_analysis::{classify_errors, summarize_compilation};
pub use pitest::{parse_pitest_xml, parse_pitest_xml_str, PitestSummary};
