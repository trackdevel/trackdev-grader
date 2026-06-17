//! Spring REST inventory integration fixtures.

use std::fs;
use std::process::Command;

use rusqlite::Connection;
use sprint_grader_core::db::apply_schema;
use sprint_grader_project_inventory::{
    metrics, scan_repo_to_db, InventoryBaseline, TechnologyCatalog,
};
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
fn spring_repo_inventory_counts_controllers_endpoints_entities() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("spring-api");
    fs::create_dir_all(repo.join("src/main/java/com/course/api")).unwrap();
    fs::write(
        repo.join("src/main/java/com/course/api/UserController.java"),
        "package com.course.api;\n\
         import org.springframework.web.bind.annotation.*;\n\
         @RestController\n\
         public class UserController {\n\
           @GetMapping(\"/users\")\n\
           public String list() { return \"ok\"; }\n\
           @PostMapping(\"/users\")\n\
           public String create() { return \"ok\"; }\n\
         }\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/main/java/com/course/api/UserRepository.java"),
        "package com.course.api;\n\
         import org.springframework.stereotype.Repository;\n\
         @Repository\n\
         public interface UserRepository {}\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/main/java/com/course/api/User.java"),
        "package com.course.api;\n\
         import jakarta.persistence.Entity;\n\
         @Entity\n\
         public class User {}\n",
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
    let summary =
        scan_repo_to_db(&conn, &repo, "org/spring-api", 1, &cat, &base, false).expect("scan");
    assert!(!summary.skipped_unchanged);
    assert!(summary.metrics_written > 0);

    let q = |key: &str| -> f64 {
        conn.query_row(
            "SELECT value FROM repo_structural_metrics WHERE repo_full_name = ? AND metric_key = ?",
            rusqlite::params!["org/spring-api", key],
            |r| r.get(0),
        )
        .unwrap()
    };

    assert_eq!(q(metrics::CONTROLLER_COUNT), 1.0);
    assert_eq!(q(metrics::ENDPOINT_COUNT), 2.0);
    assert_eq!(q(metrics::ENTITY_COUNT), 1.0);
    assert_eq!(q(metrics::REPOSITORY_COUNT), 1.0);
    assert!(q(metrics::PRODUCTION_STATEMENT_COUNT) >= 2.0);
}
