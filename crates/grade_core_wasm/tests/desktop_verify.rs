//! Run `apps/desktop` vitest parity checks via cargo (pnpm is invoked as a subprocess).
//!
//! ```bash
//! RUN_DESKTOP_TESTS=1 cargo test -p grade_core_wasm desktop_projection -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn pnpm_bin() -> PathBuf {
    if let Ok(p) = std::env::var("PNPM_PATH") {
        return PathBuf::from(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let nvm = PathBuf::from(&home).join(".nvm/versions/node");
        if nvm.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&nvm) {
                let mut vers: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                vers.sort_by_key(|e| e.file_name());
                for entry in vers.into_iter().rev() {
                    let candidate = entry.path().join("bin/pnpm");
                    if candidate.is_file() {
                        return candidate;
                    }
                }
            }
        }
    }
    PathBuf::from("pnpm")
}

fn run_pnpm(desktop: &PathBuf, args: &[&str]) {
    let status = Command::new(pnpm_bin())
        .current_dir(desktop)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("spawn pnpm {args:?}: {e}"));
    assert!(status.success(), "pnpm {:?} failed", args);
}

#[test]
#[ignore = "run with RUN_DESKTOP_TESTS=1"]
fn desktop_projection() {
    if std::env::var("RUN_DESKTOP_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skip: set RUN_DESKTOP_TESTS=1");
        return;
    }

    let root = workspace_root();
    let desktop = root.join("apps/desktop");
    assert!(desktop.is_dir(), "missing {}", desktop.display());

    if !desktop.join("node_modules").is_dir() {
        run_pnpm(&desktop, &["install"]);
    }
    run_pnpm(&desktop, &["rebuild", "better-sqlite3"]);
    run_pnpm(&desktop, &["test"]);
}
