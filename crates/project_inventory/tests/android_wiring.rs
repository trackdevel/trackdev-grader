//! Android reactive-wiring inventory integration fixtures.

use std::fs;
use std::process::Command;

use rusqlite::Connection;
use sprint_grader_core::db::apply_schema;
use sprint_grader_project_inventory::{metrics, scan_repo_to_db};
use tempfile::TempDir;

fn git_init_commit(dir: &std::path::Path) {
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
fn android_repo_inventory_counts_fragments_observe_and_nav() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("android-app");
    fs::create_dir_all(repo.join("app/src/main/java/com/course/ui")).unwrap();
    fs::write(
        repo.join("app/src/main/java/com/course/ui/HomeFragment.java"),
        "package com.course.ui;\n\
         import androidx.fragment.app.Fragment;\n\
         import androidx.lifecycle.ViewModel;\n\
         import androidx.lifecycle.LiveData;\n\
         import androidx.lifecycle.MutableLiveData;\n\
         public class HomeFragment extends Fragment {\n\
           private HomeViewModel viewModel;\n\
           void onViewCreated() {\n\
             viewModel.getUsers().observe(getViewLifecycleOwner(), u -> {});\n\
             androidx.navigation.Navigation.findNavController(requireView()).navigate(1);\n\
           }\n\
         }\n\
         class HomeViewModel extends ViewModel {\n\
           private final MutableLiveData<String> users = new MutableLiveData<>();\n\
           public LiveData<String> getUsers() { return users; }\n\
         }\n",
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

    let summary = scan_repo_to_db(&conn, &repo, "org/android-app", 1, false).expect("scan");
    assert!(!summary.skipped_unchanged);

    let q = |key: &str| -> f64 {
        conn.query_row(
            "SELECT value FROM repo_structural_metrics WHERE repo_full_name = ? AND metric_key = ?",
            rusqlite::params!["org/android-app", key],
            |r| r.get(0),
        )
        .unwrap()
    };

    assert_eq!(q(metrics::FRAGMENT_COUNT), 1.0);
    assert_eq!(q(metrics::VIEWMODEL_COUNT), 1.0);
    assert_eq!(q(metrics::OBSERVE_CALL_COUNT), 1.0);
    assert_eq!(q(metrics::NAV_DISPATCH_COUNT), 1.0);
    assert!(q(metrics::REACTIVE_WIRING_DENSITY) > 0.0);
}
