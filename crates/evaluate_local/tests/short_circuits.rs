//! Spec-parity tests for the hand-rolled task-id-only and md-link-only
//! matchers. The contract mirrors the `regex` and `fancy-regex` patterns
//! used in `crates/evaluate/src/llm_eval.rs`; we trade engine dependency
//! for a tiny hand-coded scanner.

use sprint_grader_evaluate_local::flags::{
    detect, is_task_id_only_body, is_task_md_link_only_body, DetFlag,
};

#[test]
fn task_id_only_matches_single_and_separated_tokens() {
    assert!(is_task_id_only_body("PDS-123"));
    assert!(is_task_id_only_body("pds-44"));
    assert!(is_task_id_only_body("PDS-1 PDS-2 PDS-3"));
    assert!(is_task_id_only_body("PDS-1, PDS-2"));
    assert!(is_task_id_only_body("PDS-1; pds-2"));
    assert!(is_task_id_only_body("  PDS-1  "));
}

#[test]
fn task_id_only_rejects_prose_and_malformed_tokens() {
    assert!(!is_task_id_only_body(""));
    assert!(!is_task_id_only_body("PDS-123 implements login"));
    assert!(!is_task_id_only_body("PDS"));
    assert!(!is_task_id_only_body("123"));
    assert!(!is_task_id_only_body("-123"));
    assert!(!is_task_id_only_body("PDS-"));
    assert!(!is_task_id_only_body("PDS-abc"));
    assert!(!is_task_id_only_body("PDS123"));
}

#[test]
fn md_link_only_matches_single_and_separated_links() {
    assert!(is_task_md_link_only_body(
        "[p4d-194](https://trackdev.org/dashboard/tasks/5075)"
    ));
    assert!(is_task_md_link_only_body(
        "[p4d-194](https://example.com), [p4d-195](https://example.com)"
    ));
    assert!(is_task_md_link_only_body(
        "[PDS-1](https://example.com); [PDS-2](https://example.com)"
    ));
    assert!(is_task_md_link_only_body(
        "  [PDS-1](https://example.com)  [PDS-2](https://example.com)  "
    ));
}

#[test]
fn md_link_only_rejects_prose_and_malformed_links() {
    assert!(!is_task_md_link_only_body(""));
    assert!(!is_task_md_link_only_body(
        "[p4d-194](https://example.com) Adds the user endpoint."
    ));
    assert!(!is_task_md_link_only_body("[noid](https://example.com)"));
    assert!(!is_task_md_link_only_body("[PDS-](https://example.com)"));
    assert!(!is_task_md_link_only_body("[PDS-1]()"));
    assert!(!is_task_md_link_only_body("[PDS-1](https://example.com"));
    assert!(!is_task_md_link_only_body("PDS-1"));
}

#[test]
fn detect_returns_empty_body_when_body_is_missing() {
    let flags = detect(Some("Real title here"), None);
    assert!(flags.contains(&DetFlag::EmptyBody));
}

#[test]
fn detect_returns_empty_body_for_template_only() {
    // Headings-only body is "empty" per is_empty_description.
    let flags = detect(
        Some("Login controller"),
        Some("# Summary\n## Testing\n## Details\n"),
    );
    assert!(flags.contains(&DetFlag::EmptyBody));
    // Empty-body wins over task-id-only: the trimmed body has prose-like
    // markdown headings, so the task-id matcher would have rejected it
    // anyway, but the contract is that we only flag one body category.
    assert!(!flags.contains(&DetFlag::TaskIdOnlyBody));
}

#[test]
fn detect_returns_task_id_only_for_md_link_only_body_over_20_chars() {
    let flags = detect(
        Some("Implement the login controller"),
        Some("[p4d-194](https://trackdev.org/dashboard/tasks/5075)"),
    );
    assert!(flags.contains(&DetFlag::TaskIdOnlyBody));
    assert!(!flags.contains(&DetFlag::EmptyBody));
    assert!(!flags.contains(&DetFlag::GenericTitle));
}

#[test]
fn detect_returns_generic_title_independently_of_body() {
    let flags = detect(
        Some("fix"),
        Some("This PR adds a real login controller behind the auth service so users can sign in."),
    );
    assert!(flags.contains(&DetFlag::GenericTitle));
    assert!(!flags.contains(&DetFlag::EmptyBody));
    assert!(!flags.contains(&DetFlag::TaskIdOnlyBody));
}

#[test]
fn detect_returns_no_flags_for_high_quality_pr() {
    let flags = detect(
        Some("Implement the login controller with JWT-based auth"),
        Some(
            "Adds the login controller and wires it to the existing auth service. \
             Linked to task PDS-42; verify by running the auth test suite.",
        ),
    );
    assert!(flags.is_empty(), "expected no flags, got {flags:?}");
}
