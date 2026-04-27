//! AST-driven architecture rules (T-P3.1).
//!
//! The legacy `[[layers]]` / `[[forbidden]]` rules see only the file's
//! `package` and its `import` lines. That is enough to spot a controller
//! reaching across the layered boundary via an explicit import, but blind
//! to the architectural sins that hide inside class bodies:
//!
//! - DI of a forbidden type via field or constructor parameter (e.g. an
//!   `@RestController` holding a `*Repository`),
//! - a method on a matched class returning a forbidden type (DTO leak),
//! - a method on a matched class calling into a forbidden API,
//! - methods that are simply too long on classes where they shouldn't be
//!   (controllers, ViewModels — fat-method anti-pattern).
//!
//! Rules are described in `architecture.toml` under `[[ast_rule]]` blocks
//! and applied by walking the tree-sitter-java AST. Each emitted violation
//! carries `(start_line, end_line)` so the attribution stage can blame the
//! offending lines specifically — not the whole file.
//!
//! tree-sitter-java is already a workspace dependency (`crates/quality`,
//! `crates/survival`) so this introduces no new transitive crates.
//!
//! ### Class matching
//!
//! A rule's `class_match` is an AND of any subset of:
//! - `annotation` — the class carries an annotation whose name matches
//!   (compared as either `Foo` or `@Foo`).
//! - `extends` — the superclass identifier matches.
//! - `implements` — one of the implemented interfaces matches.
//! - `package_glob` — the file's package matches the glob (re-using
//!   `crate::glob::PackagePattern`).
//!
//! All checks are exact-name comparisons against the *last* identifier
//! component (so `Activity` matches both `android.app.Activity` and a
//! locally imported `Activity`). This is intentional — qualified-name
//! resolution would require a full classpath, which isn't available.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use tree_sitter::{Node, Parser};

use crate::checker::{Violation, ViolationKind};
use crate::glob::PackagePattern;

static JAVA_LANG: Lazy<tree_sitter::Language> = Lazy::new(|| tree_sitter_java::LANGUAGE.into());

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawClassMatch {
    pub annotation: Option<String>,
    pub extends: Option<String>,
    pub implements: Option<String>,
    pub package_glob: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawAstRule {
    pub name: String,
    #[serde(default)]
    pub class_match: RawClassMatch,
    pub kind: String,
    /// Used by `forbidden_field_type`, `forbidden_constructor_param`,
    /// `forbidden_return_type`.
    #[serde(default)]
    pub type_regex: Option<String>,
    /// Used by `forbidden_method_call`. Matched against the *callee* string
    /// reconstructed from the AST (e.g. `userRepository.findAll`).
    #[serde(default)]
    pub call_regex: Option<String>,
    /// Used by `max_method_statements`.
    #[serde(default)]
    pub max: Option<usize>,
    #[serde(default = "default_severity")]
    pub severity: String,
}

fn default_severity() -> String {
    "WARNING".to_string()
}

#[derive(Debug, Clone)]
pub struct ClassMatcher {
    pub annotation: Option<String>,
    pub extends: Option<String>,
    pub implements: Option<String>,
    pub package_glob: Option<PackagePattern>,
}

#[derive(Debug, Clone)]
pub enum AstRuleKind {
    ForbiddenFieldType { type_regex: Regex },
    ForbiddenConstructorParam { type_regex: Regex },
    ForbiddenMethodCall { call_regex: Regex },
    ForbiddenReturnType { type_regex: Regex },
    MaxMethodStatements { max: usize },
}

impl AstRuleKind {
    pub fn label(&self) -> &'static str {
        match self {
            AstRuleKind::ForbiddenFieldType { .. } => "ast_forbidden_field_type",
            AstRuleKind::ForbiddenConstructorParam { .. } => "ast_forbidden_constructor_param",
            AstRuleKind::ForbiddenMethodCall { .. } => "ast_forbidden_method_call",
            AstRuleKind::ForbiddenReturnType { .. } => "ast_forbidden_return_type",
            AstRuleKind::MaxMethodStatements { .. } => "ast_max_method_statements",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AstRule {
    pub name: String,
    pub class_match: ClassMatcher,
    pub kind: AstRuleKind,
    pub severity: String,
}

impl AstRule {
    pub fn from_raw(raw: RawAstRule) -> anyhow::Result<Self> {
        let class_match = ClassMatcher {
            annotation: raw.class_match.annotation,
            extends: raw.class_match.extends,
            implements: raw.class_match.implements,
            package_glob: raw.class_match.package_glob.as_deref().map(PackagePattern::new),
        };
        let kind = match raw.kind.as_str() {
            "forbidden_field_type" => AstRuleKind::ForbiddenFieldType {
                type_regex: compile_regex(raw.type_regex, "type_regex", &raw.name)?,
            },
            "forbidden_constructor_param" => AstRuleKind::ForbiddenConstructorParam {
                type_regex: compile_regex(raw.type_regex, "type_regex", &raw.name)?,
            },
            "forbidden_method_call" => AstRuleKind::ForbiddenMethodCall {
                call_regex: compile_regex(raw.call_regex, "call_regex", &raw.name)?,
            },
            "forbidden_return_type" => AstRuleKind::ForbiddenReturnType {
                type_regex: compile_regex(raw.type_regex, "type_regex", &raw.name)?,
            },
            "max_method_statements" => AstRuleKind::MaxMethodStatements {
                max: raw.max.ok_or_else(|| {
                    anyhow::anyhow!("ast_rule '{}' kind=max_method_statements requires `max`", raw.name)
                })?,
            },
            other => {
                anyhow::bail!("ast_rule '{}' has unknown kind '{}'", raw.name, other)
            }
        };
        Ok(AstRule {
            name: raw.name,
            class_match,
            kind,
            severity: raw.severity,
        })
    }
}

fn compile_regex(value: Option<String>, field: &str, rule_name: &str) -> anyhow::Result<Regex> {
    let s = value.ok_or_else(|| {
        anyhow::anyhow!("ast_rule '{rule_name}' requires `{field}` for this kind")
    })?;
    Ok(Regex::new(&s)?)
}

/// Top-level entry: parse one Java file, evaluate every rule, return
/// violations. `package_name` is the file's declared package (used by
/// `package_glob` matchers); `rel_path` is recorded on each violation row.
pub fn check_java_file(
    rules: &[AstRule],
    rel_path: &str,
    package_name: &str,
    source: &[u8],
) -> Vec<Violation> {
    if rules.is_empty() {
        return Vec::new();
    }
    let mut parser = Parser::new();
    if parser.set_language(&JAVA_LANG).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    visit_classes(tree.root_node(), source, &mut |class_node| {
        let info = ClassInfo::new(class_node, source);
        for rule in rules {
            if !class_matches(&rule.class_match, &info, package_name) {
                continue;
            }
            apply_rule(rule, &info, source, rel_path, &mut out);
        }
    });
    out
}

fn visit_classes<F: FnMut(Node)>(node: Node, source: &[u8], cb: &mut F) {
    let kind = node.kind();
    if kind == "class_declaration" || kind == "interface_declaration" || kind == "enum_declaration"
    {
        cb(node);
    }
    for child in children(node) {
        visit_classes(child, source, cb);
    }
}

#[derive(Debug)]
struct ClassInfo<'a> {
    node: Node<'a>,
    name: String,
    annotations: Vec<String>,
    extends: Option<String>,
    implements: Vec<String>,
}

impl<'a> ClassInfo<'a> {
    fn new(node: Node<'a>, source: &[u8]) -> Self {
        let mut name = String::from("<anonymous>");
        let mut annotations: Vec<String> = Vec::new();
        let mut extends: Option<String> = None;
        let mut implements: Vec<String> = Vec::new();

        for c in children(node) {
            match c.kind() {
                "identifier" => {
                    if name == "<anonymous>" {
                        name = node_text(c, source);
                    }
                }
                "modifiers" => {
                    for m in children(c) {
                        if let Some(a) = annotation_name(m, source) {
                            annotations.push(a);
                        }
                    }
                }
                "superclass" => {
                    extends = simple_type_name(c, source);
                }
                "super_interfaces" => {
                    for it in children(c) {
                        // type_list → child types
                        if it.kind() == "type_list" {
                            for ty in children(it) {
                                if let Some(n) = simple_type_name(ty, source) {
                                    implements.push(n);
                                }
                            }
                        } else if let Some(n) = simple_type_name(it, source) {
                            implements.push(n);
                        }
                    }
                }
                _ => {}
            }
        }

        ClassInfo {
            node,
            name,
            annotations,
            extends,
            implements,
        }
    }

    fn class_body(&self) -> Option<Node<'a>> {
        children(self.node).into_iter().find(|c| c.kind() == "class_body")
    }
}

fn class_matches(matcher: &ClassMatcher, info: &ClassInfo, package_name: &str) -> bool {
    if let Some(want) = matcher.annotation.as_deref() {
        let want = want.trim_start_matches('@');
        if !info.annotations.iter().any(|a| a == want) {
            return false;
        }
    }
    if let Some(want) = matcher.extends.as_deref() {
        if info.extends.as_deref() != Some(want) {
            return false;
        }
    }
    if let Some(want) = matcher.implements.as_deref() {
        if !info.implements.iter().any(|i| i == want) {
            return false;
        }
    }
    if let Some(p) = matcher.package_glob.as_ref() {
        if !p.matches(package_name) {
            return false;
        }
    }
    true
}

fn apply_rule(
    rule: &AstRule,
    info: &ClassInfo,
    source: &[u8],
    rel_path: &str,
    out: &mut Vec<Violation>,
) {
    let body = match info.class_body() {
        Some(b) => b,
        None => return,
    };
    match &rule.kind {
        AstRuleKind::ForbiddenFieldType { type_regex } => {
            for member in children(body) {
                if member.kind() != "field_declaration" {
                    continue;
                }
                let ty = type_text_of_field(member, source);
                if let Some(t) = ty {
                    if type_regex.is_match(&t) {
                        out.push(make_violation(
                            rel_path,
                            rule,
                            &format!("{}::{}", info.name, t),
                            member,
                        ));
                    }
                }
            }
        }
        AstRuleKind::ForbiddenConstructorParam { type_regex } => {
            for member in children(body) {
                if member.kind() != "constructor_declaration" {
                    continue;
                }
                for param in formal_parameters(member) {
                    let ty = type_text_of_param(param, source);
                    if let Some(t) = ty {
                        if type_regex.is_match(&t) {
                            out.push(make_violation(
                                rel_path,
                                rule,
                                &format!("{}::ctor({})", info.name, t),
                                param,
                            ));
                        }
                    }
                }
            }
        }
        AstRuleKind::ForbiddenMethodCall { call_regex } => {
            for member in children(body) {
                if member.kind() != "method_declaration" && member.kind() != "constructor_declaration" {
                    continue;
                }
                let mut hits: Vec<(String, Node)> = Vec::new();
                collect_method_invocations(member, source, &mut hits);
                for (callee, n) in hits {
                    if call_regex.is_match(&callee) {
                        out.push(make_violation(
                            rel_path,
                            rule,
                            &format!("{}::call({})", info.name, callee),
                            n,
                        ));
                    }
                }
            }
        }
        AstRuleKind::ForbiddenReturnType { type_regex } => {
            for member in children(body) {
                if member.kind() != "method_declaration" {
                    continue;
                }
                let ty = method_return_type(member, source);
                if let Some(t) = ty {
                    if type_regex.is_match(&t) {
                        out.push(make_violation(
                            rel_path,
                            rule,
                            &format!("{}::return({})", info.name, t),
                            member,
                        ));
                    }
                }
            }
        }
        AstRuleKind::MaxMethodStatements { max } => {
            for member in children(body) {
                if member.kind() != "method_declaration" {
                    continue;
                }
                let count = count_method_statements(member);
                if count > *max {
                    let m_name = method_name(member, source).unwrap_or_else(|| "<anon>".into());
                    out.push(make_violation(
                        rel_path,
                        rule,
                        &format!("{}::{}#{}stmts", info.name, m_name, count),
                        member,
                    ));
                }
            }
        }
    }
}

fn make_violation(rel_path: &str, rule: &AstRule, descriptor: &str, anchor: Node) -> Violation {
    let start = anchor.start_position().row as u32 + 1;
    let end = anchor.end_position().row as u32 + 1;
    // Suffix the descriptor with the start line so two structurally
    // identical occurrences in the same file (e.g. the same forbidden call
    // in two methods) don't collide on the composite PK
    // `(repo, sprint, file, rule_name, offending_import)`.
    let descriptor = format!("{descriptor}@L{start}");
    Violation {
        file_path: rel_path.to_string(),
        rule_name: rule.name.clone(),
        kind: ViolationKind::AstRule(rule.kind.label().to_string()),
        offending_import: descriptor,
        start_line: Some(start),
        end_line: Some(end),
    }
}

// ---------- AST helpers ----------

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

/// Pull the annotation name out of a `marker_annotation` / `annotation` node;
/// strips the leading `@` and any qualifier (`org.springframework.web.bind.annotation.RestController`
/// → `RestController`).
fn annotation_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "marker_annotation" | "annotation" => {
            // Children include `@`, then a `name` (identifier / scoped_identifier).
            for c in children(node) {
                let k = c.kind();
                if k == "identifier" || k == "scoped_identifier" || k == "type_identifier" {
                    let raw = node_text(c, source);
                    return Some(last_identifier_segment(&raw).to_string());
                }
            }
            None
        }
        _ => None,
    }
}

fn last_identifier_segment(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s).trim()
}

/// For `superclass` and similar wrappers: drill through to the `type_identifier`
/// or `scoped_type_identifier` child and return its leaf name.
fn simple_type_name(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "scoped_identifier" | "scoped_type_identifier" => {
            Some(last_identifier_segment(&node_text(node, source)).to_string())
        }
        "generic_type" => {
            // First child is the underlying type name (with type-params after).
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

fn type_text_of_field(node: Node, source: &[u8]) -> Option<String> {
    // field_declaration: modifiers? type variable_declarator (',' variable_declarator)* ';'
    for c in children(node) {
        let k = c.kind();
        if k == "modifiers" {
            continue;
        }
        if k.ends_with("_type") || k == "type_identifier" || k == "scoped_type_identifier"
            || k == "generic_type" || k == "array_type"
        {
            return simple_type_name(c, source);
        }
    }
    None
}

fn formal_parameters(method_or_ctor: Node) -> Vec<Node> {
    let mut out = Vec::new();
    for c in children(method_or_ctor) {
        if c.kind() == "formal_parameters" {
            for p in children(c) {
                if p.kind() == "formal_parameter" {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn type_text_of_param(node: Node, source: &[u8]) -> Option<String> {
    // formal_parameter: modifiers? type identifier dims?
    for c in children(node) {
        let k = c.kind();
        if k == "modifiers" {
            continue;
        }
        if k.ends_with("_type") || k == "type_identifier" || k == "scoped_type_identifier"
            || k == "generic_type" || k == "array_type"
        {
            return simple_type_name(c, source);
        }
    }
    None
}

fn method_return_type(method: Node, source: &[u8]) -> Option<String> {
    // method_declaration: modifiers? type_parameters? type identifier formal_parameters ...
    // The return type is the first type-shaped child after `modifiers` /
    // `type_parameters`. `void_type` is excluded — there is no type name
    // to match a blacklist against.
    for c in children(method) {
        let k = c.kind();
        if k == "modifiers" || k == "type_parameters" {
            continue;
        }
        if k == "void_type" {
            return None;
        }
        if k.ends_with("_type")
            || k == "type_identifier"
            || k == "scoped_type_identifier"
            || k == "generic_type"
            || k == "array_type"
        {
            return simple_type_name(c, source);
        }
    }
    None
}

fn method_name(method: Node, source: &[u8]) -> Option<String> {
    // method_declaration: ... identifier formal_parameters ...
    // We take the first identifier child that comes *after* modifiers/type_parameters/return-type.
    let mut saw_type = false;
    for c in children(method) {
        let k = c.kind();
        if k == "modifiers" || k == "type_parameters" {
            continue;
        }
        if !saw_type {
            // Skip the return-type slot.
            if k.ends_with("_type")
                || k == "void_type"
                || k == "type_identifier"
                || k == "scoped_type_identifier"
                || k == "generic_type"
                || k == "array_type"
            {
                saw_type = true;
                continue;
            }
        }
        if k == "identifier" {
            return Some(node_text(c, source));
        }
    }
    None
}

fn count_method_statements(method: Node) -> usize {
    // The method body is a `block`; count direct statement-kind children.
    for c in children(method) {
        if c.kind() == "block" {
            return children(c)
                .into_iter()
                .filter(|n| {
                    let k = n.kind();
                    k.ends_with("_statement")
                        || k == "local_variable_declaration"
                        || k == "expression_statement"
                })
                .count();
        }
    }
    0
}

/// Collect every `method_invocation` inside `node`, returning the
/// reconstructed dotted callee plus the call node (for line range).
/// Examples:
/// - `userRepo.findAll()`       → `userRepo.findAll`
/// - `this.repo.find(...)`      → `this.repo.find`
/// - `User.from(...)`           → `User.from`
/// - bare `helper()` (same-class) → `helper`
fn collect_method_invocations<'a>(
    node: Node<'a>,
    source: &[u8],
    out: &mut Vec<(String, Node<'a>)>,
) {
    if node.kind() == "method_invocation" {
        let callee = reconstruct_callee(node, source);
        out.push((callee, node));
        // fall through: nested calls inside arguments still need to be seen
    }
    for c in children(node) {
        collect_method_invocations(c, source, out);
    }
}

fn reconstruct_callee(invocation: Node, source: &[u8]) -> String {
    // method_invocation: <expr>(args). Take the source slice from the
    // start of the invocation up to the start of the argument list — that
    // yields exactly `userRepository.findAll`, `helper`, `User.from`,
    // etc. without re-implementing tree-sitter-java's grammar quirks.
    let inv_start = invocation.start_byte();
    let arg_start = children(invocation)
        .into_iter()
        .find(|c| c.kind() == "argument_list")
        .map(|n| n.start_byte())
        .unwrap_or(invocation.end_byte());
    let end = arg_start.min(source.len()).max(inv_start);
    String::from_utf8_lossy(&source[inv_start..end])
        .trim_end_matches('(')
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(toml_body: &str) -> AstRule {
        let raw: RawAstRule = toml::from_str(toml_body).unwrap();
        AstRule::from_raw(raw).unwrap()
    }

    #[test]
    fn forbidden_field_type_fires_on_repository_in_controller() {
        let r = rule(
            r#"
            name = "controller-no-repo-field"
            class_match.annotation = "RestController"
            kind = "forbidden_field_type"
            type_regex = ".*Repository$"
            severity = "WARNING"
            "#,
        );
        let src = r#"
            package com.x.controller;
            import com.x.repo.UserRepository;
            @RestController
            public class UserController {
                private final UserRepository repo;
                public UserController(UserRepository r) { this.repo = r; }
                public Object listAll() { return repo.findAll(); }
            }
        "#;
        let v = check_java_file(&[r], "User.java", "com.x.controller", src.as_bytes());
        assert!(v.iter().any(|x| x.rule_name == "controller-no-repo-field"
            && x.start_line.is_some()
            && x.end_line.is_some()));
    }

    #[test]
    fn forbidden_constructor_param_fires_independently_of_field() {
        let r = rule(
            r#"
            name = "controller-no-repo-ctor-param"
            class_match.annotation = "RestController"
            kind = "forbidden_constructor_param"
            type_regex = ".*Repository$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                public C(UserRepository r) {}
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind.as_str(), "ast_forbidden_constructor_param");
    }

    #[test]
    fn forbidden_method_call_picks_up_qualified_callee() {
        let r = rule(
            r#"
            name = "controller-no-repo-call"
            class_match.annotation = "RestController"
            kind = "forbidden_method_call"
            call_regex = "Repository\\.findAll$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                private UserRepository userRepository;
                public Object all() { return userRepository.findAll(); }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.iter().any(|x| x.rule_name == "controller-no-repo-call"));
    }

    #[test]
    fn forbidden_return_type_fires_on_dto_leak() {
        let r = rule(
            r#"
            name = "controller-no-entity-return"
            class_match.annotation = "RestController"
            kind = "forbidden_return_type"
            type_regex = "^User$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                public User getUser() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind.as_str(), "ast_forbidden_return_type");
    }

    #[test]
    fn max_method_statements_fires_on_fat_method() {
        let r = rule(
            r#"
            name = "controller-thin-methods"
            class_match.annotation = "RestController"
            kind = "max_method_statements"
            max = 3
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                public int fat() {
                    int a = 1;
                    int b = 2;
                    int c = 3;
                    int d = 4;
                    return a + b + c + d;
                }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert!(v[0].offending_import.contains("fat"));
    }

    #[test]
    fn class_matcher_supports_extends_for_android() {
        let r = rule(
            r#"
            name = "activity-no-retrofit"
            class_match.extends = "Activity"
            kind = "forbidden_field_type"
            type_regex = "^Retrofit$"
            "#,
        );
        let src = r#"
            package com.x.ui;
            public class MainActivity extends Activity {
                private Retrofit retrofit;
            }
        "#;
        let v = check_java_file(&[r], "MainActivity.java", "com.x.ui", src.as_bytes());
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn rule_does_not_fire_on_unrelated_class() {
        let r = rule(
            r#"
            name = "controller-no-repo-field"
            class_match.annotation = "RestController"
            kind = "forbidden_field_type"
            type_regex = ".*Repository$"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            public class UserService {
                private final UserRepository repo;
            }
        "#;
        let v = check_java_file(&[r], "UserService.java", "com.x.service", src.as_bytes());
        assert!(v.is_empty(), "service classes are not @RestController");
    }
}
