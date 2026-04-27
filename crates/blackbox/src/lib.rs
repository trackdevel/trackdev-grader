//! Black-box / integration test infrastructure (T-T0.x).
//!
//! Three building blocks reused by every scenario:
//! * [`fixture::Fixture`] — builds a complete `grading.db` plus an
//!   on-disk `data/entregues/<project>/` layout.
//! * [`runner::Runner`] — invokes the `sprint-grader` binary in a
//!   hermetic temp dir with all network env vars unset.
//! * [`snapshot`] — `insta` filters that strip non-deterministic
//!   fields out of `REPORT.md` before comparison.

pub mod fixture;
pub mod runner;
pub mod snapshot;

pub use fixture::{Fixture, FixturePaths};
pub use runner::{Runner, RunnerOutput};
