//! End-to-end smoke test: build a throwaway git repo, run compile_pr_in_worktree.
//! Skipped on platforms without /bin/sh (i.e. only runs on Unix).

#![cfg(unix)]

use regex::Regex;
use sprint_grader_compile::builder::{compile_pr_in_worktree, BuildProfileRe};
use std::collections::HashMap;
use std::process::Command;

fn run(args: &[&str], cwd: &std::path::Path) {
    let o = Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .output()
        .expect("spawn");
    assert!(
        o.status.success(),
        "cmd failed: {:?} stderr={}",
        args,
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn worktree_build_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    run(
        &["git", "-c", "init.defaultBranch=main", "init", "-q"],
        &repo,
    );
    run(&["git", "config", "user.email", "t@t"], &repo);
    run(&["git", "config", "user.name", "T"], &repo);
    std::fs::write(repo.join("build.sh"), "#!/bin/sh\necho ok\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = repo.join("build.sh");
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&p, perms).unwrap();
    }
    run(&["git", "add", "build.sh"], &repo);
    run(&["git", "commit", "-q", "-m", "initial"], &repo);
    let sha = {
        let o = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };

    let profile = BuildProfileRe {
        repo_pattern: Regex::new(".*").unwrap(),
        command: "./build.sh".into(),
        timeout_seconds: 30,
        working_dir: ".".into(),
        env: HashMap::new(),
        mutation_command: None,
        mutation_timeout_seconds: 600,
        mutation_report_path: "build/reports/pitest/mutations.xml".into(),
    };
    let r = compile_pr_in_worktree(&repo, &sha, &profile, "pr_test_1234", 10_000)
        .expect("build returned None");
    assert!(r.compiles, "expected success, got: {:?}", r);
    assert_eq!(r.exit_code, 0);
    assert_eq!(r.merge_sha, sha);
}

#[test]
fn worktree_build_timeout_kills_hard() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    run(
        &["git", "-c", "init.defaultBranch=main", "init", "-q"],
        &repo,
    );
    run(&["git", "config", "user.email", "t@t"], &repo);
    run(&["git", "config", "user.name", "T"], &repo);
    std::fs::write(repo.join("slow.sh"), "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = repo.join("slow.sh");
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&p, perms).unwrap();
    }
    run(&["git", "add", "slow.sh"], &repo);
    run(&["git", "commit", "-q", "-m", "initial"], &repo);
    let sha = {
        let o = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };

    let profile = BuildProfileRe {
        repo_pattern: Regex::new(".*").unwrap(),
        command: "./slow.sh".into(),
        timeout_seconds: 1,
        working_dir: ".".into(),
        env: HashMap::new(),
        mutation_command: None,
        mutation_timeout_seconds: 600,
        mutation_report_path: "build/reports/pitest/mutations.xml".into(),
    };
    let start = std::time::Instant::now();
    let r = compile_pr_in_worktree(&repo, &sha, &profile, "pr_slow_0001", 10_000).unwrap();
    let elapsed = start.elapsed().as_secs();
    assert!(r.timed_out, "expected timed_out, got {:?}", r);
    assert!(!r.compiles);
    // Should kill promptly — give 10s slack for worktree/cleanup overhead on slow CI.
    assert!(elapsed < 10, "timeout did not kill promptly: {}s", elapsed);
}
