//! Tree-sitter statement and method extraction for Java and XML.

use std::path::Path;

use once_cell::sync::Lazy;
use tree_sitter::{Node, Parser};

use crate::types::{Method, ParseResult, Statement, VarKind, VariableDecl};

// ---- Node type sets (mirrors parser.py constants) ----

const JAVA_STATEMENT_TYPES: &[&str] = &[
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

const JAVA_METHOD_TYPES: &[&str] = &["method_declaration", "constructor_declaration"];

const JAVA_CLASS_LEVEL_TYPES: &[&str] = &["field_declaration", "import_declaration"];

const CLASS_LIKE_TYPES: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

fn is_stmt(kind: &str) -> bool {
    JAVA_STATEMENT_TYPES.contains(&kind)
}
fn is_method(kind: &str) -> bool {
    JAVA_METHOD_TYPES.contains(&kind)
}
fn is_class_level(kind: &str) -> bool {
    JAVA_CLASS_LEVEL_TYPES.contains(&kind)
}
fn is_class_like(kind: &str) -> bool {
    CLASS_LIKE_TYPES.contains(&kind)
}

// ---- Helpers ----

/// UTF-8 slice of the source corresponding to this node, with lossy replacement
/// for invalid bytes (matches Python's `decode("utf-8", errors="replace")`).
fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    let bytes = &source[start..end];
    String::from_utf8_lossy(bytes).into_owned()
}

fn node_stmt(node: Node, source: &[u8], method_name: Option<&str>) -> Statement {
    Statement {
        raw_text: node_text(node, source),
        start_line: (node.start_position().row as u32) + 1,
        end_line: (node.end_position().row as u32) + 1,
        statement_type: node.kind().to_string(),
        method_name: method_name.map(str::to_owned),
    }
}

fn field_name_text(node: Node, source: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(n, source))
}

fn field_name_line(node: Node) -> Option<u32> {
    node.child_by_field_name("name")
        .map(|n| (n.start_position().row as u32) + 1)
}

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

// ---- Java parser (singleton-style, one per thread via Parser::parse) ----

static JAVA_LANG: Lazy<tree_sitter::Language> = Lazy::new(|| tree_sitter_java::LANGUAGE.into());

fn parse_java_tree(source: &[u8]) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser.set_language(&JAVA_LANG).ok()?;
    parser.parse(source, None)
}

// ---- Variable declaration collection ----

fn walk_variables(node: Node, source: &[u8], out: &mut Vec<VariableDecl>) {
    // Don't descend into anonymous/inner class bodies — they have their own scope.
    if node.kind() == "class_body" {
        return;
    }

    let kind = node.kind();
    if kind == "formal_parameter" {
        if let Some(name) = field_name_text(node, source) {
            out.push(VariableDecl {
                name,
                line: field_name_line(node).unwrap_or(1),
                kind: VarKind::Parameter,
            });
        }
        return; // parameters don't have interesting children
    }

    if kind == "local_variable_declaration" {
        for child in children(node) {
            if child.kind() == "variable_declarator" {
                if let Some(name) = field_name_text(child, source) {
                    out.push(VariableDecl {
                        name,
                        line: field_name_line(child).unwrap_or(1),
                        kind: VarKind::Local,
                    });
                }
            }
        }
    }

    if kind == "enhanced_for_statement" {
        if let Some(name) = field_name_text(node, source) {
            out.push(VariableDecl {
                name,
                line: field_name_line(node).unwrap_or(1),
                kind: VarKind::ForVar,
            });
        }
    }

    if kind == "catch_formal_parameter" {
        if let Some(name) = field_name_text(node, source) {
            out.push(VariableDecl {
                name,
                line: field_name_line(node).unwrap_or(1),
                kind: VarKind::Catch,
            });
        }
    }

    if kind == "resource" {
        if let Some(name) = field_name_text(node, source) {
            out.push(VariableDecl {
                name,
                line: field_name_line(node).unwrap_or(1),
                kind: VarKind::Resource,
            });
        }
    }

    if kind == "lambda_expression" {
        if let Some(params) = node.child_by_field_name("parameters") {
            if params.kind() == "identifier" {
                out.push(VariableDecl {
                    name: node_text(params, source),
                    line: (params.start_position().row as u32) + 1,
                    kind: VarKind::Lambda,
                });
            } else {
                for p in children(params) {
                    match p.kind() {
                        "formal_parameter" => {
                            if let Some(name) = field_name_text(p, source) {
                                out.push(VariableDecl {
                                    name,
                                    line: field_name_line(p).unwrap_or(1),
                                    kind: VarKind::Lambda,
                                });
                            }
                        }
                        "identifier" => {
                            out.push(VariableDecl {
                                name: node_text(p, source),
                                line: (p.start_position().row as u32) + 1,
                                kind: VarKind::Lambda,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    for c in children(node) {
        walk_variables(c, source, out);
    }
}

// ---- Statement collection ----

fn walk_statements(node: Node, source: &[u8], method_name: &str, out: &mut Vec<Statement>) {
    if node.kind() == "class_body" {
        return;
    }
    if is_stmt(node.kind()) {
        out.push(node_stmt(node, source, Some(method_name)));
    }
    for c in children(node) {
        walk_statements(c, source, method_name, out);
    }
}

// ---- Method extraction ----

fn extract_method(node: Node, source: &[u8], prefix: &str) -> Method {
    let name_text = field_name_text(node, source).unwrap_or_else(|| "<init>".to_string());
    let qualified = format!("{prefix}{name_text}");

    let mut method = Method {
        name: qualified.clone(),
        start_line: (node.start_position().row as u32) + 1,
        end_line: (node.end_position().row as u32) + 1,
        statements: Vec::new(),
        variables: Vec::new(),
    };

    // Parameters + body locals (sorted by line).
    let mut vars: Vec<VariableDecl> = Vec::new();
    walk_variables(node, source, &mut vars);
    vars.sort_by_key(|v| v.line);
    method.variables = vars;

    if let Some(body) = node.child_by_field_name("body") {
        let mut stmts: Vec<Statement> = Vec::new();
        walk_statements(body, source, &qualified, &mut stmts);
        method.statements = stmts;
    }

    method
}

// ---- Class-level walk ----

fn walk_java_tree(node: Node, source: &[u8], result: &mut ParseResult, prefix: &str) {
    for child in children(node) {
        let kind = child.kind();

        if is_method(kind) {
            let method = extract_method(child, source, prefix);
            let short_name = method.name.rsplit('.').next().unwrap_or("").to_string();
            result.methods.push(method);
            if let Some(body) = child.child_by_field_name("body") {
                let new_prefix = format!("{prefix}{short_name}.");
                find_anon_classes(body, source, result, &new_prefix);
            }
        } else if is_class_level(kind) {
            result
                .class_level_statements
                .push(node_stmt(child, source, None));
        } else if kind == "package_declaration" {
            result
                .class_level_statements
                .push(node_stmt(child, source, None));
        } else if is_class_like(kind) {
            let cls_name = field_name_text(child, source).unwrap_or_else(|| "Unknown".to_string());
            if let Some(body) = child.child_by_field_name("body") {
                let new_prefix = format!("{prefix}{cls_name}.");
                walk_java_tree(body, source, result, &new_prefix);
            }
        } else if kind == "enum_constant" {
            // Enum constants can have their own class body.
            for gc in children(child) {
                if gc.kind() == "class_body" {
                    let cname =
                        field_name_text(child, source).unwrap_or_else(|| "CONST".to_string());
                    let new_prefix = format!("{prefix}{cname}.");
                    walk_java_tree(gc, source, result, &new_prefix);
                    break;
                }
            }
        } else if kind == "static_initializer" {
            let qname = format!("{prefix}<clinit>");
            let mut m = Method {
                name: qname.clone(),
                start_line: (child.start_position().row as u32) + 1,
                end_line: (child.end_position().row as u32) + 1,
                statements: Vec::new(),
                variables: Vec::new(),
            };
            for gc in children(child) {
                if gc.kind() == "block" {
                    let mut stmts: Vec<Statement> = Vec::new();
                    walk_statements(gc, source, &qname, &mut stmts);
                    m.statements = stmts;
                    let mut vars: Vec<VariableDecl> = Vec::new();
                    walk_variables(gc, source, &mut vars);
                    vars.sort_by_key(|v| v.line);
                    m.variables = vars;
                    break;
                }
            }
            result.methods.push(m);
        }
    }
}

fn find_anon_classes(node: Node, source: &[u8], result: &mut ParseResult, prefix: &str) {
    if node.kind() == "class_body" {
        return;
    }
    if node.kind() == "object_creation_expression" {
        for child in children(node) {
            if child.kind() == "class_body" {
                let type_name = node
                    .child_by_field_name("type")
                    .map(|n| node_text(n, source))
                    .unwrap_or_else(|| "Anon".to_string());
                let new_prefix = format!("{prefix}{type_name}.");
                walk_java_tree(child, source, result, &new_prefix);
                return;
            }
        }
    }
    for c in children(node) {
        find_anon_classes(c, source, result, prefix);
    }
}

// ---- Public Java API ----

pub fn parse_java_file(source: &[u8], file_path: &str) -> ParseResult {
    let mut result = ParseResult::new(file_path, "java");
    if let Some(tree) = parse_java_tree(source) {
        walk_java_tree(tree.root_node(), source, &mut result, "");
    }
    result
}

// ---- XML parser ----

static XML_LANG: Lazy<Option<tree_sitter::Language>> = Lazy::new(|| {
    // tree-sitter-xml 0.7 ships two grammars: `LANGUAGE_XML` and `LANGUAGE_DTD`.
    Some(tree_sitter_xml::LANGUAGE_XML.into())
});

fn parse_xml_tree(source: &[u8]) -> Option<tree_sitter::Tree> {
    let lang = XML_LANG.as_ref()?;
    let mut parser = Parser::new();
    parser.set_language(lang).ok()?;
    parser.parse(source, None)
}

fn xml_tag_name(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if matches!(child.kind(), "STag" | "EmptyElemTag") {
            for gc in children(child) {
                if gc.kind() == "Name" {
                    return node_text(gc, source);
                }
            }
        }
    }
    "unknown".to_string()
}

fn xml_tag_text(node: Node, source: &[u8]) -> String {
    for child in children(node) {
        if matches!(child.kind(), "STag" | "EmptyElemTag") {
            return node_text(child, source);
        }
    }
    // Fallback: first line of element text.
    let t = node_text(node, source);
    t.split('\n').next().unwrap_or("").to_string()
}

fn xml_child_elements(node: Node) -> Vec<Node> {
    for child in children(node) {
        if child.kind() == "content" {
            return children(child)
                .into_iter()
                .filter(|c| c.kind() == "element")
                .collect();
        }
    }
    Vec::new()
}

fn walk_xml_tree(node: Node, source: &[u8], method: &mut Method) {
    method.statements.push(Statement {
        raw_text: xml_tag_text(node, source),
        start_line: (node.start_position().row as u32) + 1,
        end_line: (node.end_position().row as u32) + 1,
        statement_type: "xml_element".to_string(),
        method_name: Some(method.name.clone()),
    });
    for c in xml_child_elements(node) {
        walk_xml_tree(c, source, method);
    }
}

pub fn parse_xml_file(source: &[u8], file_path: &str) -> ParseResult {
    let mut result = ParseResult::new(file_path, "xml");
    let tree = match parse_xml_tree(source) {
        Some(t) => t,
        None => return result,
    };
    let root = tree.root_node();

    // Find document root element (skip prolog, comments, etc.).
    let doc_root = match children(root).into_iter().find(|c| c.kind() == "element") {
        Some(n) => n,
        None => return result,
    };

    // Root element itself = class-level statement.
    result.class_level_statements.push(Statement {
        raw_text: xml_tag_text(doc_root, source),
        start_line: (doc_root.start_position().row as u32) + 1,
        end_line: (doc_root.start_position().row as u32) + 1,
        statement_type: "xml_element".to_string(),
        method_name: None,
    });

    // Root's direct element children = "methods" (layout sections).
    for (idx, child) in xml_child_elements(doc_root).into_iter().enumerate() {
        let tag = xml_tag_name(child, source);
        let mut method = Method {
            name: format!("{tag}[{idx}]"),
            start_line: (child.start_position().row as u32) + 1,
            end_line: (child.end_position().row as u32) + 1,
            statements: Vec::new(),
            variables: Vec::new(),
        };
        walk_xml_tree(child, source, &mut method);
        result.methods.push(method);
    }

    result
}

// ---- Public dispatch ----

pub fn parse_file(source: &[u8], file_path: &str) -> Option<ParseResult> {
    let suffix = Path::new(file_path)
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    match suffix.as_deref() {
        Some("java") => Some(parse_java_file(source, file_path)),
        Some("xml") => Some(parse_xml_file(source, file_path)),
        _ => None,
    }
}
