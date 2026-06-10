//! Java AST inventory counters for Spring + Android structural metrics.

use std::collections::BTreeMap;

use sprint_grader_architecture::scanner::ScannedFile;
use sprint_grader_quality::complexity::{analyze_method, cyclomatic_complexity};
use tree_sitter::Node;

use crate::metrics;

const MAIN_SOURCE_MARKERS: &[&str] = &["src/main/java/", "app/src/main/java/"];

const MAPPING_ANNOTATIONS: &[&str] = &[
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PatchMapping",
    "RequestMapping",
];

const REACTIVE_FIELD_TYPES: &[&str] = &[
    "LiveData",
    "MutableLiveData",
    "StateFlow",
    "MutableStateFlow",
    "SharedFlow",
];

const STATEMENT_KINDS: &[&str] = &[
    "expression_statement",
    "local_variable_declaration",
    "return_statement",
    "if_statement",
    "for_statement",
    "enhanced_for_statement",
    "while_statement",
    "do_statement",
    "switch_expression",
    "switch_statement",
    "throw_statement",
    "try_statement",
    "try_with_resources_statement",
    "assert_statement",
    "break_statement",
    "continue_statement",
    "yield_statement",
    "synchronized_statement",
];

#[derive(Debug, Default)]
struct FileScan {
    controller_count: u32,
    service_count: u32,
    entity_count: u32,
    repository_count: u32,
    endpoint_count: u32,
    fragment_count: u32,
    activity_count: u32,
    viewmodel_count: u32,
    room_database_count: u32,
    custom_query_count: u32,
    scheduled_task_count: u32,
    observe_call_count: u32,
    nav_dispatch_count: u32,
    reactive_state_field_count: u32,
    production_loc: u32,
    controller_cc_sum: u64,
    controller_cc_n: u32,
    fragment_cc_sum: u64,
    fragment_cc_n: u32,
    endpoint_stmt_sum: u64,
    endpoint_stmt_n: u32,
}

#[derive(Debug, Default)]
struct RepoScan {
    files: u32,
    inner: FileScan,
}

pub fn is_production_main_source(rel_path: &str) -> bool {
    let norm = rel_path.replace('\\', "/");
    MAIN_SOURCE_MARKERS
        .iter()
        .any(|m| norm.contains(m))
}

/// Scan parsed production Java files and return metric key → value.
pub fn scan_files(files: &[ScannedFile]) -> BTreeMap<String, f64> {
    let mut repo = RepoScan::default();
    for file in files {
        if !is_production_main_source(&file.rel_path) {
            continue;
        }
        repo.files += 1;
        scan_one_file(file, &mut repo.inner);
    }
    finalize(&repo.inner)
}

fn scan_one_file(file: &ScannedFile, acc: &mut FileScan) {
    let source = file.source();
    let root = file.root();
    acc.production_loc += count_code_lines(source);
    walk_node(root, source, &file.rel_path, acc, None);
}

#[derive(Clone, Copy)]
enum ClassKind {
    None,
    Controller,
    Fragment,
}

fn walk_node(node: Node, source: &[u8], rel_path: &str, acc: &mut FileScan, class_kind: Option<ClassKind>) {
    let kind = node.kind();
    if kind == "class_declaration" || kind == "interface_declaration" {
        let class_ann = class_annotations(node, source);
        let extends = extends_simple_name(node, source);
        let class_name = class_simple_name(node, source);

        if class_ann.iter().any(|a| a == "RestController" || a == "Controller") {
            acc.controller_count += 1;
        }
        if class_ann.iter().any(|a| a == "Service") {
            acc.service_count += 1;
        }
        if class_ann.iter().any(|a| a == "Entity") {
            acc.entity_count += 1;
        }
        if class_ann.iter().any(|a| a == "Repository") {
            acc.repository_count += 1;
        }
        if class_ann.iter().any(|a| a == "Database") {
            acc.room_database_count += 1;
        }
        if extends.as_deref() == Some("Fragment") {
            acc.fragment_count += 1;
        }
        if matches!(extends.as_deref(), Some("Activity") | Some("AppCompatActivity")) {
            acc.activity_count += 1;
        }
        if extends.as_deref() == Some("ViewModel")
            || (class_name.ends_with("ViewModel") && extends.as_deref() != Some("AndroidViewModel"))
        {
            acc.viewmodel_count += 1;
        }

        let ck = if class_ann.iter().any(|a| a == "RestController" || a == "Controller") {
            ClassKind::Controller
        } else if extends.as_deref() == Some("Fragment") {
            ClassKind::Fragment
        } else {
            ClassKind::None
        };

        for child in children(node) {
            walk_node(child, source, rel_path, acc, Some(ck));
        }
        return;
    }

    if kind == "method_declaration" || kind == "constructor_declaration" {
        let method_ann = method_annotations(node, source);
        if method_ann.iter().any(|a| MAPPING_ANNOTATIONS.contains(&a.as_str())) {
            acc.endpoint_count += 1;
            let stmts = count_statements_in_method(node);
            acc.endpoint_stmt_sum += stmts as u64;
            acc.endpoint_stmt_n += 1;
        }
        if method_ann.iter().any(|a| a == "Query") {
            acc.custom_query_count += 1;
        }
        if method_ann.iter().any(|a| a == "Scheduled") {
            acc.scheduled_task_count += 1;
        }

        if matches!(class_kind, Some(ClassKind::Controller)) && kind == "method_declaration" {
            let cc = cyclomatic_complexity(node, source) as u64;
            acc.controller_cc_sum += cc;
            acc.controller_cc_n += 1;
        }
        if matches!(class_kind, Some(ClassKind::Fragment)) && kind == "method_declaration" {
            let m = analyze_method(node, source, rel_path);
            acc.fragment_cc_sum += m.cyclomatic_complexity as u64;
            acc.fragment_cc_n += 1;
        }

        for child in children(node) {
            walk_node(child, source, rel_path, acc, class_kind);
        }
        return;
    }

    if kind == "field_declaration" {
        if let Some(type_name) = field_type_simple_name(node, source) {
            if REACTIVE_FIELD_TYPES.contains(&type_name.as_str()) {
                acc.reactive_state_field_count += field_declarator_count(node) as u32;
            }
        }
    }

    if kind == "method_invocation" {
        match method_invocation_name(node, source).as_deref() {
            Some("observe") => acc.observe_call_count += 1,
            Some("navigate") => acc.nav_dispatch_count += 1,
            _ => {}
        }
    }

    for child in children(node) {
        walk_node(child, source, rel_path, acc, class_kind);
    }
}

fn finalize(acc: &FileScan) -> BTreeMap<String, f64> {
    let screens = (acc.fragment_count + acc.activity_count).max(1) as f64;
    let fragments = acc.fragment_count.max(1) as f64;

    let mut out = BTreeMap::new();
    out.insert(metrics::CONTROLLER_COUNT.into(), acc.controller_count as f64);
    out.insert(metrics::SERVICE_COUNT.into(), acc.service_count as f64);
    out.insert(metrics::ENTITY_COUNT.into(), acc.entity_count as f64);
    out.insert(metrics::REPOSITORY_COUNT.into(), acc.repository_count as f64);
    out.insert(metrics::ENDPOINT_COUNT.into(), acc.endpoint_count as f64);
    out.insert(metrics::FRAGMENT_COUNT.into(), acc.fragment_count as f64);
    out.insert(metrics::ACTIVITY_COUNT.into(), acc.activity_count as f64);
    out.insert(metrics::VIEWMODEL_COUNT.into(), acc.viewmodel_count as f64);
    out.insert(
        metrics::ROOM_DATABASE_COUNT.into(),
        acc.room_database_count as f64,
    );
    out.insert(
        metrics::CUSTOM_QUERY_COUNT.into(),
        acc.custom_query_count as f64,
    );
    out.insert(
        metrics::SCHEDULED_TASK_COUNT.into(),
        acc.scheduled_task_count as f64,
    );
    out.insert(
        metrics::OBSERVE_CALL_COUNT.into(),
        acc.observe_call_count as f64,
    );
    out.insert(
        metrics::NAV_DISPATCH_COUNT.into(),
        acc.nav_dispatch_count as f64,
    );
    out.insert(
        metrics::REACTIVE_STATE_FIELD_COUNT.into(),
        acc.reactive_state_field_count as f64,
    );
    out.insert(metrics::PRODUCTION_LOC.into(), acc.production_loc as f64);
    out.insert(
        metrics::REACTIVE_WIRING_DENSITY.into(),
        acc.observe_call_count as f64 / screens,
    );
    out.insert(
        metrics::NAV_DISPATCH_DENSITY.into(),
        acc.nav_dispatch_count as f64 / fragments,
    );
    out.insert(
        metrics::AVG_CC_PER_CONTROLLER.into(),
        if acc.controller_cc_n == 0 {
            0.0
        } else {
            acc.controller_cc_sum as f64 / f64::from(acc.controller_cc_n)
        },
    );
    out.insert(
        metrics::AVG_CC_PER_FRAGMENT.into(),
        if acc.fragment_cc_n == 0 {
            0.0
        } else {
            acc.fragment_cc_sum as f64 / f64::from(acc.fragment_cc_n)
        },
    );
    out.insert(
        metrics::AVG_STATEMENTS_PER_ENDPOINT.into(),
        if acc.endpoint_stmt_n == 0 {
            0.0
        } else {
            acc.endpoint_stmt_sum as f64 / f64::from(acc.endpoint_stmt_n)
        },
    );
    out
}

fn method_invocation_name(inv: Node, source: &[u8]) -> Option<String> {
    for c in children(inv) {
        if c.kind() == "identifier" {
            return Some(node_text(c, source));
        }
    }
    None
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

fn last_segment(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s).trim()
}

fn annotation_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "marker_annotation" | "annotation" => {
            for c in children(node) {
                let k = c.kind();
                if k == "identifier" || k == "scoped_identifier" || k == "type_identifier" {
                    return Some(last_segment(&node_text(c, source)).to_string());
                }
            }
            None
        }
        _ => None,
    }
}

fn class_annotations(class: Node, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in children(class) {
        if c.kind() != "modifiers" {
            continue;
        }
        for m in children(c) {
            if let Some(a) = annotation_name(m, source) {
                out.push(a);
            }
        }
    }
    out
}

fn method_annotations(method: Node, source: &[u8]) -> Vec<String> {
    class_annotations(method, source)
}

fn extends_simple_name(class: Node, source: &[u8]) -> Option<String> {
    for c in children(class) {
        if c.kind() != "superclass" {
            continue;
        }
        for sub in children(c) {
            if let Some(name) = simple_type_name(sub, source) {
                return Some(name);
            }
        }
    }
    None
}

fn simple_type_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "scoped_identifier" | "scoped_type_identifier" => {
            Some(last_segment(&node_text(node, source)).to_string())
        }
        "generic_type" => {
            for c in children(node) {
                if let Some(n) = simple_type_name(c, source) {
                    return Some(n);
                }
            }
            None
        }
        _ => {
            for c in children(node) {
                if let Some(n) = simple_type_name(c, source) {
                    return Some(n);
                }
            }
            None
        }
    }
}

fn class_simple_name(class: Node, source: &[u8]) -> String {
    for c in children(class) {
        if c.kind() == "identifier" {
            return node_text(c, source);
        }
    }
    String::new()
}

fn field_type_simple_name(field: Node, source: &[u8]) -> Option<String> {
    for c in children(field) {
        if c.kind().ends_with("_type") || c.kind() == "generic_type" || c.kind() == "type_identifier"
        {
            return simple_type_name(c, source);
        }
    }
    None
}

fn field_declarator_count(field: Node) -> usize {
    children(field)
        .into_iter()
        .filter(|c| c.kind() == "variable_declarator")
        .count()
}

fn count_statements_in_method(method: Node) -> u32 {
    let body = match children(method).into_iter().find(|c| c.kind() == "block") {
        Some(b) => b,
        None => return 0,
    };
    let mut n = 0u32;
    count_statements(body, &mut n);
    n
}

fn count_statements(node: Node, n: &mut u32) {
    if STATEMENT_KINDS.contains(&node.kind()) {
        *n += 1;
    }
    for c in children(node) {
        count_statements(c, n);
    }
}

fn count_code_lines(source: &[u8]) -> u32 {
    let text = String::from_utf8_lossy(source);
    text.lines()
        .filter(|line| {
            let t = line.trim();
            !t.is_empty() && !t.starts_with("//") && t != "{" && t != "}"
        })
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_grader_architecture::scanner::ScannedFile;

    #[test]
    fn production_path_gate() {
        assert!(is_production_main_source(
            "src/main/java/com/x/App.java"
        ));
        assert!(is_production_main_source(
            "app/src/main/java/com/x/App.java"
        ));
        assert!(!is_production_main_source("src/test/java/com/x/AppTest.java"));
    }

    #[test]
    fn spring_controller_and_endpoint_counts() {
        let src = br#"package com.x.api;
import org.springframework.web.bind.annotation.*;
@RestController
@RequestMapping("/api")
public class UserController {
    @GetMapping("/users")
    public List<User> list() {
        if (true) { return null; }
        return null;
    }
    @PostMapping("/users")
    public User create() { return null; }
}
"#;
        let f = ScannedFile::from_inline("src/main/java/com/x/UserController.java", src).unwrap();
        let m = scan_files(&[f]);
        assert_eq!(m[metrics::CONTROLLER_COUNT], 1.0);
        assert_eq!(m[metrics::ENDPOINT_COUNT], 2.0);
        assert!(m[metrics::AVG_STATEMENTS_PER_ENDPOINT] >= 1.0);
    }

    #[test]
    fn android_observe_and_livedata_counts() {
        let src = br#"package com.x.ui;
import androidx.fragment.app.Fragment;
import androidx.lifecycle.LiveData;
import androidx.lifecycle.MutableLiveData;
import androidx.lifecycle.ViewModel;
public class HomeFragment extends Fragment {
    private HomeViewModel viewModel;
    void bind() {
        viewModel.getUsers().observe(getViewLifecycleOwner(), x -> {});
        findNavController().navigate(R.id.detail);
    }
}
class HomeViewModel extends ViewModel {
    private final MutableLiveData<String> users = new MutableLiveData<>();
    public LiveData<String> getUsers() { return users; }
}
"#;
        let f = ScannedFile::from_inline("app/src/main/java/com/x/HomeFragment.java", src).unwrap();
        let m = scan_files(&[f]);
        assert_eq!(m[metrics::FRAGMENT_COUNT], 1.0);
        assert_eq!(m[metrics::VIEWMODEL_COUNT], 1.0);
        assert_eq!(m[metrics::OBSERVE_CALL_COUNT], 1.0);
        assert_eq!(m[metrics::NAV_DISPATCH_COUNT], 1.0);
        assert!(m[metrics::REACTIVE_STATE_FIELD_COUNT] >= 1.0);
        assert!(m[metrics::REACTIVE_WIRING_DENSITY] > 0.0);
    }
}
