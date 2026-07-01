//! Built-in Java type-naming checks. External analyzers map Checkstyle
//! "error" to WARNING so style rules never outrank bugs; snake_case
//! type names are a curriculum violation severe enough to surface as
//! CRITICAL via this pass.

use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use tree_sitter::{Node, Parser};

use crate::adapter::{Category, Finding, Severity};

pub const BUILTIN_ANALYZER_ID: &str = "grader";
pub const BUILTIN_ANALYZER_VERSION: &str = "1";
pub const SNAKE_CASE_TYPE_RULE_ID: &str = "SNAKE_CASE_TYPE_NAME";

static JAVA_LANG: Lazy<tree_sitter::Language> = Lazy::new(|| tree_sitter_java::LANGUAGE.into());

const TYPE_DECL_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
];

/// `true` when `name` is a snake_case type identifier: starts with a
/// lowercase ASCII letter and contains only lowercase letters, digits, and
/// underscores, with at least one underscore (so `userservice` is not
/// flagged — wrong CamelCase but not snake_case).
pub fn is_snake_case_type_name(name: &str) -> bool {
    if name.is_empty() || !name.contains('_') {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Walk `source_roots` under `repo_path` and emit one CRITICAL finding per
/// snake_case type declaration.
pub fn scan_snake_case_types(repo_path: &Path, source_roots: &[PathBuf]) -> Vec<Finding> {
    let roots: Vec<PathBuf> = if source_roots.is_empty() {
        vec![repo_path.to_path_buf()]
    } else {
        source_roots.to_vec()
    };

    let mut findings = Vec::new();
    for root in &roots {
        scan_tree(root, repo_path, &mut findings);
    }
    findings.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    findings
}

fn scan_tree(dir: &Path, repo_root: &Path, out: &mut Vec<Finding>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_tree(&path, repo_root, out);
        } else if path.extension().is_some_and(|e| e == "java") {
            scan_java_file(&path, repo_root, out);
        }
    }
}

fn scan_java_file(path: &Path, repo_root: &Path, out: &mut Vec<Finding>) {
    let source = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    let mut parser = Parser::new();
    if parser.set_language(&JAVA_LANG).is_err() {
        return;
    }
    let tree = match parser.parse(&source, None) {
        Some(t) => t,
        None => return,
    };
    let rel_path = repo_relative_path(repo_root, path);
    walk_types(tree.root_node(), &source, &rel_path, out);
}

fn walk_types(node: Node, source: &[u8], rel_path: &str, out: &mut Vec<Finding>) {
    if TYPE_DECL_KINDS.contains(&node.kind()) {
        if let Some(id_node) = type_name_identifier(node) {
            let name = node_text(id_node, source);
            if is_snake_case_type_name(&name) {
                let line = (id_node.start_position().row as u32) + 1;
                let message = format!(
                    "Type name '{name}' uses snake_case; Java types must be UpperCamelCase \
                     (PascalCase), e.g. 'UserService'."
                );
                out.push(Finding {
                    analyzer: BUILTIN_ANALYZER_ID.to_string(),
                    rule_id: SNAKE_CASE_TYPE_RULE_ID.to_string(),
                    category: Category::Style,
                    severity: Severity::Critical,
                    file_path: rel_path.to_string(),
                    start_line: Some(line),
                    end_line: Some(line),
                    message,
                    help_uri: Some(
                        "https://www.oracle.com/java/technologies/javase/codeconventions-namingconventions.html".into(),
                    ),
                    fingerprint: Finding::compute_fingerprint(
                        BUILTIN_ANALYZER_ID,
                        SNAKE_CASE_TYPE_RULE_ID,
                        rel_path,
                        Some(line),
                        &format!("Type name '{name}' uses snake_case"),
                    ),
                });
            }
        }
    }
    for child in children(node) {
        walk_types(child, source, rel_path, out);
    }
}

fn type_name_identifier(node: Node) -> Option<Node> {
    children(node)
        .into_iter()
        .find(|c| c.kind() == "identifier")
}

fn children(node: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        out.push(child);
    }
    out
}

fn node_text(node: Node, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").to_string()
}

fn repo_relative_path(repo_root: &Path, file: &Path) -> String {
    file.strip_prefix(repo_root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_detector() {
        assert!(is_snake_case_type_name("user_service"));
        assert!(is_snake_case_type_name("my_api_client"));
        assert!(is_snake_case_type_name("foo_bar_baz"));
        assert!(!is_snake_case_type_name("UserService"));
        assert!(!is_snake_case_type_name("userservice"));
        assert!(!is_snake_case_type_name("HTTP_CLIENT"));
        assert!(!is_snake_case_type_name("My_Class"));
        assert!(!is_snake_case_type_name("Foo"));
    }

    #[test]
    fn scan_finds_snake_case_class() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snake_case_types");
        let src = root.join("src/main/java");
        let findings = scan_snake_case_types(&root, &[src]);
        assert_eq!(findings.len(), 2, "expected class + interface");
        assert!(findings.iter().all(|f| f.severity == Severity::Critical));
        assert!(findings
            .iter()
            .all(|f| f.rule_id == SNAKE_CASE_TYPE_RULE_ID));
        let names: Vec<_> = findings.iter().map(|f| f.message.as_str()).collect();
        assert!(names.iter().any(|m| m.contains("user_service")));
        assert!(names.iter().any(|m| m.contains("my_api_client")));
    }

    #[test]
    fn scan_ignores_valid_camel_case_fixture() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/foo_unused_field");
        let findings = scan_snake_case_types(&root, &[root.clone()]);
        assert!(findings.is_empty());
    }
}
