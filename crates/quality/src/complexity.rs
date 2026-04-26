//! Cyclomatic + cognitive complexity via tree-sitter. Mirrors `src/quality/complexity.py`.

use once_cell::sync::Lazy;
use tree_sitter::{Node, Parser};

static JAVA_LANG: Lazy<tree_sitter::Language> = Lazy::new(|| tree_sitter_java::LANGUAGE.into());

const CC_NODE_TYPES: &[&str] = &[
    "if_statement",
    "for_statement",
    "enhanced_for_statement",
    "while_statement",
    "do_statement",
    "catch_clause",
    "conditional_expression",
    "ternary_expression",
];

const CC_OPERATORS: &[&str] = &["&&", "||"];

const COGNITIVE_INCREMENT_NODES: &[&str] = &[
    "if_statement",
    "for_statement",
    "enhanced_for_statement",
    "while_statement",
    "do_statement",
    "catch_clause",
    "conditional_expression",
    "ternary_expression",
    "switch_expression",
];

const COGNITIVE_NESTING_NODES: &[&str] = &[
    "if_statement",
    "for_statement",
    "enhanced_for_statement",
    "while_statement",
    "do_statement",
    "catch_clause",
    "lambda_expression",
];

#[derive(Debug, Clone)]
pub struct MethodMetrics {
    pub file_path: String,
    pub class_name: String,
    pub method_name: String,
    pub loc: i64,
    pub cyclomatic_complexity: i64,
    pub cognitive_complexity: i64,
    pub parameter_count: i64,
    pub max_nesting_depth: i64,
    pub return_count: i64,
}

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

pub fn cyclomatic_complexity(node: Node, source: &[u8]) -> i64 {
    let mut cc = 1i64;
    fn walk(node: Node, _source: &[u8], cc: &mut i64) {
        let kind = node.kind();
        if CC_NODE_TYPES.contains(&kind) {
            *cc += 1;
        } else if kind == "binary_expression" {
            for child in children(node) {
                if CC_OPERATORS.contains(&child.kind()) {
                    *cc += 1;
                }
            }
        } else if kind == "switch_block_statement_group" {
            *cc += 1;
        }
        for c in children(node) {
            walk(c, _source, cc);
        }
    }
    walk(node, source, &mut cc);
    cc
}

pub fn cognitive_complexity(node: Node, source: &[u8]) -> i64 {
    let mut total = 0i64;
    fn walk(node: Node, _source: &[u8], nesting: i64, total: &mut i64) {
        let kind = node.kind();
        if COGNITIVE_INCREMENT_NODES.contains(&kind) {
            *total += 1 + nesting;
        }
        if kind == "binary_expression" {
            for child in children(node) {
                let k = child.kind();
                if k == "&&" || k == "||" {
                    *total += 1;
                }
            }
        }
        let new_nesting = nesting
            + if COGNITIVE_NESTING_NODES.contains(&kind) {
                1
            } else {
                0
            };
        for c in children(node) {
            walk(c, _source, new_nesting, total);
        }
    }
    walk(node, source, 0, &mut total);
    total
}

pub fn max_nesting_depth(node: Node, source: &[u8]) -> i64 {
    let mut max_depth = 0i64;
    fn walk(node: Node, _source: &[u8], depth: i64, max_depth: &mut i64) {
        let d = if COGNITIVE_NESTING_NODES.contains(&node.kind()) {
            let nd = depth + 1;
            if nd > *max_depth {
                *max_depth = nd;
            }
            nd
        } else {
            depth
        };
        for c in children(node) {
            walk(c, _source, d, max_depth);
        }
    }
    walk(node, source, 0, &mut max_depth);
    max_depth
}

fn count_parameters(node: Node) -> i64 {
    for child in children(node) {
        if child.kind() == "formal_parameters" {
            return children(child)
                .into_iter()
                .filter(|c| c.kind() == "formal_parameter")
                .count() as i64;
        }
    }
    0
}

fn count_returns(node: Node) -> i64 {
    let mut n = 0i64;
    fn walk(node: Node, n: &mut i64) {
        if node.kind() == "return_statement" {
            *n += 1;
        }
        for c in children(node) {
            walk(c, n);
        }
    }
    walk(node, &mut n);
    n
}

fn count_loc(node: Node) -> i64 {
    (node.end_position().row as i64) - (node.start_position().row as i64) + 1
}

fn find_class_name(node: Node, source: &[u8]) -> String {
    let mut cur = node.parent();
    while let Some(n) = cur {
        let k = n.kind();
        if k == "class_declaration" || k == "interface_declaration" || k == "enum_declaration" {
            for child in children(n) {
                if child.kind() == "identifier" {
                    return node_text(child, source);
                }
            }
        }
        cur = n.parent();
    }
    "<unknown>".to_string()
}

pub fn analyze_method(node: Node, source: &[u8], file_path: &str) -> MethodMetrics {
    let mut name = "<unknown>".to_string();
    for child in children(node) {
        if child.kind() == "identifier" {
            name = node_text(child, source);
            break;
        }
    }
    MethodMetrics {
        file_path: file_path.to_string(),
        class_name: find_class_name(node, source),
        method_name: name,
        loc: count_loc(node),
        cyclomatic_complexity: cyclomatic_complexity(node, source),
        cognitive_complexity: cognitive_complexity(node, source),
        parameter_count: count_parameters(node),
        max_nesting_depth: max_nesting_depth(node, source),
        return_count: count_returns(node),
    }
}

pub fn analyze_file(file_path: &str, source: &[u8]) -> Vec<MethodMetrics> {
    let mut parser = Parser::new();
    if parser.set_language(&JAVA_LANG).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut out: Vec<MethodMetrics> = Vec::new();
    fn walk(node: Node, source: &[u8], file_path: &str, out: &mut Vec<MethodMetrics>) {
        let k = node.kind();
        if k == "method_declaration" || k == "constructor_declaration" {
            out.push(analyze_method(node, source, file_path));
        }
        for c in children(node) {
            walk(c, source, file_path, out);
        }
    }
    walk(tree.root_node(), source, file_path, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_method_has_cc_1() {
        let src = br#"class A { int f() { return 1; } }"#;
        let methods = analyze_file("A.java", src);
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].cyclomatic_complexity, 1);
        assert_eq!(methods[0].return_count, 1);
    }

    #[test]
    fn branching_increases_cc_and_cognitive() {
        let src = br#"class A {
            int f(int x) {
                if (x > 0 && x < 10) {
                    for (int i = 0; i < x; i++) {
                        if (i == 5) return i;
                    }
                }
                return 0;
            }
        }"#;
        let methods = analyze_file("A.java", src);
        assert_eq!(methods.len(), 1);
        let m = &methods[0];
        // CC: 1 base + if (outer) + && + for + if (inner) = 5
        assert_eq!(m.cyclomatic_complexity, 5);
        assert!(m.cognitive_complexity >= m.cyclomatic_complexity);
        assert_eq!(m.return_count, 2);
        assert_eq!(m.parameter_count, 1);
        assert!(m.max_nesting_depth >= 2);
    }
}
