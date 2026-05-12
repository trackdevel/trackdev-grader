//! Smoke-load the on-disk rubrics through `architecture::rubric::load`
//! to confirm the new Haiku-tuned files parse, the `rubric_version:`
//! frontmatter key is read, and the body hash is stable.
//!
//! Run:
//!
//! ```sh
//! cargo run -p sprint-grader-architecture --example smoke_rubric
//! ```

use sprint_grader_architecture::rubric;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    for name in ["spring-boot-rubric.md", "android-rubric.md"] {
        let path = PathBuf::from("config").join(name);
        let r = rubric::load(&path)?;
        println!(
            "{name}: version={}, body_hash={}, body_len={}",
            r.version,
            &r.body_hash[..16],
            r.body.len()
        );
    }
    Ok(())
}
