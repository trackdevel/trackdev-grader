//! File-path → layer inference for peer-group analysis.
//!
//! Replaces the prior keyword scan over task names. The signal we want is
//! "what kind of code did this task ship", which lives in the file paths
//! the task's PRs touched, not in the prose name a student gave the task.
//! Names in this codebase are routinely Catalan/Spanish/English mixes,
//! parent USER_STORY names trigger spurious multi-layer hits, and the
//! action token is wrong more often than right (a "fix" that net-adds
//! 200 lines is not a modification by any sane definition).
//!
//! This module owns the path-pattern table. The vocabulary mirrors
//! `task_similarity::LAYER_ORDER` so the peer-group section in REPORT.md
//! stays stable across the rewrite.
//!
//! Patterns are checked in declaration order; the first match wins.
//! Order matters: `*ViewModel.java` must beat `**/ui/**` (otherwise
//! every fragment lookalike falls into the same bucket as actual UI
//! glue), and `**/dto/**` must beat `**/api/**` (DTO containers often
//! sit under api/).
//!
//! Stack is supplied by the caller (already known from the linked PR's
//! repo name). Mixed-stack PRs supply `None`; we don't try to guess.

/// Classify a file path into a peer-group layer. Returns `None` when the
/// path doesn't match any pattern (test sources, build outputs,
/// generated code, top-level config, ...). Caller may fold those into
/// an `<stack>_other` bucket — we deliberately don't return one here so
/// the caller can decide whether unmatched files should be displayed.
pub fn layer_for_path(path: &str, stack: Option<&str>) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if should_skip(&lower) {
        return None;
    }
    match stack {
        Some("spring") => spring_layer(&lower, path),
        Some("android") => android_layer(&lower, path),
        _ => None,
    }
}

/// Return `true` for paths that should never participate in peer-group
/// classification: test sources, build outputs, generated code,
/// top-level config files. Lower-cased input.
fn should_skip(p: &str) -> bool {
    SKIP_FRAGMENTS.iter().any(|f| p.contains(f))
}

const SKIP_FRAGMENTS: &[&str] = &[
    "/test/",
    "/tests/",
    "/androidtest/",
    "/build/",
    "/.gradle/",
    "/generated/",
    "/build-output/",
];

/// Spring layer detection. The reference layout is
/// `src/main/java/<root>/<layer-segment>/...` but a few teams collapse
/// the segment into the file basename (e.g. `Foo.java` directly under
/// `controller/`); both shapes are picked up by the same patterns.
fn spring_layer(lower: &str, original: &str) -> Option<&'static str> {
    let basename = file_basename(original);
    let basename_l = basename.to_lowercase();

    // DTO / Mapper sit anywhere — including under api/ — so check first
    // so they don't get classified as the surrounding layer.
    if lower.contains("/dto/")
        || lower.contains("/mapper/")
        || basename_l.ends_with("dto.java")
        || basename_l.ends_with("mapper.java")
        || basename_l.ends_with("converter.java")
    {
        return Some("spring_dto_mapper");
    }
    if lower.contains("/controller/")
        || lower.contains("/web/")
        || lower.contains("/api/")
        || lower.contains("/rest/")
        || basename_l.ends_with("controller.java")
        || basename_l.ends_with("restcontroller.java")
    {
        return Some("spring_controller");
    }
    if lower.contains("/service/")
        || lower.contains("/usecase/")
        || lower.contains("/application/")
        || basename_l.ends_with("service.java")
        || basename_l.ends_with("serviceimpl.java")
    {
        return Some("spring_service");
    }
    if lower.contains("/repository/")
        || lower.contains("/repositories/")
        || lower.contains("/persistence/")
        || lower.contains("/infrastructure/")
        || basename_l.ends_with("repository.java")
        || basename_l.ends_with("dao.java")
    {
        return Some("spring_repository");
    }
    if lower.contains("/entity/")
        || lower.contains("/entities/")
        || lower.contains("/model/")
        || lower.contains("/models/")
        || lower.contains("/domain/")
    {
        return Some("spring_entity");
    }
    if lower.contains("/configuration/")
        || lower.contains("/config/")
        || lower.contains("/security/")
        || basename_l.ends_with("config.java")
        || basename_l.ends_with("configuration.java")
        || basename_l.ends_with("filter.java")
    {
        return Some("spring_config_security");
    }
    None
}

/// Android layer detection. Class-name suffixes (Fragment / Activity /
/// ViewModel / Adapter) are reliable in this course because the
/// professor-supplied skeleton uses them; package-segment heuristics
/// catch the cases where students invented their own grouping.
fn android_layer(lower: &str, original: &str) -> Option<&'static str> {
    let basename = file_basename(original);
    let basename_l = basename.to_lowercase();

    // res/layout/*.xml is a layout file regardless of basename — and
    // gets classified before XML hits any other pattern.
    if lower.ends_with(".xml") && lower.contains("/res/layout") {
        return Some("android_layout");
    }
    // Non-Java / non-XML resources or random asset files don't classify.
    if !lower.ends_with(".java") && !lower.ends_with(".kt") {
        return None;
    }
    if basename_l.ends_with("fragment.java")
        || basename_l.ends_with("fragment.kt")
        || lower.contains("/fragment/")
        || lower.contains("/fragments/")
    {
        return Some("android_fragment");
    }
    if basename_l.ends_with("viewmodel.java")
        || basename_l.ends_with("viewmodel.kt")
        || lower.contains("/viewmodel/")
        || lower.contains("/viewmodels/")
    {
        return Some("android_viewmodel");
    }
    if basename_l.ends_with("adapter.java")
        || basename_l.ends_with("adapter.kt")
        || basename_l.ends_with("viewholder.java")
        || basename_l.ends_with("viewholder.kt")
    {
        return Some("android_recyclerview");
    }
    if basename_l.ends_with("activity.java") || basename_l.ends_with("activity.kt") {
        return Some("android_activity");
    }
    if lower.contains("/navigation/")
        || basename_l.contains("navgraph")
        || basename_l.contains("navcontroller")
    {
        return Some("android_navigation");
    }
    if lower.contains("/retrofit/")
        || basename_l.ends_with("apiservice.java")
        || basename_l.ends_with("apiservice.kt")
        || basename_l.ends_with("apiclient.java")
        || basename_l.ends_with("apiclient.kt")
        || lower.contains("/api/")
    {
        return Some("android_retrofit");
    }
    if lower.contains("/room/")
        || basename_l.ends_with("dao.java")
        || basename_l.ends_with("dao.kt")
        || basename_l.ends_with("database.java")
        || basename_l.ends_with("database.kt")
    {
        return Some("android_room");
    }
    if lower.contains("/data/")
        || lower.contains("/repository/")
        || lower.contains("/repositories/")
        || basename_l.ends_with("repository.java")
        || basename_l.ends_with("repository.kt")
    {
        return Some("android_repository");
    }
    None
}

fn file_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_controller_paths_classify_as_controller() {
        assert_eq!(
            layer_for_path(
                "src/main/java/org/udg/pds/springtodo/controller/UserController.java",
                Some("spring"),
            ),
            Some("spring_controller")
        );
        // Bare api/ folder also lands in the controller bucket — common
        // shape on student forks.
        assert_eq!(
            layer_for_path(
                "src/main/java/org/example/api/PostsApi.java",
                Some("spring"),
            ),
            Some("spring_controller")
        );
    }

    #[test]
    fn spring_dto_inside_api_classifies_as_dto_not_controller() {
        // Regression: DTO-like files in the controller package must not
        // be classified as controllers; the rule order is dto-first.
        assert_eq!(
            layer_for_path("src/main/java/org/example/api/UserDto.java", Some("spring"),),
            Some("spring_dto_mapper")
        );
        assert_eq!(
            layer_for_path(
                "src/main/java/org/example/dto/CreatePostRequest.java",
                Some("spring"),
            ),
            Some("spring_dto_mapper")
        );
    }

    #[test]
    fn spring_service_repository_entity() {
        assert_eq!(
            layer_for_path(
                "src/main/java/org/udg/pds/springtodo/service/PostService.java",
                Some("spring"),
            ),
            Some("spring_service")
        );
        assert_eq!(
            layer_for_path(
                "src/main/java/org/udg/pds/springtodo/repository/UserRepository.java",
                Some("spring"),
            ),
            Some("spring_repository")
        );
        // Spring teams use `model/` for entities in this course.
        assert_eq!(
            layer_for_path(
                "src/main/java/org/udg/pds/springtodo/model/User.java",
                Some("spring"),
            ),
            Some("spring_entity")
        );
    }

    #[test]
    fn spring_security_and_config() {
        assert_eq!(
            layer_for_path(
                "src/main/java/org/example/configuration/SecSecurityConfig.java",
                Some("spring"),
            ),
            Some("spring_config_security")
        );
        assert_eq!(
            layer_for_path(
                "src/main/java/org/example/security/JwtFilter.java",
                Some("spring"),
            ),
            Some("spring_config_security")
        );
    }

    #[test]
    fn android_fragment_activity_viewmodel_adapter() {
        let base = "app/src/main/java/org/udg/pds/todoandroid/ui";
        assert_eq!(
            layer_for_path(&format!("{base}/AlbumsFragment.java"), Some("android")),
            Some("android_fragment")
        );
        assert_eq!(
            layer_for_path(&format!("{base}/LoginActivity.java"), Some("android")),
            Some("android_activity")
        );
        assert_eq!(
            layer_for_path(
                &format!("{base}/viewmodel/CurrentUserViewModel.java"),
                Some("android"),
            ),
            Some("android_viewmodel")
        );
        assert_eq!(
            layer_for_path(&format!("{base}/UserAdapter.java"), Some("android")),
            Some("android_recyclerview")
        );
    }

    #[test]
    fn android_layout_xml_classifies_as_layout() {
        assert_eq!(
            layer_for_path(
                "app/src/main/res/layout/fragment_login.xml",
                Some("android"),
            ),
            Some("android_layout")
        );
        // Other res/ XML (strings, drawables, …) is not a layout.
        assert_eq!(
            layer_for_path("app/src/main/res/values/strings.xml", Some("android")),
            None
        );
    }

    #[test]
    fn android_retrofit_room_repository() {
        let base = "app/src/main/java/org/udg/pds/todoandroid";
        assert_eq!(
            layer_for_path(&format!("{base}/api/ApiService.java"), Some("android")),
            Some("android_retrofit")
        );
        assert_eq!(
            layer_for_path(&format!("{base}/data/UserRepository.java"), Some("android")),
            Some("android_repository")
        );
        assert_eq!(
            layer_for_path(&format!("{base}/room/AppDatabase.kt"), Some("android")),
            Some("android_room")
        );
    }

    #[test]
    fn test_paths_are_skipped() {
        assert_eq!(
            layer_for_path(
                "src/test/java/org/example/controller/UserControllerTest.java",
                Some("spring"),
            ),
            None
        );
        assert_eq!(
            layer_for_path("app/src/androidTest/java/Foo.java", Some("android"),),
            None
        );
        assert_eq!(
            layer_for_path("app/build/generated/UserBindingImpl.java", Some("android")),
            None
        );
    }

    #[test]
    fn unknown_stack_returns_none() {
        assert_eq!(
            layer_for_path("foo/bar/Baz.java", None),
            None,
            "no stack means we can't classify"
        );
        assert_eq!(layer_for_path("foo/Baz.java", Some("spring")), None);
    }

    #[test]
    fn xml_outside_layout_does_not_classify_as_android() {
        // Manifest, build files, gradle XML — none of these are peer-group
        // signals.
        assert_eq!(
            layer_for_path("app/src/main/AndroidManifest.xml", Some("android")),
            None
        );
    }
}
