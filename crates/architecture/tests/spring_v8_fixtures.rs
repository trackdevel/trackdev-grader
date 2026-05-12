//! Integration test for Wave 2 of the AST-rubric migration.
//!
//! For each Spring v8 rule we lay down a small Java fixture under a
//! tempdir, run `scan_repo_to_db`, and assert the rule fires (BAD) or
//! stays silent (GOOD). The fixtures and assertions live in one file so
//! a future plan author can grep for a rule_id and find both its
//! offending source and its expected behaviour together.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use sprint_grader_architecture::{scan_repo_to_db, ArchitectureRules};
use tempfile::TempDir;

/// Convenience: writes `body` to `repo_root/rel` (creating parent dirs)
/// and returns the absolute path.
fn write_java(repo_root: &Path, rel: &str, body: &str) {
    let p = repo_root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// Initialise `dir` as a single-commit git repo so the architecture
/// scan's `head_sha` lookup succeeds. Without it the run is recorded as
/// `SKIPPED_NO_SOURCES`.
fn git_init(dir: &Path) {
    let run = |args: &[&str]| {
        let s = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git invocation");
        assert!(s.success(), "git {args:?} failed");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "fixtures@example.com"]);
    run(&["config", "user.name", "Fixtures"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "fixture"]);
}

/// Scan the tempdir under one of the Spring v8 rules in
/// `config/architecture.toml`, returning the rule_names that fired.
fn scan_with_production_config(repo: &Path) -> HashSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/architecture.toml");
    let rules = ArchitectureRules::load(&path).expect("config/architecture.toml must parse");
    let conn = Connection::open_in_memory().unwrap();
    sprint_grader_core::db::apply_schema(&conn).unwrap();
    scan_repo_to_db(&conn, repo, "udg/fixture", &rules).unwrap();
    let mut out = HashSet::new();
    let mut stmt = conn
        .prepare("SELECT rule_name FROM architecture_violations WHERE repo_full_name = ?")
        .unwrap();
    let rows = stmt
        .query_map(["udg/fixture"], |r| r.get::<_, String>(0))
        .unwrap();
    for r in rows {
        out.insert(r.unwrap());
    }
    out
}

/// Like above, but also captures `(rule_name, severity)` so a test can
/// assert the rubric-prescribed severity reached SQLite.
fn scan_with_severity(repo: &Path) -> Vec<(String, String)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/architecture.toml");
    let rules = ArchitectureRules::load(&path).expect("config/architecture.toml must parse");
    let conn = Connection::open_in_memory().unwrap();
    sprint_grader_core::db::apply_schema(&conn).unwrap();
    scan_repo_to_db(&conn, repo, "udg/fixture", &rules).unwrap();
    let mut stmt = conn
        .prepare("SELECT rule_name, severity FROM architecture_violations WHERE repo_full_name = ?")
        .unwrap();
    let rows = stmt
        .query_map(["udg/fixture"], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

#[test]
fn bad_case_controller_returns_non_dto() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import com.x.domain.User;\n\
         import org.springframework.web.bind.annotation.GetMapping;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             @GetMapping(\"/u\")\n\
             public User get() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "expected CONTROLLER_RETURNS_NON_DTO, got: {hits:?}"
    );
}

#[test]
fn good_case_controller_returns_dto_imported_from_dto_package() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import com.x.dto.UserView;\n\
         import org.springframework.web.bind.annotation.GetMapping;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             @GetMapping(\"/u\")\n\
             public UserView get() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "DTO-package import must suppress: {hits:?}"
    );
}

#[test]
fn bad_case_controller_uses_repository_field() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             private final UserRepository userRepository = null;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("CONTROLLER_USES_REPOSITORY"),
        "expected CONTROLLER_USES_REPOSITORY, got: {hits:?}"
    );
}

#[test]
fn bad_case_controller_uses_repository_ctor() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public UserController(UserRepository r) {}\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("CONTROLLER_USES_REPOSITORY"),
        "expected CONTROLLER_USES_REPOSITORY (ctor form), got: {hits:?}"
    );
}

#[test]
fn bad_case_controller_has_transactional_class_level() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/OrderController.java",
        "package com.x.controller;\n\
         import org.springframework.transaction.annotation.Transactional;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         @Transactional\n\
         public class OrderController {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("CONTROLLER_HAS_TRANSACTIONAL"),
        "expected CONTROLLER_HAS_TRANSACTIONAL, got: {hits:?}"
    );
}

#[test]
fn bad_case_controller_has_transactional_method_level() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/OrderController.java",
        "package com.x.controller;\n\
         import org.springframework.transaction.annotation.Transactional;\n\
         import org.springframework.web.bind.annotation.PostMapping;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class OrderController {\n\
             @Transactional\n\
             @PostMapping\n\
             public Object create() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("CONTROLLER_HAS_TRANSACTIONAL"),
        "expected CONTROLLER_HAS_TRANSACTIONAL on method, got: {hits:?}"
    );
}

#[test]
fn bad_case_transactional_on_non_public_method() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/OrderService.java",
        "package com.x.service;\n\
         import org.springframework.stereotype.Service;\n\
         import org.springframework.transaction.annotation.Transactional;\n\
         @Service\n\
         class OrderService {\n\
             @Transactional\n\
             void saveOrder() {}\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("TRANSACTIONAL_ON_NON_PUBLIC_METHOD"),
        "expected TRANSACTIONAL_ON_NON_PUBLIC_METHOD, got: {hits:?}"
    );
}

#[test]
fn bad_case_unbounded_find_all_in_service() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/UserService.java",
        "package com.x.service;\n\
         import org.springframework.stereotype.Service;\n\
         @Service\n\
         public class UserService {\n\
             private final UserRepository userRepository = null;\n\
             public Object list() { return userRepository.findAll(); }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("UNBOUNDED_FIND_ALL"),
        "expected UNBOUNDED_FIND_ALL, got: {hits:?}"
    );
}

#[test]
fn good_case_paginated_find_all_is_silent() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/UserService.java",
        "package com.x.service;\n\
         import org.springframework.stereotype.Service;\n\
         @Service\n\
         public class UserService {\n\
             private final UserRepository userRepository = null;\n\
             public Object list(Object pageable) { return userRepository.findAll(pageable); }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("UNBOUNDED_FIND_ALL"),
        "findAll(pageable) must not fire: {hits:?}"
    );
}

#[test]
fn bad_case_entity_uses_lombok_data() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/entity/Post.java",
        "package com.x.entity;\n\
         import jakarta.persistence.Entity;\n\
         import lombok.Data;\n\
         @Entity\n\
         @Data\n\
         public class Post {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("ENTITY_USES_LOMBOK_DATA"),
        "expected ENTITY_USES_LOMBOK_DATA, got: {hits:?}"
    );
}

#[test]
fn bad_case_entity_uses_javax_import() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/entity/Post.java",
        "package com.x.entity;\n\
         import javax.persistence.Entity;\n\
         @Entity\n\
         public class Post {}\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("ENTITY_USES_JAVAX_IMPORT"),
        "expected ENTITY_USES_JAVAX_IMPORT, got: {hits:?}"
    );
}

#[test]
fn bad_case_fat_controller_method() {
    let body: String = (1..=30)
        .map(|i| format!("        int v{i} = {i};\n"))
        .collect();
    let src = format!(
        "package com.x.controller;\n\
         import org.springframework.web.bind.annotation.PostMapping;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class FatController {{\n\
             @PostMapping\n\
             public int fat() {{\n\
{body}\
                 return v1 + v2;\n\
             }}\n\
         }}\n"
    );
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/FatController.java",
        &src,
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("FAT_CONTROLLER_METHOD"),
        "expected FAT_CONTROLLER_METHOD, got: {hits:?}"
    );
}

#[test]
fn bad_case_manual_dto_mapping() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public Object handle(Object user) {\n\
                 return new UserResponse(user, user);\n\
             }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("MANUAL_DTO_MAPPING_IN_CONTROLLER"),
        "expected MANUAL_DTO_MAPPING_IN_CONTROLLER, got: {hits:?}"
    );
}

#[test]
fn bad_case_missing_valid_on_request_body() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import org.springframework.web.bind.annotation.RequestBody;\n\
         import org.springframework.web.bind.annotation.PostMapping;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             @PostMapping\n\
             public Object create(@RequestBody CreateRequest req) { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("MISSING_VALID_ON_REQUEST_BODY"),
        "expected MISSING_VALID_ON_REQUEST_BODY, got: {hits:?}"
    );
}

#[test]
fn good_case_request_body_with_valid_companion_is_silent() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import jakarta.validation.Valid;\n\
         import org.springframework.web.bind.annotation.RequestBody;\n\
         import org.springframework.web.bind.annotation.PostMapping;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             @PostMapping\n\
             public Object create(@Valid @RequestBody CreateRequest req) { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("MISSING_VALID_ON_REQUEST_BODY"),
        "@Valid companion must suppress: {hits:?}"
    );
}

#[test]
fn bad_case_service_public_method_uses_non_dto() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/UserService.java",
        "package com.x.service;\n\
         import com.x.domain.User;\n\
         import org.springframework.stereotype.Service;\n\
         @Service\n\
         public class UserService {\n\
             public User create(User u) { return u; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("SERVICE_PUBLIC_METHOD_USES_NON_DTO"),
        "expected SERVICE_PUBLIC_METHOD_USES_NON_DTO, got: {hits:?}"
    );
}

#[test]
fn good_case_service_uses_dto_at_boundary() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/UserService.java",
        "package com.x.service;\n\
         import com.x.dto.CreateUserRequest;\n\
         import com.x.dto.UserResponse;\n\
         import org.springframework.stereotype.Service;\n\
         @Service\n\
         public class UserService {\n\
             public UserResponse create(CreateUserRequest r) { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("SERVICE_PUBLIC_METHOD_USES_NON_DTO"),
        "DTO at boundary must not fire: {hits:?}"
    );
}

#[test]
fn bad_case_service_uses_multiple_repositories() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/OrderService.java",
        "package com.x.service;\n\
         import org.springframework.stereotype.Service;\n\
         @Service\n\
         public class OrderService {\n\
             private final OrderRepository a = null;\n\
             private final UserRepository b = null;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("SERVICE_USES_MULTIPLE_REPOSITORIES"),
        "expected SERVICE_USES_MULTIPLE_REPOSITORIES, got: {hits:?}"
    );
}

#[test]
fn bad_case_entity_depends_on_spring_bean() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/entity/Order.java",
        "package com.x.entity;\n\
         import jakarta.persistence.Entity;\n\
         @Entity\n\
         public class Order {\n\
             private PricingService pricingService;\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("ENTITY_DEPENDS_ON_SPRING_BEAN"),
        "expected ENTITY_DEPENDS_ON_SPRING_BEAN, got: {hits:?}"
    );
}

// ---------- generic-wrapper unwrapping for the non-DTO gate ----------
//
// The rubric prose promises "Generic wrappers (`ResponseEntity<T>`,
// `Optional<T>`, `List<T>`, `Page<T>`, `Mono<T>`, `Flux<T>`) are
// stripped before the type is tested, so the inner type is what
// matters." These tests pin that behaviour down end-to-end.

#[test]
fn good_case_controller_returns_list_of_dto_by_package() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import java.util.List;\n\
         import com.x.dto.UserView;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public List<UserView> list() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "List<UserView> (UserView imported from a dto. package) must not fire: {hits:?}"
    );
}

#[test]
fn good_case_controller_returns_collection_of_dto_by_name() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import java.util.Collection;\n\
         import com.x.domain.UserResponse;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public Collection<UserResponse> list() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "Collection<UserResponse> (DTO by name) must not fire: {hits:?}"
    );
}

#[test]
fn good_case_controller_returns_optional_of_dto() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import java.util.Optional;\n\
         import com.x.dto.UserView;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public Optional<UserView> get() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "Optional<UserView> must not fire: {hits:?}"
    );
}

#[test]
fn good_case_controller_returns_responseentity_of_list_of_dto_nested() {
    // Two layers of wrappers — `ResponseEntity<List<UserView>>` —
    // unwrap recurses until it hits the meaningful inner type.
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import java.util.List;\n\
         import com.x.dto.UserView;\n\
         import org.springframework.http.ResponseEntity;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public ResponseEntity<List<UserView>> list() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "ResponseEntity<List<UserView>> must not fire: {hits:?}"
    );
}

#[test]
fn good_case_controller_returns_list_of_stdlib_value() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import java.util.List;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public List<Long> ids() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "List<Long> must not fire (stdlib inner): {hits:?}"
    );
}

#[test]
fn bad_case_controller_returns_list_of_entity() {
    // Counter-example: the wrapper-stripping must not become a blanket
    // pass. `List<User>` from a domain package still exposes a non-DTO.
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import java.util.List;\n\
         import com.x.domain.User;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public List<User> list() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        hits.contains("CONTROLLER_RETURNS_NON_DTO"),
        "List<User> (non-DTO inner) must still fire: {hits:?}"
    );
}

#[test]
fn good_case_service_takes_list_of_dto_param() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/UserService.java",
        "package com.x.service;\n\
         import java.util.List;\n\
         import com.x.dto.CreateUserRequest;\n\
         import com.x.dto.UserResponse;\n\
         import org.springframework.stereotype.Service;\n\
         @Service\n\
         public class UserService {\n\
             public UserResponse createAll(List<CreateUserRequest> reqs) { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    assert!(
        !hits.contains("SERVICE_PUBLIC_METHOD_USES_NON_DTO"),
        "List<CreateUserRequest> param must not fire: {hits:?}"
    );
}

/// One GOOD fixture per rule, all stuffed into one project — together
/// they must produce zero Spring v8 violations.
#[test]
fn all_good_cases_produce_zero_spring_v8_violations() {
    let tmp = TempDir::new().unwrap();
    let v8: HashSet<&str> = [
        "CONTROLLER_RETURNS_NON_DTO",
        "CONTROLLER_USES_REPOSITORY",
        "CONTROLLER_HAS_TRANSACTIONAL",
        "TRANSACTIONAL_ON_NON_PUBLIC_METHOD",
        "UNBOUNDED_FIND_ALL",
        "ENTITY_USES_LOMBOK_DATA",
        "ENTITY_USES_JAVAX_IMPORT",
        "FAT_CONTROLLER_METHOD",
        "MANUAL_DTO_MAPPING_IN_CONTROLLER",
        "MISSING_VALID_ON_REQUEST_BODY",
        "SERVICE_PUBLIC_METHOD_USES_NON_DTO",
        "SERVICE_USES_MULTIPLE_REPOSITORIES",
        "ENTITY_DEPENDS_ON_SPRING_BEAN",
    ]
    .into_iter()
    .collect();

    // A clean controller using a DTO + a service that mediates.
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import com.x.dto.UserView;\n\
         import com.x.dto.CreateUserRequest;\n\
         import jakarta.validation.Valid;\n\
         import org.springframework.web.bind.annotation.GetMapping;\n\
         import org.springframework.web.bind.annotation.PostMapping;\n\
         import org.springframework.web.bind.annotation.RequestBody;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public UserView get() { return null; }\n\
             public UserView create(@Valid @RequestBody CreateUserRequest r) { return null; }\n\
         }\n",
    );

    // A service with one repository, DTO at the boundary.
    write_java(
        tmp.path(),
        "src/main/java/com/x/service/UserService.java",
        "package com.x.service;\n\
         import com.x.dto.CreateUserRequest;\n\
         import com.x.dto.UserResponse;\n\
         import org.springframework.stereotype.Service;\n\
         import org.springframework.transaction.annotation.Transactional;\n\
         @Service\n\
         public class UserService {\n\
             private final UserRepository userRepository = null;\n\
             @Transactional\n\
             public UserResponse create(CreateUserRequest r) { return null; }\n\
         }\n",
    );

    // An entity with Lombok @Getter/@Setter (allowed by the rubric).
    write_java(
        tmp.path(),
        "src/main/java/com/x/entity/User.java",
        "package com.x.entity;\n\
         import jakarta.persistence.Entity;\n\
         import jakarta.persistence.Id;\n\
         @Entity\n\
         public class User {\n\
             @Id private Long id;\n\
         }\n",
    );

    git_init(tmp.path());
    let hits = scan_with_production_config(tmp.path());
    let v8_hits: Vec<_> = hits.iter().filter(|h| v8.contains(h.as_str())).collect();
    assert!(
        v8_hits.is_empty(),
        "GOOD fixtures must produce zero Spring v8 violations, got: {v8_hits:?}"
    );
}

/// The severity column on every inserted row must come from the rule's
/// own `severity = "..."`, not from the file-default. This guards the
/// W2 verification gate.
#[test]
fn severity_is_per_rule_not_file_default() {
    let tmp = TempDir::new().unwrap();
    write_java(
        tmp.path(),
        "src/main/java/com/x/controller/UserController.java",
        "package com.x.controller;\n\
         import com.x.domain.User;\n\
         import org.springframework.web.bind.annotation.RestController;\n\
         @RestController\n\
         public class UserController {\n\
             public User get() { return null; }\n\
         }\n",
    );
    git_init(tmp.path());
    let rows = scan_with_severity(tmp.path());
    let crn = rows
        .iter()
        .find(|(name, _)| name == "CONTROLLER_RETURNS_NON_DTO")
        .expect("CONTROLLER_RETURNS_NON_DTO must fire");
    assert_eq!(
        crn.1, "CRITICAL",
        "rule-level severity must override the file default, got: {crn:?}"
    );
}
