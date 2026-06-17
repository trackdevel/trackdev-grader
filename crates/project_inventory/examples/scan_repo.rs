//! Throwaway real-repo validation for EXTRA_TECH detectors (no DB writes).
//! Usage: cargo run -p sprint-grader-project-inventory --example scan_repo -- <repo_path> <android|spring>

use std::path::Path;

use sprint_grader_architecture::scanner::scan_repo;
use sprint_grader_project_inventory::{detect_depth, gradle, metrics, Stack};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repo = args
        .get(1)
        .expect("usage: scan_repo <path> <android|spring>");
    let stack = match args.get(2).map(|s| s.as_str()) {
        Some("android") => Stack::Android,
        _ => Stack::Spring,
    };
    let files = scan_repo(Path::new(repo));
    let depth = detect_depth(&files, stack);
    println!("== {repo}  ({:?})  java_files={}", stack, files.len());
    for k in metrics::EXTRA_TECH_KEYS {
        let v = depth.metrics.get(*k).copied().unwrap_or(0.0);
        if v != 0.0 {
            println!("   metric {k} = {v}");
        }
    }
    for f in &depth.features {
        println!(
            "   feature [{}] {} (depth {}) @ {}",
            f.category, f.technology, f.depth, f.evidence
        );
    }
    let coords = gradle::scan_gradle_coords(Path::new(repo));
    println!("   gradle coords = {}", coords.len());
}
