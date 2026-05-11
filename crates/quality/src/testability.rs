//! Per-method complexity & testability findings (T-CX).
//!
//! Two rule families share the same scan:
//!
//! - Classic complexity axes — cyclomatic, cognitive, max-nesting,
//!   long-method (LOC), wide-signature (parameter count). Reuses the
//!   tree-sitter walk in `complexity.rs` so we don't double-parse.
//! - Targeted "hard to test" rules — broad-catch, non-deterministic-call,
//!   inline-collaborator, static-singleton, reflection. Each is a small
//!   focused AST query; the rationale is that high cyclomatic complexity
//!   is *necessary but not sufficient* for untestability.
//!
//! Findings are written to `method_complexity_findings`; per-student
//! attribution (bad-line-weighted blame) lives in
//! `method_complexity_attribution`. The `analyze` crate then aggregates
//! attributions into a `COMPLEXITY_HOTSPOT` flag.
//!
//! ### Discovery scope
//!
//! Production code only. Tests, build outputs, and known generated-code
//! patterns (Room `*_Impl`, Dagger/Hilt `*_Factory`, MapStruct
//! `*MapperImpl`, `R.java`) are skipped at the file-discovery layer so a
//! noisy generator never reaches the AST scan.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use sprint_grader_core::config::DetectorThresholdsConfig;
use sprint_grader_core::finding::{LineSpan, RuleFinding, RuleKind, Severity as CoreSeverity};
use tracing::info;
use tree_sitter::{Node, Parser};
use walkdir::WalkDir;

use crate::complexity::{analyze_method, MethodMetrics};

static JAVA_LANG: once_cell::sync::Lazy<tree_sitter::Language> =
    once_cell::sync::Lazy::new(|| tree_sitter_java::LANGUAGE.into());

/// Java source roots scanned by the testability scan. Mirrors the
/// `static_analysis::discover_source_roots` convention but explicitly
/// excludes `src/test/java` / `app/src/test/java` — testability rules
/// are about production code only, not test fixtures.
pub(crate) const MAIN_SOURCE_ROOTS: &[&str] = &["src/main/java", "app/src/main/java"];

/// Path-component substrings whose presence anywhere in the file path
/// disqualifies a file from the scan. These cover Gradle / Maven build
/// outputs and the conventional generated-code locations Spring Boot,
/// AndroidX, Hilt, MapStruct and Room dump into.
const SKIP_PATH_SUBSTRINGS: &[&str] = &[
    "/test/",
    "/androidTest/",
    "/generated/",
    "/build/",
    "/.gradle/",
    "/target/",
    "/out/",
    "/bin/",
    "/hilt_aggregated_deps/",
    "/mapstruct/",
];

/// Generated-code filename suffixes. A file whose name (basename) ends
/// in any of these is skipped before parsing. Matches Room
/// (`AppDatabase_Impl.java`), Dagger / Hilt (`Foo_Factory.java`,
/// `Bar_MembersInjector.java`), MapStruct (`UserMapperImpl.java`), and
/// the well-known Android `R.java` resource shim.
const SKIP_BASENAME_SUFFIXES: &[&str] = &[
    "_Impl.java",
    "_Factory.java",
    "_MembersInjector.java",
    "MapperImpl.java",
];

/// Exact filenames to skip regardless of directory.
const SKIP_BASENAMES: &[&str] = &["R.java", "BuildConfig.java", "Manifest.java"];

/// Returns `true` if the given path is **inside** one of `MAIN_SOURCE_ROOTS`
/// and is **not** matched by any of the skip rules above. The check is
/// lexical only — no filesystem stat — so it can be run on candidate
/// paths produced by directory walks before deciding whether to parse.
///
/// `repo_relative_path` must be the path **relative to the repo root**.
/// For absolute or repo-prefixed paths the caller normalises first
/// (mirrors `static_analysis::attribution::normalize_file_path`).
pub(crate) fn is_scannable_main_source(repo_relative_path: &str) -> bool {
    if !repo_relative_path.ends_with(".java") {
        return false;
    }
    let normalised = repo_relative_path.replace('\\', "/");

    let in_main_root = MAIN_SOURCE_ROOTS
        .iter()
        .any(|root| normalised.starts_with(&format!("{root}/")) || normalised == *root);
    if !in_main_root {
        return false;
    }

    // Wrap in slashes so substring checks like "/test/" match a leading
    // segment too (e.g. someone places a stray `test/` directly under
    // `src/main/java`).
    let padded = format!("/{normalised}/");
    if SKIP_PATH_SUBSTRINGS.iter().any(|s| padded.contains(s)) {
        return false;
    }

    let basename = Path::new(&normalised)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if SKIP_BASENAMES.iter().any(|b| basename == *b) {
        return false;
    }
    if SKIP_BASENAME_SUFFIXES
        .iter()
        .any(|suf| basename.ends_with(suf))
    {
        return false;
    }

    true
}

/// One row destined for `method_complexity_findings`. The owning method
/// is identified by `(file_path, class_name, method_name, start_line,
/// end_line)`; the same method may produce multiple findings (one per
/// rule that fires), each carrying its own `(rule_key, severity,
/// measured_value, threshold)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub file_path: String,
    pub class_name: Option<String>,
    pub method_name: String,
    pub start_line: i64,
    pub end_line: i64,
    pub rule_key: String,
    pub severity: Severity,
    /// Measured value for the rule (e.g. CC = 22). `None` for boolean
    /// rules whose firing condition isn't a number (broad-catch).
    pub measured_value: Option<f64>,
    /// Threshold the measurement was compared against. `None` for boolean
    /// rules.
    pub threshold: Option<f64>,
    /// Short human-readable detail. Rendered verbatim in the report.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "CRITICAL",
            Severity::Warning => "WARNING",
            Severity::Info => "INFO",
        }
    }

    /// Map this crate's local severity onto the shared
    /// `sprint_grader_core::finding::Severity` (W2.T2).
    pub fn to_core(self) -> CoreSeverity {
        match self {
            Severity::Critical => CoreSeverity::Critical,
            Severity::Warning => CoreSeverity::Warning,
            Severity::Info => CoreSeverity::Info,
        }
    }
}

impl Finding {
    /// W2.T2: convert one complexity scanner finding into the shared
    /// `RuleFinding` shape consumed by the unified attribution +
    /// renderer pipeline.
    ///
    /// `extra` carries the "measured > threshold" overflow string so
    /// the unified renderer can append `(12 > 10)` without re-parsing
    /// the rule. `evidence` carries `Finding::detail` which is the
    /// human-readable prose the current renderer prints verbatim.
    /// `rule_id` matches the legacy `rule_key` field so blame look-ups
    /// keyed on `(file_path, rule_id)` stay valid.
    pub fn into_rule_finding(self, repo_full_name: &str) -> RuleFinding {
        let span = if self.end_line > self.start_line && self.start_line >= 1 {
            LineSpan::range(self.start_line as u32, self.end_line as u32)
        } else if self.start_line >= 1 {
            LineSpan::single(self.start_line as u32)
        } else {
            LineSpan::single(0)
        };
        let extra = match (self.measured_value, self.threshold) {
            (Some(m), Some(t)) => Some(format!("{m} > {t}")),
            _ => None,
        };
        RuleFinding {
            rule_id: self.rule_key,
            kind: RuleKind::Complexity,
            severity: self.severity.to_core(),
            repo_full_name: repo_full_name.to_string(),
            file_repo_relative: self.file_path,
            span,
            evidence: self.detail,
            extra,
        }
    }
}

const RULE_CYCLOMATIC: &str = "cyclomatic";
const RULE_COGNITIVE: &str = "cognitive";
const RULE_NESTING: &str = "nesting";
const RULE_LONG_METHOD: &str = "long-method";
const RULE_WIDE_SIGNATURE: &str = "wide-signature";

/// Apply the five fixed-cutoff complexity-axis rules to a single
/// method's metrics. The thresholds come from `course.toml
/// [detector_thresholds]` (see `DetectorThresholdsConfig::complexity_*`).
///
/// Each axis can fire at most once per method. Severity escalates from
/// WARNING to CRITICAL when the metric crosses the `_crit` band.
pub fn classic_complexity_findings(
    method: &MethodMetrics,
    th: &DetectorThresholdsConfig,
) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let mut push = |rule: &str, sev: Severity, value: f64, threshold: f64, detail: String| {
        out.push(Finding {
            file_path: method.file_path.clone(),
            class_name: Some(method.class_name.clone()),
            method_name: method.method_name.clone(),
            start_line: 0,
            end_line: 0,
            rule_key: rule.to_string(),
            severity: sev,
            measured_value: Some(value),
            threshold: Some(threshold),
            detail,
        });
    };

    let cc = method.cyclomatic_complexity as f64;
    if cc > th.complexity_cc_crit {
        push(
            RULE_CYCLOMATIC,
            Severity::Critical,
            cc,
            th.complexity_cc_crit,
            format!(
                "cyclomatic complexity {} exceeds critical bound {}",
                cc as i64, th.complexity_cc_crit as i64
            ),
        );
    } else if cc > th.complexity_cc_warn {
        push(
            RULE_CYCLOMATIC,
            Severity::Warning,
            cc,
            th.complexity_cc_warn,
            format!(
                "cyclomatic complexity {} exceeds warning bound {}",
                cc as i64, th.complexity_cc_warn as i64
            ),
        );
    }

    let cog = method.cognitive_complexity as f64;
    if cog > th.complexity_cognitive_crit {
        push(
            RULE_COGNITIVE,
            Severity::Critical,
            cog,
            th.complexity_cognitive_crit,
            format!(
                "cognitive complexity {} exceeds critical bound {}",
                cog as i64, th.complexity_cognitive_crit as i64
            ),
        );
    } else if cog > th.complexity_cognitive_warn {
        push(
            RULE_COGNITIVE,
            Severity::Warning,
            cog,
            th.complexity_cognitive_warn,
            format!(
                "cognitive complexity {} exceeds warning bound {}",
                cog as i64, th.complexity_cognitive_warn as i64
            ),
        );
    }

    let nesting = method.max_nesting_depth as f64;
    if nesting > th.complexity_nesting_crit {
        push(
            RULE_NESTING,
            Severity::Critical,
            nesting,
            th.complexity_nesting_crit,
            format!(
                "max nesting depth {} exceeds critical bound {}",
                nesting as i64, th.complexity_nesting_crit as i64
            ),
        );
    } else if nesting > th.complexity_nesting_warn {
        push(
            RULE_NESTING,
            Severity::Warning,
            nesting,
            th.complexity_nesting_warn,
            format!(
                "max nesting depth {} exceeds warning bound {}",
                nesting as i64, th.complexity_nesting_warn as i64
            ),
        );
    }

    let loc = method.loc as f64;
    if loc > th.complexity_loc_crit {
        push(
            RULE_LONG_METHOD,
            Severity::Critical,
            loc,
            th.complexity_loc_crit,
            format!(
                "method spans {} lines (> critical bound {})",
                loc as i64, th.complexity_loc_crit as i64
            ),
        );
    } else if loc > th.complexity_loc_warn {
        push(
            RULE_LONG_METHOD,
            Severity::Warning,
            loc,
            th.complexity_loc_warn,
            format!(
                "method spans {} lines (> warning bound {})",
                loc as i64, th.complexity_loc_warn as i64
            ),
        );
    }

    let params = method.parameter_count as f64;
    if params > th.complexity_params_crit {
        push(
            RULE_WIDE_SIGNATURE,
            Severity::Critical,
            params,
            th.complexity_params_crit,
            format!(
                "method takes {} parameters (> critical bound {})",
                params as i64, th.complexity_params_crit as i64
            ),
        );
    } else if params > th.complexity_params_warn {
        push(
            RULE_WIDE_SIGNATURE,
            Severity::Warning,
            params,
            th.complexity_params_warn,
            format!(
                "method takes {} parameters (> warning bound {})",
                params as i64, th.complexity_params_warn as i64
            ),
        );
    }

    out
}

/// Parse the file once and return a `Vec` of `(method_node_lines,
/// classic_findings)` so that the per-method line range can be paired
/// with the rule rows before persistence. Method line numbers are
/// 1-based, both ends inclusive.
pub fn classic_findings_for_file(
    file_path: &str,
    source: &[u8],
    th: &DetectorThresholdsConfig,
) -> Vec<Finding> {
    let mut parser = Parser::new();
    if parser.set_language(&JAVA_LANG).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut out: Vec<Finding> = Vec::new();
    fn walk(
        node: Node,
        source: &[u8],
        file_path: &str,
        th: &DetectorThresholdsConfig,
        out: &mut Vec<Finding>,
    ) {
        let k = node.kind();
        if k == "method_declaration" || k == "constructor_declaration" {
            let metrics = analyze_method(node, source, file_path);
            let mut findings = classic_complexity_findings(&metrics, th);
            let start = (node.start_position().row as i64) + 1;
            let end = (node.end_position().row as i64) + 1;
            for f in &mut findings {
                f.start_line = start;
                f.end_line = end;
            }
            out.extend(findings);
        }
        let mut cursor = node.walk();
        for c in node.children(&mut cursor) {
            walk(c, source, file_path, th, out);
        }
    }
    walk(tree.root_node(), source, file_path, th, &mut out);
    out
}

// ----------------------------------------------------------------------
// Targeted testability rules. Each rule is a focused AST query whose
// rationale is "this code is hard to test in isolation" rather than
// "this code is complex". The rules emit `Finding` rows alongside the
// classic-axis ones so the report can group them together per method.
// ----------------------------------------------------------------------

const RULE_BROAD_CATCH: &str = "broad-catch";
const RULE_NON_DETERMINISTIC: &str = "non-deterministic-call";
const RULE_INLINE_COLLABORATOR: &str = "inline-collaborator";
const RULE_STATIC_SINGLETON: &str = "static-singleton";
const RULE_REFLECTION: &str = "reflection";

/// Catch-clause type identifiers we treat as "broad" — they swallow more
/// than the specific failure mode the test needs to assert against, so a
/// reviewer can't tell what was intended to fail.
const BROAD_CATCH_TYPES: &[&str] = &["Exception", "RuntimeException", "Throwable"];

/// Method qualifiers we treat as non-deterministic. The match is on the
/// last identifier of the qualified call (`System.currentTimeMillis` →
/// `currentTimeMillis`); we then check the preceding qualifier when one
/// is required to disambiguate (`Math.random` vs an arbitrary
/// `random()` user method).
struct NonDetCall {
    /// The required qualifier on the call (e.g. `"System"`). `None` means
    /// any receiver is fine — used for the `LocalDateTime.now()` family
    /// where the qualifier varies.
    qualifier: Option<&'static str>,
    /// The method name being looked for.
    method: &'static str,
}

const NON_DET_CALLS: &[NonDetCall] = &[
    NonDetCall {
        qualifier: Some("System"),
        method: "currentTimeMillis",
    },
    NonDetCall {
        qualifier: Some("System"),
        method: "nanoTime",
    },
    NonDetCall {
        qualifier: Some("Math"),
        method: "random",
    },
    NonDetCall {
        qualifier: None,
        method: "now", // LocalDateTime.now / Instant.now / ZonedDateTime.now / Clock.now
    },
];

/// Time-style class names whose `new X()` we treat as non-deterministic.
/// `new Date()` reads the system clock; `new Random()` without a seed is
/// equally untestable.
const NON_DET_NEW_TYPES: &[&str] = &["Date", "Random"];

/// Collaborator-style class-name suffixes. `new UserService(...)` inside
/// a method body wires a real dependency at call time and bypasses any
/// constructor injection the team set up — making it impossible to
/// substitute a fake in tests.
const COLLABORATOR_SUFFIXES: &[&str] = &[
    "Service",
    "Client",
    "Repository",
    "Dao",
    "Manager",
    "Helper",
    "Gateway",
];

/// Class names we explicitly do NOT flag as collaborators even though
/// they end in a collaborator suffix. JDK / common library types live
/// here.
const COLLABORATOR_ALLOWLIST: &[&str] = &[
    "ResponseEntityExceptionHandler",
    "ServletContextEvent",
    "ApplicationContext",
];

fn child_with_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

fn children_of<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text<'a>(node: Node<'a>, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

/// Walk every descendant node of `root` and call `visit` on each one.
fn for_each_descendant<F: FnMut(Node)>(root: Node, mut visit: F) {
    fn rec<F: FnMut(Node)>(n: Node, visit: &mut F) {
        visit(n);
        let mut cur = n.walk();
        for c in n.children(&mut cur) {
            rec(c, visit);
        }
    }
    rec(root, &mut visit);
}

fn line_of(node: Node) -> i64 {
    (node.start_position().row as i64) + 1
}

/// True if any descendant of `body` is a `throw_statement`. Used by
/// broad-catch to distinguish "swallows the exception" (bad) from
/// "wraps + rethrows" (acceptable).
fn body_contains_throw(body: Node) -> bool {
    let mut found = false;
    for_each_descendant(body, |n| {
        if n.kind() == "throw_statement" {
            found = true;
        }
    });
    found
}

/// True if the `catch_formal_parameter` matches one of the broad types,
/// including multi-catch (`Foo | Exception`) where any union member is
/// broad.
fn catch_param_is_broad(param: Node, source: &[u8]) -> bool {
    // tree-sitter-java emits `catch_formal_parameter` containing one or
    // more `catch_type` children, each of which contains `type_identifier`
    // / `scoped_type_identifier` nodes. Walk descendants and pick up any
    // type identifier that matches the broad list.
    let mut broad = false;
    for_each_descendant(param, |n| {
        let k = n.kind();
        if k == "type_identifier" || k == "scoped_type_identifier" {
            let txt = node_text(n, source);
            let last = txt.rsplit('.').next().unwrap_or(&txt);
            if BROAD_CATCH_TYPES.contains(&last) {
                broad = true;
            }
        }
    });
    broad
}

fn detect_broad_catch_in_method(body: Node, source: &[u8], out: &mut Vec<(i64, i64, String)>) {
    for_each_descendant(body, |n| {
        if n.kind() != "catch_clause" {
            return;
        }
        // catch_clause = `catch ( catch_formal_parameter ) block`
        let param = match child_with_kind(n, "catch_formal_parameter") {
            Some(p) => p,
            None => return,
        };
        if !catch_param_is_broad(param, source) {
            return;
        }
        let catch_body = match child_with_kind(n, "block") {
            Some(b) => b,
            None => return,
        };
        if body_contains_throw(catch_body) {
            return;
        }
        let start = line_of(n);
        let end = (catch_body.end_position().row as i64) + 1;
        let txt = node_text(param, source);
        out.push((start, end, txt));
    });
}

/// Resolve a `method_invocation` node to (qualifier_text, method_name).
/// `qualifier_text` is the receiver expression's source (`"System"`,
/// `"this"`, `"foo.bar"`, etc.) or `None` when the call has no explicit
/// receiver.
fn split_method_invocation<'a>(inv: Node<'a>, source: &[u8]) -> Option<(Option<String>, String)> {
    // tree-sitter-java field names: `object` (the receiver) and `name`
    // (the method identifier).
    let name_node = inv.child_by_field_name("name")?;
    let method = node_text(name_node, source);
    let qualifier = inv
        .child_by_field_name("object")
        .map(|q| node_text(q, source));
    Some((qualifier, method))
}

fn detect_non_deterministic_in_method(
    body: Node,
    source: &[u8],
    out: &mut Vec<(i64, i64, String)>,
) {
    for_each_descendant(body, |n| match n.kind() {
        "method_invocation" => {
            if let Some((qualifier, method)) = split_method_invocation(n, source) {
                for spec in NON_DET_CALLS {
                    if spec.method != method {
                        continue;
                    }
                    let qual_match = match (spec.qualifier, qualifier.as_deref()) {
                        (Some(req), Some(got)) => {
                            // accept `System.currentTimeMillis()` and
                            // `java.lang.System.currentTimeMillis()`.
                            got.rsplit('.').next() == Some(req)
                        }
                        (None, Some(got)) => {
                            // Restrict bare `now()` to known time classes
                            // so it doesn't flag arbitrary user methods.
                            matches!(
                                got.rsplit('.').next().unwrap_or(""),
                                "LocalDate"
                                    | "LocalDateTime"
                                    | "LocalTime"
                                    | "ZonedDateTime"
                                    | "OffsetDateTime"
                                    | "Instant"
                                    | "Clock"
                            )
                        }
                        _ => false,
                    };
                    if qual_match {
                        let start = line_of(n);
                        let end = (n.end_position().row as i64) + 1;
                        let detail = match spec.qualifier {
                            Some(q) => format!("calls {}.{}()", q, method),
                            None => format!("calls {}()", method),
                        };
                        out.push((start, end, detail));
                    }
                }
            }
        }
        "object_creation_expression" => {
            // `new X(...)` — pick the type identifier child.
            if let Some(type_node) = n.child_by_field_name("type") {
                let txt = node_text(type_node, source);
                let last = txt.rsplit('.').next().unwrap_or(&txt);
                if NON_DET_NEW_TYPES.contains(&last) {
                    let start = line_of(n);
                    let end = (n.end_position().row as i64) + 1;
                    out.push((start, end, format!("instantiates {last}")));
                }
            }
        }
        _ => {}
    });
}

/// Identify whether the enclosing method's name and signature look like a
/// factory / setter / builder where instantiating a collaborator is the
/// expected behaviour. We don't have constructor-injection awareness, so
/// the heuristic stays loose — factories are explicit setters/builders
/// or methods returning the type itself.
fn enclosing_method_is_factory_like(method_name: &str) -> bool {
    let m = method_name;
    m.starts_with("set")
        || m.starts_with("with")
        || m.starts_with("build")
        || m.starts_with("create")
        || m.starts_with("provide")
        || m.starts_with("make")
        || m.starts_with("of")
        || m == "newInstance"
}

fn detect_inline_collaborator_in_method(
    body: Node,
    source: &[u8],
    method_name: &str,
    out: &mut Vec<(i64, i64, String)>,
) {
    if enclosing_method_is_factory_like(method_name) {
        return;
    }
    for_each_descendant(body, |n| {
        if n.kind() != "object_creation_expression" {
            return;
        }
        let type_node = match n.child_by_field_name("type") {
            Some(t) => t,
            None => return,
        };
        let txt = node_text(type_node, source);
        let last = txt.rsplit('.').next().unwrap_or(&txt).to_string();
        if COLLABORATOR_ALLOWLIST.contains(&last.as_str()) {
            return;
        }
        if !COLLABORATOR_SUFFIXES.iter().any(|s| last.ends_with(s)) {
            return;
        }
        let start = line_of(n);
        let end = (n.end_position().row as i64) + 1;
        out.push((start, end, format!("instantiates collaborator {last}")));
    });
}

fn detect_static_singleton_in_method(body: Node, source: &[u8], out: &mut Vec<(i64, i64, String)>) {
    for_each_descendant(body, |n| {
        if n.kind() != "method_invocation" {
            return;
        }
        let (qualifier, method) = match split_method_invocation(n, source) {
            Some(x) => x,
            None => return,
        };
        let q = match qualifier {
            Some(q) => q,
            None => return,
        };
        // Two patterns count:
        //  1. `X.getInstance()` where X starts with an uppercase letter.
        //  2. `X.INSTANCE.something()` — the `.INSTANCE.` chain.
        let last_q = q.rsplit('.').next().unwrap_or(&q);
        let starts_upper = last_q
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false);

        if starts_upper && method == "getInstance" {
            let start = line_of(n);
            let end = (n.end_position().row as i64) + 1;
            out.push((start, end, format!("calls {last_q}.getInstance()")));
            return;
        }
        if q.contains(".INSTANCE") {
            let start = line_of(n);
            let end = (n.end_position().row as i64) + 1;
            out.push((start, end, format!("calls singleton {q}.{method}()")));
        }
    });
}

fn detect_reflection_in_method(
    body: Node,
    source: &[u8],
    has_reflect_import: bool,
    out: &mut Vec<(i64, i64, String)>,
) {
    for_each_descendant(body, |n| {
        if n.kind() != "method_invocation" {
            return;
        }
        let (qualifier, method) = match split_method_invocation(n, source) {
            Some(x) => x,
            None => return,
        };
        let q = qualifier.unwrap_or_default();
        let last_q = q.rsplit('.').next().unwrap_or(&q).to_string();

        // `Class.forName(...)` is unambiguously reflection regardless of
        // imports (java.lang.Class is auto-imported, no other forName
        // exists in common Java code).
        if last_q == "Class" && method == "forName" {
            let start = line_of(n);
            let end = (n.end_position().row as i64) + 1;
            out.push((start, end, "uses Class.forName()".to_string()));
            return;
        }
        // `.invoke(...)` and `.newInstance(...)` are reflection when the
        // file imports `java.lang.reflect.*` — at that point the
        // receiver is overwhelmingly likely to be a Method or
        // Constructor instance. Without type-resolution we can't be
        // certain, but the import is a strong narrowing signal: a
        // course project that doesn't import `java.lang.reflect` and
        // has a method called `invoke` is almost certainly user code.
        if has_reflect_import && (method == "invoke" || method == "newInstance") {
            let start = line_of(n);
            let end = (n.end_position().row as i64) + 1;
            out.push((start, end, format!("uses reflection .{method}()")));
        }
    });
}

/// True if any `import_declaration` in the file resolves a member of
/// `java.lang.reflect`. Used as a precondition for the targeted
/// `Method.invoke` / `Field.set` heuristics so user code that happens to
/// have a method named `invoke` doesn't false-positive.
fn file_imports_reflect(root: Node, source: &[u8]) -> bool {
    let mut found = false;
    for_each_descendant(root, |n| {
        if n.kind() == "import_declaration" {
            let txt = node_text(n, source);
            if txt.contains("java.lang.reflect") {
                found = true;
            }
        }
    });
    found
}

/// Apply every targeted testability rule to a single parsed file.
/// Returns one finding per (method, fired rule). Method-level metadata
/// (file_path, class_name, method_name, line range) is attached so the
/// caller can write rows to `method_complexity_findings` directly.
pub fn testability_findings_for_file(file_path: &str, source: &[u8]) -> Vec<Finding> {
    let mut parser = Parser::new();
    if parser.set_language(&JAVA_LANG).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let root = tree.root_node();
    let has_reflect_import = file_imports_reflect(root, source);
    let mut out: Vec<Finding> = Vec::new();

    fn collect_methods<'a>(node: Node<'a>, acc: &mut Vec<Node<'a>>) {
        let k = node.kind();
        if k == "method_declaration" || k == "constructor_declaration" {
            acc.push(node);
        }
        for c in children_of(node) {
            collect_methods(c, acc);
        }
    }
    let mut methods: Vec<Node> = Vec::new();
    collect_methods(root, &mut methods);

    for method_node in methods {
        let metrics = analyze_method(method_node, source, file_path);
        let body = match method_node.child_by_field_name("body") {
            Some(b) => b,
            None => continue,
        };
        let m_start = (method_node.start_position().row as i64) + 1;
        let m_end = (method_node.end_position().row as i64) + 1;

        let mut push_findings = |rule: &str, sev: Severity, hits: Vec<(i64, i64, String)>| {
            if hits.is_empty() {
                return;
            }
            // One finding per fired rule per method; the detail records
            // each hit line so the report can point to the offending
            // construct.
            let detail = hits
                .iter()
                .map(|(l, _, d)| format!("L{l}: {d}"))
                .collect::<Vec<_>>()
                .join("; ");
            out.push(Finding {
                file_path: file_path.to_string(),
                class_name: Some(metrics.class_name.clone()),
                method_name: metrics.method_name.clone(),
                start_line: m_start,
                end_line: m_end,
                rule_key: rule.to_string(),
                severity: sev,
                measured_value: Some(hits.len() as f64),
                threshold: None,
                detail,
            });
        };

        let mut hits = Vec::new();
        detect_broad_catch_in_method(body, source, &mut hits);
        push_findings(RULE_BROAD_CATCH, Severity::Warning, hits);

        let mut hits = Vec::new();
        detect_non_deterministic_in_method(body, source, &mut hits);
        push_findings(RULE_NON_DETERMINISTIC, Severity::Warning, hits);

        let mut hits = Vec::new();
        detect_inline_collaborator_in_method(body, source, &metrics.method_name, &mut hits);
        push_findings(RULE_INLINE_COLLABORATOR, Severity::Warning, hits);

        let mut hits = Vec::new();
        detect_static_singleton_in_method(body, source, &mut hits);
        push_findings(RULE_STATIC_SINGLETON, Severity::Info, hits);

        let mut hits = Vec::new();
        detect_reflection_in_method(body, source, has_reflect_import, &mut hits);
        push_findings(RULE_REFLECTION, Severity::Warning, hits);
    }

    out
}

// ----------------------------------------------------------------------
// Bad-line-weighted blame attribution. For each `method_complexity_findings`
// row in (repo, sprint), we blame the method's [start_line..=end_line]
// range, classify each line's "badness" (3x for the offending construct,
// 2x for control-flow, 1x otherwise), then write per-student
// `method_complexity_attribution` rows whose `weight` sums to 1.
//
// Mirrors `static_analysis::attribution::attribute_findings_for_repo`
// in shape — same blame infrastructure, same email→student map. The
// novel piece here is the weighting policy: a junior who only touched
// formatting on a long bad method shouldn't carry the same attribution
// as the author who wrote the catch block.
// ----------------------------------------------------------------------

use std::collections::HashMap;

use rusqlite::{params, Connection};
use sprint_grader_core::time::{containing_sprint_id, load_sprint_windows, track_min_time};
use sprint_grader_survival::blame::{blame_file, build_email_to_student_map, EmailStudentMap};
use tracing::warn;

/// Per-line weighting policy applied during attribution. Each variant
/// corresponds to a different role the line plays inside the method
/// body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineBadness {
    /// Plain method-body line. Weight 1.
    Plain,
    /// Line that introduces control flow (`if`, `while`, `for`,
    /// `switch`, `case`, `do`, `catch`, or contains `&&`/`||`/ternary
    /// `?:`). Weight 2.
    ControlFlow,
    /// Line that contains the actual offending construct (the catch
    /// header for `broad-catch`, the line containing the offending
    /// call for `non-deterministic-call` / `inline-collaborator` /
    /// `static-singleton` / `reflection`). Weight 3.
    Offending,
}

impl LineBadness {
    fn weight(self) -> f64 {
        match self {
            LineBadness::Plain => 1.0,
            LineBadness::ControlFlow => 2.0,
            LineBadness::Offending => 3.0,
        }
    }
}

fn is_blank_or_brace(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t == "{" || t == "}" || t == "});" || t == "};"
}

/// Quick syntactic classification of a single source line. The
/// classifier is deliberately string-based: parsing every method again
/// for the badness map would double the AST cost. The patterns we
/// recognise are kept narrow (whole-word matches, comment-stripping)
/// so the false-positive rate stays low. Strings inside a literal
/// would trip a naive `contains("if")` — we strip line comments and
/// the most common string-literal forms before checking.
fn classify_control_flow(line_no_comment_no_string: &str) -> bool {
    let s = line_no_comment_no_string;
    // Whole-word keywords. Padding with spaces lets us check word
    // boundaries cheaply without pulling in a regex.
    let padded = format!(" {s} ");
    const KEYWORDS: &[&str] = &[
        " if ", " if(", " for ", " for(", " while ", " while(", " do ", " do{", " switch ",
        " switch(", " case ", " catch ", " catch(", " ? ",
    ];
    if KEYWORDS.iter().any(|k| padded.contains(k)) {
        return true;
    }
    s.contains("&&") || s.contains("||")
}

/// Strip `//` line comments and the contents of double-quoted strings
/// so the keyword scan in `classify_control_flow` doesn't false-trip
/// on `String s = "if you ..."`. Cheap; not a real Java tokenizer.
fn strip_line_for_keyword_scan(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_str = false;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if !in_str && c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            break;
        }
        if !in_str && c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if in_str {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// Build a per-line badness map for the method `[start..=end]`. The
/// `offending_lines` set lists the lines flagged by the rule itself
/// (the catch header line, the line containing the offending call,
/// etc.); these always score `Offending` regardless of what the line
/// looks like syntactically.
fn line_badness_map(
    file_lines: &[String],
    start: u32,
    end: u32,
    offending_lines: &[u32],
) -> HashMap<u32, LineBadness> {
    let mut map: HashMap<u32, LineBadness> = HashMap::new();
    let off_set: std::collections::HashSet<u32> = offending_lines.iter().copied().collect();
    for ln in start..=end {
        let idx = (ln as usize).saturating_sub(1);
        let raw = file_lines.get(idx).cloned().unwrap_or_default();
        if is_blank_or_brace(&raw) {
            continue;
        }
        if off_set.contains(&ln) {
            map.insert(ln, LineBadness::Offending);
            continue;
        }
        let stripped = strip_line_for_keyword_scan(&raw);
        if classify_control_flow(&stripped) {
            map.insert(ln, LineBadness::ControlFlow);
            continue;
        }
        map.insert(ln, LineBadness::Plain);
    }
    map
}

/// Read a file and split it into 1-indexed lines (so `lines[i]` gives
/// the source for line number `i+1`).
fn read_file_lines(repo_path: &Path, repo_relative: &str) -> Vec<String> {
    let path = repo_path.join(repo_relative);
    match std::fs::read_to_string(&path) {
        Ok(s) => s.lines().map(|l| l.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Resolve a blame email to a student id, mirroring the
/// `static_analysis::attribution::resolve_student` policy. Lower-cases
/// before lookup, falls back to the local-part and the GitHub
/// noreply form so identity-resolution stays consistent across the
/// pipeline.
fn resolve_student(map: &EmailStudentMap, email: &str) -> Option<String> {
    let key = email.to_lowercase();
    if let Some((sid, _)) = map.get(&key) {
        return Some(sid.clone());
    }
    if let Some(local) = key.split('@').next() {
        if let Some((sid, _)) = map.get(local) {
            return Some(sid.clone());
        }
        if let Some((sid, _)) = map.get(&format!("{local}@users.noreply.github.com")) {
            return Some(sid.clone());
        }
    }
    None
}

/// Parse `detail` strings of the form `"L<lineno>: …; L<lineno>: …"` —
/// the format produced by `testability_findings_for_file` — and return
/// the offending line numbers. Used to locate the
/// `LineBadness::Offending` lines when re-attributing from cached
/// findings rows.
fn parse_offending_lines(detail: &str) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for piece in detail.split(';') {
        let piece = piece.trim();
        let rest = match piece.strip_prefix('L') {
            Some(r) => r,
            None => continue,
        };
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<u32>() {
            out.push(n);
        }
    }
    out
}

/// Run blame attribution for every `method_complexity_findings` row in
/// `repo_full_name` (T-P3.4: sprint-free, artifact-shape).
/// Pre-existing `method_complexity_attribution` rows for the repo's
/// findings are deleted before re-inserting, mirroring the
/// architecture-attribution idempotency idiom. Also fills
/// `method_complexity_findings.introduced_sprint_id` for each finding
/// from per-line blame author-times (earliest containing sprint window).
/// Returns the number of attribution rows written.
pub fn attribute_findings_for_repo(
    conn: &Connection,
    repo_path: &Path,
    repo_full_name: &str,
) -> rusqlite::Result<usize> {
    let email_map = build_email_to_student_map(conn)?;
    let sprint_windows = load_sprint_windows(conn)?;

    // Pull every finding row that owns a non-zero line range.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(i64, String, i64, i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, file_path, start_line, end_line, rule_key, COALESCE(detail, '')
             FROM method_complexity_findings
             WHERE repo_full_name = ?
               AND start_line > 0 AND end_line >= start_line",
        )?;
        let it = stmt.query_map(params![repo_full_name], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        it.collect::<rusqlite::Result<_>>()?
    };

    // Group rows by file so we blame each file once.
    let mut by_file: HashMap<String, Vec<(i64, u32, u32, String, String)>> = HashMap::new();
    for (id, file_path, start, end, rule, detail) in rows {
        let s = start.max(1) as u32;
        let e = end.max(start) as u32;
        by_file
            .entry(file_path)
            .or_default()
            .push((id, s, e, rule, detail));
    }

    conn.execute(
        "DELETE FROM method_complexity_attribution
         WHERE finding_id IN (
             SELECT id FROM method_complexity_findings
             WHERE repo_full_name = ?
         )",
        params![repo_full_name],
    )?;

    let mut written = 0usize;
    for (file_path, findings) in by_file {
        let blame = blame_file(repo_path, &file_path);
        if blame.is_empty() {
            warn!(
                repo = repo_full_name,
                file = %file_path,
                "blame returned no lines; skipping attribution for this file"
            );
            continue;
        }
        let file_lines = read_file_lines(repo_path, &file_path);
        for (id, start, end, _rule, detail) in findings {
            // `Offending` lines come from the rule's `detail` field for
            // testability rules. Classic-axis rules don't pin a
            // specific line — for those, every line just gets its
            // syntactic classification (control-flow → 2x, else 1x).
            let offending = parse_offending_lines(&detail);
            let badness = line_badness_map(&file_lines, start, end, &offending);

            let mut per_student_lines: HashMap<String, u32> = HashMap::new();
            let mut per_student_weight: HashMap<String, f64> = HashMap::new();
            let mut total_weight: f64 = 0.0;
            let mut total_lines: u32 = 0;
            let mut min_author_time: Option<i64> = None;
            for ln in start..=end {
                let bl = match blame.get(&ln) {
                    Some(b) => b,
                    None => continue,
                };
                let bd = match badness.get(&ln) {
                    Some(b) => *b,
                    None => continue, // blank or brace-only line
                };
                let w = bd.weight();
                total_weight += w;
                total_lines += 1;
                if let Some(sid) = resolve_student(&email_map, &bl.author_email) {
                    *per_student_lines.entry(sid.clone()).or_default() += 1;
                    *per_student_weight.entry(sid).or_default() += w;
                }
                track_min_time(&mut min_author_time, bl.author_time);
            }

            // T-P3.4: write the earliest containing sprint as
            // `introduced_sprint_id`. Always update (even to NULL) so a
            // re-run cleanly overwrites prior values.
            let introduced = min_author_time.and_then(|t| containing_sprint_id(&sprint_windows, t));
            conn.execute(
                "UPDATE method_complexity_findings
                 SET introduced_sprint_id = ?
                 WHERE id = ?",
                params![introduced, id],
            )?;

            if total_weight <= 0.0 || per_student_weight.is_empty() {
                continue;
            }
            for (sid, weighted) in per_student_weight {
                let weight = weighted / total_weight;
                let lines_attributed = per_student_lines.get(&sid).copied().unwrap_or(0);
                conn.execute(
                    "INSERT OR REPLACE INTO method_complexity_attribution
                        (finding_id, student_id, lines_attributed,
                         weighted_lines, weight)
                     VALUES (?, ?, ?, ?, ?)",
                    params![id, sid, lines_attributed as i64, weighted, weight],
                )?;
                written += 1;
            }
            let _ = total_lines; // tracked for future diagnostics
        }
    }
    Ok(written)
}

// ----------------------------------------------------------------------
// Repo / project scan entry points (T-CX, step 8). Walks every Java file
// under a cloned repo, applies the file-discovery skip filter, parses
// each file ONCE, and writes both `method_metrics` (the long-orphaned
// metrics cache the rest of the quality stage already reads) and
// `method_complexity_findings`. Then runs bad-line-weighted blame
// attribution. Persists a `method_complexity_runs` row keyed on
// `(repo_full_name, sprint_id)` so re-runs against the same head SHA
// can short-circuit.
// ----------------------------------------------------------------------

/// Outcome rows persisted to `method_complexity_runs.status`.
const STATUS_OK: &str = "OK";
const STATUS_SKIPPED_NO_SOURCES: &str = "SKIPPED_NO_SOURCES";
const STATUS_SKIPPED_HEAD_UNCHANGED: &str = "SKIPPED_HEAD_UNCHANGED";
const STATUS_CRASHED: &str = "CRASHED";

/// Capture the current `HEAD` SHA of `repo_path`. Returns `None` when
/// `git` isn't available or the directory isn't a repo (e.g. the test
/// harness writes plain files into a tmpdir without `git init`).
fn git_head_sha(repo_path: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Walk a repo and return `.java` paths (relative to `repo_path`) that
/// pass `is_scannable_main_source`. The walker reuses
/// `walkdir::WalkDir` like the architecture scanner does, with the same
/// `target/`, `build/`, `.gradle/` etc. directory pruning baked into
/// the skip predicate via the path-substring rules.
fn discover_main_java_files(repo_path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("java") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(repo_path) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !is_scannable_main_source(&rel_str) {
            continue;
        }
        out.push(rel_str);
    }
    out.sort();
    out
}

/// Persist one method's metrics into `method_metrics` (the cache the
/// rest of the quality stage already reads). Idempotent via
/// `INSERT OR REPLACE` on the natural PK `(file_path, class_name,
/// method_name, sprint_id)`. The `author_id` column is intentionally
/// NULL here — the testability stage doesn't resolve method-level
/// authorship at the metric level (attribution happens per-finding).
fn persist_method_metrics(
    conn: &rusqlite::Connection,
    sprint_id: i64,
    metric: &MethodMetrics,
) -> rusqlite::Result<()> {
    use rusqlite::params;
    conn.execute(
        "INSERT OR REPLACE INTO method_metrics
            (file_path, class_name, method_name, sprint_id,
             author_id, loc, cyclomatic_complexity, cognitive_complexity,
             parameter_count, max_nesting_depth, return_count,
             halstead_volume, halstead_difficulty, halstead_effort,
             halstead_bugs, maintainability_index, start_line, end_line)
         VALUES (?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?,
                 NULL, NULL, NULL, NULL, NULL, ?, ?)",
        params![
            metric.file_path,
            metric.class_name,
            metric.method_name,
            sprint_id,
            metric.loc,
            metric.cyclomatic_complexity,
            metric.cognitive_complexity,
            metric.parameter_count,
            metric.max_nesting_depth,
            metric.return_count,
            metric.start_line,
            metric.end_line,
        ],
    )?;
    Ok(())
}

/// Persist one rule firing into `method_complexity_findings`. Returns
/// the new row's id so the caller can run attribution off it later.
fn persist_finding(
    conn: &rusqlite::Connection,
    project_id: i64,
    repo_full_name: &str,
    f: &Finding,
) -> rusqlite::Result<i64> {
    use rusqlite::params;
    conn.execute(
        "INSERT INTO method_complexity_findings
            (project_id, repo_full_name, file_path, class_name,
             method_name, start_line, end_line, rule_key, severity,
             measured_value, threshold, detail)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            project_id,
            repo_full_name,
            f.file_path,
            f.class_name,
            f.method_name,
            f.start_line,
            f.end_line,
            f.rule_key,
            f.severity.as_str(),
            f.measured_value,
            f.threshold,
            f.detail,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Persist the `method_complexity_runs` row for `repo_full_name`.
/// Idempotent: `INSERT OR REPLACE` on the natural PK so re-runs
/// overwrite the prior status atomically (T-P3.4: per-repo, sprint-free).
fn record_run(
    conn: &rusqlite::Connection,
    repo_full_name: &str,
    status: &str,
    findings_count: usize,
    duration_ms: i64,
    head_sha: Option<&str>,
    diagnostics: Option<&str>,
) -> rusqlite::Result<()> {
    use rusqlite::params;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO method_complexity_runs
            (repo_full_name, status, findings_count,
             duration_ms, head_sha, diagnostics, ran_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            repo_full_name,
            status,
            findings_count as i64,
            duration_ms,
            head_sha,
            diagnostics,
            now,
        ],
    )?;
    Ok(())
}

/// Returns the cached head SHA (if any) for `repo_full_name`. Callers
/// compare it against the current `git_head_sha` to decide whether to
/// short-circuit the AST scan.
fn cached_head_sha(conn: &rusqlite::Connection, repo_full_name: &str) -> Option<String> {
    conn.query_row(
        "SELECT head_sha FROM method_complexity_runs
         WHERE repo_full_name = ? AND status = ?",
        rusqlite::params![repo_full_name, STATUS_OK],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// Scan one cloned repo, persist findings + metrics + attribution
/// (T-P3.4: artifact-shape, sprint-free). Idempotent at
/// `(repo_full_name)` granularity: pre-existing finding rows for the
/// repo are dropped before re-insert (cascade clears attribution via
/// the FK).
///
/// Short-circuit: when `method_complexity_runs.head_sha` for this repo
/// already matches the current `git rev-parse HEAD`, the AST walk is
/// skipped and the cached findings stay in place. This keeps `report`
/// regeneration cheap after a config tweak.
///
/// `project_id` is recorded on every finding row so artifact-flag
/// queries can scope to a project without joining `pull_requests`.
/// Note that the `method_metrics` cache is still keyed per-sprint —
/// that table is not part of the artifact migration.
pub fn scan_repo_to_db(
    conn: &rusqlite::Connection,
    repo_path: &Path,
    repo_full_name: &str,
    sprint_id_for_metrics_cache: i64,
    project_id: i64,
    thresholds: &DetectorThresholdsConfig,
) -> rusqlite::Result<usize> {
    use rusqlite::params;
    let started = Instant::now();
    let head = git_head_sha(repo_path);

    // Short-circuit when the head SHA hasn't moved since the last
    // successful run. The cached findings + attribution remain valid.
    if let (Some(current), Some(cached)) = (head.as_deref(), cached_head_sha(conn, repo_full_name))
    {
        if current == cached {
            // Bookkeeping: refresh ran_at so operators can tell the
            // scan was *considered* this run, just not re-executed.
            let kept: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM method_complexity_findings
                     WHERE repo_full_name = ?",
                    params![repo_full_name],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            record_run(
                conn,
                repo_full_name,
                STATUS_SKIPPED_HEAD_UNCHANGED,
                kept as usize,
                started.elapsed().as_millis() as i64,
                Some(current),
                None,
            )?;
            info!(
                repo = repo_full_name,
                head = current,
                kept_findings = kept,
                "complexity scan skipped: head SHA unchanged"
            );
            return Ok(kept as usize);
        }
    }

    let files = discover_main_java_files(repo_path);
    if files.is_empty() {
        record_run(
            conn,
            repo_full_name,
            STATUS_SKIPPED_NO_SOURCES,
            0,
            started.elapsed().as_millis() as i64,
            head.as_deref(),
            None,
        )?;
        return Ok(0);
    }

    // Idempotency: drop the previous run's findings for this repo
    // (cascade clears attribution via the FK) before re-inserting.
    conn.execute(
        "DELETE FROM method_complexity_findings
         WHERE repo_full_name = ?",
        params![repo_full_name],
    )?;

    let mut written = 0usize;
    for rel in &files {
        let abs = repo_path.join(rel);
        let Ok(src) = std::fs::read(&abs) else {
            continue;
        };

        // Parse once for both the metrics cache and the targeted
        // testability rules; the classic-axis rules then read the
        // metrics back out of `method_metrics`-shaped data we
        // already built. The two scan paths internally re-parse the
        // file once each — collapsing them into a single parse-and-
        // walk pass is a step-9 micro-optimisation.
        for f in classic_findings_for_file(rel, &src, thresholds) {
            persist_finding(conn, project_id, repo_full_name, &f)?;
            written += 1;
        }
        for f in testability_findings_for_file(rel, &src) {
            persist_finding(conn, project_id, repo_full_name, &f)?;
            written += 1;
        }
        // Populate `method_metrics` so subsequent quality_delta /
        // future testability_findings_from_db calls don't have to
        // re-parse. method_metrics keeps its sprint_id PK column —
        // it's not part of the artifact migration.
        for m in crate::complexity::analyze_file(rel, &src) {
            persist_method_metrics(conn, sprint_id_for_metrics_cache, &m)?;
        }
    }

    let attribution_rows = match attribute_findings_for_repo(conn, repo_path, repo_full_name) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(repo = repo_full_name, error = %e, "complexity attribution failed; continuing without it");
            0
        }
    };

    record_run(
        conn,
        repo_full_name,
        STATUS_OK,
        written,
        started.elapsed().as_millis() as i64,
        head.as_deref(),
        None,
    )?;

    info!(
        repo = repo_full_name,
        files = files.len(),
        findings = written,
        attribution_rows,
        "complexity scan complete"
    );
    Ok(written)
}

/// Convenience: scan every directory under `entregues_dir/<project_name>`
/// that looks like a cloned repo (T-P3.4: sprint-free at the artifact
/// level). The `sprint_id_for_metrics_cache` is forwarded only to
/// `method_metrics` (which keeps its per-sprint shape); finding /
/// attribution / runs rows are sprint-free.
pub fn scan_project_to_db(
    conn: &rusqlite::Connection,
    project_root: &Path,
    sprint_id_for_metrics_cache: i64,
    project_id: i64,
    thresholds: &DetectorThresholdsConfig,
) -> rusqlite::Result<usize> {
    if !project_root.is_dir() {
        return Ok(0);
    }
    let mut total = 0usize;
    let entries = match std::fs::read_dir(project_root) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let repo_path = entry.path();
        let bare = entry.file_name().to_string_lossy().into_owned();
        let repo_full_name = resolve_qualified_repo_name(conn, &bare).unwrap_or(bare);
        match scan_repo_to_db(
            conn,
            &repo_path,
            &repo_full_name,
            sprint_id_for_metrics_cache,
            project_id,
            thresholds,
        ) {
            Ok(n) => total += n,
            Err(e) => {
                tracing::warn!(
                    repo = %repo_full_name,
                    error = %e,
                    "complexity scan failed; recording crashed status"
                );
                let _ = record_run(
                    conn,
                    &repo_full_name,
                    STATUS_CRASHED,
                    0,
                    0,
                    git_head_sha(&repo_path).as_deref(),
                    Some(&format!("{e}")),
                );
            }
        }
    }
    Ok(total)
}

/// `<org>/<repo>` resolver, mirrors
/// `architecture::resolve_qualified_repo_name`. Kept private here to
/// avoid a cross-crate dep just for this helper.
fn resolve_qualified_repo_name(conn: &rusqlite::Connection, bare: &str) -> Option<String> {
    use rusqlite::params;
    let like = format!("%/{}", bare);
    conn.query_row(
        "SELECT repo_full_name FROM pull_requests
         WHERE repo_full_name = ? OR repo_full_name LIKE ?
         ORDER BY (repo_full_name = ?) DESC, length(repo_full_name) DESC
         LIMIT 1",
        params![bare, like, bare],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|s| s.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_into_rule_finding_carries_extra_for_numeric_rules() {
        // W2.T2: in-memory Finding → RuleFinding lossless for rules with
        // measured_value + threshold (wide-signature, cyclomatic, etc.).
        let f = Finding {
            file_path: "src/main/java/Login.java".to_string(),
            class_name: Some("LoginController".to_string()),
            method_name: "authenticate".to_string(),
            start_line: 42,
            end_line: 99,
            rule_key: "wide-signature".to_string(),
            severity: Severity::Warning,
            measured_value: Some(12.0),
            threshold: Some(10.0),
            detail: "Method takes more parameters than the ceiling allows.".to_string(),
        };
        let r = f.into_rule_finding("udg/spring-x");
        assert_eq!(r.kind, RuleKind::Complexity);
        assert_eq!(r.severity, CoreSeverity::Warning);
        assert_eq!(r.rule_id, "wide-signature");
        assert_eq!(r.repo_full_name, "udg/spring-x");
        assert_eq!(r.file_repo_relative, "src/main/java/Login.java");
        assert_eq!(r.span, LineSpan::range(42, 99));
        assert_eq!(r.extra.as_deref(), Some("12 > 10"));
        assert_eq!(
            r.evidence,
            "Method takes more parameters than the ceiling allows."
        );
    }

    #[test]
    fn finding_into_rule_finding_omits_extra_for_boolean_rules() {
        // Targeted testability rules (broad-catch etc.) carry no
        // measured/threshold pair; the renderer must not see a bogus
        // "None > None" overflow.
        let f = Finding {
            file_path: "Foo.java".to_string(),
            class_name: None,
            method_name: "m".to_string(),
            start_line: 13,
            end_line: 13,
            rule_key: "broad-catch".to_string(),
            severity: Severity::Info,
            measured_value: None,
            threshold: None,
            detail: String::new(),
        };
        let r = f.into_rule_finding("o/r");
        assert_eq!(r.span, LineSpan::single(13));
        assert!(r.extra.is_none(), "boolean rules carry no overflow string");
        assert_eq!(r.severity, CoreSeverity::Info);
    }

    #[test]
    fn keeps_spring_main_java_file() {
        assert!(is_scannable_main_source("src/main/java/com/x/Foo.java"));
    }

    #[test]
    fn keeps_android_app_main_java_file() {
        assert!(is_scannable_main_source("app/src/main/java/com/x/Foo.java"));
    }

    #[test]
    fn rejects_test_tree() {
        assert!(!is_scannable_main_source(
            "src/test/java/com/x/FooTest.java"
        ));
        assert!(!is_scannable_main_source(
            "app/src/androidTest/java/com/x/FooIT.java"
        ));
    }

    #[test]
    fn rejects_build_and_generated_outputs() {
        assert!(!is_scannable_main_source(
            "app/build/generated/source/buildConfig/debug/com/x/BuildConfig.java"
        ));
        assert!(!is_scannable_main_source(
            "build/generated/sources/annotationProcessor/java/main/com/x/Foo_Impl.java"
        ));
        assert!(!is_scannable_main_source(
            "src/main/generated/com/x/Foo.java"
        ));
    }

    #[test]
    fn rejects_room_dagger_mapstruct_filenames() {
        assert!(!is_scannable_main_source(
            "src/main/java/com/x/db/AppDatabase_Impl.java"
        ));
        assert!(!is_scannable_main_source(
            "src/main/java/com/x/di/MyModule_Factory.java"
        ));
        assert!(!is_scannable_main_source(
            "src/main/java/com/x/di/MyFragment_MembersInjector.java"
        ));
        assert!(!is_scannable_main_source(
            "src/main/java/com/x/mapper/UserMapperImpl.java"
        ));
    }

    #[test]
    fn rejects_well_known_android_shims() {
        assert!(!is_scannable_main_source("app/src/main/java/com/x/R.java"));
        assert!(!is_scannable_main_source(
            "app/src/main/java/com/x/BuildConfig.java"
        ));
    }

    #[test]
    fn rejects_files_outside_main_roots() {
        assert!(!is_scannable_main_source("config/Foo.java"));
        assert!(!is_scannable_main_source("scripts/Util.java"));
        assert!(!is_scannable_main_source(
            "buildSrc/src/main/java/Plugin.java"
        ));
    }

    #[test]
    fn rejects_non_java_files() {
        assert!(!is_scannable_main_source("src/main/java/com/x/foo.kt"));
        assert!(!is_scannable_main_source(
            "src/main/resources/application.yml"
        ));
    }

    #[test]
    fn handles_windows_style_separators() {
        assert!(is_scannable_main_source(
            "src\\main\\java\\com\\x\\Foo.java"
        ));
        assert!(!is_scannable_main_source(
            "src\\test\\java\\com\\x\\FooTest.java"
        ));
    }

    fn th() -> DetectorThresholdsConfig {
        DetectorThresholdsConfig::default()
    }

    fn rule_keys(findings: &[Finding]) -> Vec<&str> {
        let mut k: Vec<&str> = findings.iter().map(|f| f.rule_key.as_str()).collect();
        k.sort();
        k
    }

    fn fired(findings: &[Finding], rule: &str) -> Option<Finding> {
        findings.iter().find(|f| f.rule_key == rule).cloned()
    }

    #[test]
    fn trivial_method_fires_no_complexity_rules() {
        let src = br#"class A { int f() { return 1; } }"#;
        let f = classic_findings_for_file("A.java", src, &th());
        assert!(f.is_empty(), "got {:?}", f);
    }

    #[test]
    fn cyclomatic_warning_at_warn_band_critical_at_crit_band() {
        // 11 ifs → CC = 12 → above warn (10), below crit (15)
        let mut body = String::from("class A { int f(int x) { int y = 0;");
        for i in 0..11 {
            body.push_str(&format!(" if (x == {i}) y++;"));
        }
        body.push_str(" return y; } }");
        let f = classic_findings_for_file("A.java", body.as_bytes(), &th());
        let cc = fired(&f, "cyclomatic").expect("cyclomatic must fire");
        assert_eq!(cc.severity, Severity::Warning);
        assert_eq!(cc.measured_value, Some(12.0));

        // 16 ifs → CC = 17 → above crit (15)
        let mut body = String::from("class A { int f(int x) { int y = 0;");
        for i in 0..16 {
            body.push_str(&format!(" if (x == {i}) y++;"));
        }
        body.push_str(" return y; } }");
        let f = classic_findings_for_file("A.java", body.as_bytes(), &th());
        let cc = fired(&f, "cyclomatic").expect("cyclomatic must fire");
        assert_eq!(cc.severity, Severity::Critical);
        assert_eq!(cc.measured_value, Some(17.0));
    }

    #[test]
    fn cognitive_fires_independently_of_cyclomatic_via_nested_branches() {
        // Deeply nested branches inflate cognitive complexity faster
        // than cyclomatic. Cognitive is ~21 here; CC is ~7.
        let src = br#"class A {
            int f(int x) {
                if (x > 0) {                 // +1
                    if (x > 1) {             // +2
                        if (x > 2) {         // +3
                            if (x > 3) {     // +4
                                if (x > 4) { // +5
                                    if (x > 5) { return 1; } // +6
                                }
                            }
                        }
                    }
                }
                return 0;
            }
        }"#;
        let f = classic_findings_for_file("A.java", src, &th());
        let cog = fired(&f, "cognitive").expect("cognitive must fire");
        assert!(
            cog.measured_value.unwrap() > 15.0,
            "got {:?}",
            cog.measured_value
        );
    }

    #[test]
    fn nesting_warning_at_5_levels() {
        // 5 nested `if`s → max nesting depth 5 → exceeds warn (4)
        let src = br#"class A {
            void f() {
                if (true) { if (true) { if (true) { if (true) { if (true) { } } } } }
            }
        }"#;
        let f = classic_findings_for_file("A.java", src, &th());
        let n = fired(&f, "nesting").expect("nesting must fire");
        assert_eq!(n.severity, Severity::Warning);
        assert!(n.measured_value.unwrap() >= 5.0);
    }

    #[test]
    fn long_method_warning_at_70_loc() {
        let mut body = String::from("class A { void f() {\n");
        for _ in 0..70 {
            body.push_str("int x = 0;\n");
        }
        body.push_str("} }\n");
        let f = classic_findings_for_file("A.java", body.as_bytes(), &th());
        let lm = fired(&f, "long-method").expect("long-method must fire");
        assert_eq!(lm.severity, Severity::Warning);
    }

    #[test]
    fn wide_signature_critical_at_9_parameters() {
        let src = br#"class A {
            void f(int a, int b, int c, int d, int e, int f, int g, int h, int i) { }
        }"#;
        let findings = classic_findings_for_file("A.java", src, &th());
        let ws = fired(&findings, "wide-signature").expect("wide-signature must fire");
        assert_eq!(ws.severity, Severity::Critical);
        assert_eq!(ws.measured_value, Some(9.0));
    }

    #[test]
    fn one_method_can_fire_multiple_rules() {
        // CC > 15, params > 5, LOC > 60
        let mut body = String::from(
            "class A { void f(int a, int b, int c, int d, int e, int g) { int y = 0;\n",
        );
        for i in 0..18 {
            body.push_str(&format!("if (a == {i}) y++;\n"));
        }
        for _ in 0..50 {
            body.push_str("y++;\n");
        }
        body.push_str("} }\n");
        let f = classic_findings_for_file("A.java", body.as_bytes(), &th());
        let keys = rule_keys(&f);
        assert!(keys.contains(&"cyclomatic"), "{:?}", keys);
        assert!(keys.contains(&"wide-signature"), "{:?}", keys);
        assert!(keys.contains(&"long-method"), "{:?}", keys);
    }

    fn t_findings(src: &[u8]) -> Vec<Finding> {
        testability_findings_for_file("A.java", src)
    }

    #[test]
    fn broad_catch_with_no_throw_fires() {
        let src = br#"class A {
            void f() {
                try { doStuff(); }
                catch (Exception e) { /* swallow */ }
            }
        }"#;
        let f = t_findings(src);
        let bc = fired(&f, "broad-catch").expect("broad-catch must fire");
        assert_eq!(bc.severity, Severity::Warning);
    }

    #[test]
    fn broad_catch_silent_when_rethrown() {
        let src = br#"class A {
            void f() throws RuntimeException {
                try { doStuff(); }
                catch (Exception e) { throw new RuntimeException(e); }
            }
        }"#;
        let f = t_findings(src);
        assert!(fired(&f, "broad-catch").is_none(), "got {:?}", f);
    }

    #[test]
    fn broad_catch_silent_for_specific_exception() {
        let src = br#"class A {
            void f() {
                try { doStuff(); }
                catch (java.io.IOException e) { /* swallow */ }
            }
        }"#;
        let f = t_findings(src);
        assert!(fired(&f, "broad-catch").is_none(), "got {:?}", f);
    }

    #[test]
    fn broad_catch_fires_for_multi_catch_with_throwable() {
        let src = br#"class A {
            void f() {
                try { doStuff(); }
                catch (java.io.IOException | Throwable e) { /* swallow */ }
            }
        }"#;
        let f = t_findings(src);
        assert!(fired(&f, "broad-catch").is_some(), "got {:?}", f);
    }

    #[test]
    fn non_deterministic_system_current_time_millis_fires() {
        let src = br#"class A {
            long f() { return System.currentTimeMillis(); }
        }"#;
        let f = t_findings(src);
        let n = fired(&f, "non-deterministic-call").expect("must fire");
        assert!(n.detail.contains("currentTimeMillis"));
    }

    #[test]
    fn non_deterministic_now_fires_only_for_time_classes() {
        let src = br#"import java.time.LocalDateTime; class A {
            LocalDateTime f() { return LocalDateTime.now(); }
        }"#;
        assert!(fired(&t_findings(src), "non-deterministic-call").is_some());

        // Bare `now()` on a user class must not flag.
        let src = br#"class A { class Clock { Object now() { return null; } } void f() { new Clock().now(); } }"#;
        assert!(
            fired(&t_findings(src), "non-deterministic-call").is_none(),
            "user .now() must not fire"
        );
    }

    #[test]
    fn non_deterministic_new_date_and_random_fire() {
        let src = br#"import java.util.Date; import java.util.Random; class A {
            void f() { Date d = new Date(); Random r = new Random(); }
        }"#;
        let f = t_findings(src);
        let n = fired(&f, "non-deterministic-call").expect("must fire");
        assert!(n.measured_value.unwrap() >= 2.0);
    }

    #[test]
    fn inline_collaborator_fires_for_new_service_in_business_method() {
        let src = br#"class Controller {
            String handle(int id) {
                UserService svc = new UserService();
                return svc.lookup(id);
            }
        }"#;
        let f = t_findings(src);
        let ic = fired(&f, "inline-collaborator").expect("must fire");
        assert!(ic.detail.contains("UserService"));
    }

    #[test]
    fn inline_collaborator_silent_in_factory_method() {
        let src = br#"class Module {
            UserService provideService() { return new UserService(); }
            UserService createService() { return new UserService(); }
            void setService(UserService s) { this.svc = new UserService(); }
            UserService svc;
        }"#;
        let f = t_findings(src);
        assert!(
            fired(&f, "inline-collaborator").is_none(),
            "factory-style methods must not fire: {f:?}"
        );
    }

    #[test]
    fn static_singleton_get_instance_fires() {
        let src = br#"class A {
            void f() { Auth.getInstance().login("x"); }
        }"#;
        let f = t_findings(src);
        let s = fired(&f, "static-singleton").expect("must fire");
        assert_eq!(s.severity, Severity::Info);
    }

    #[test]
    fn static_singleton_kotlin_style_instance_fires() {
        let src = br#"class A {
            void f() { Cache.INSTANCE.put("k", "v"); }
        }"#;
        let f = t_findings(src);
        assert!(fired(&f, "static-singleton").is_some(), "got {:?}", f);
    }

    #[test]
    fn static_singleton_silent_for_lowercase_receiver() {
        let src = br#"class A {
            void f(Helper helper) { helper.getInstance(); }
        }"#;
        let f = t_findings(src);
        assert!(
            fired(&f, "static-singleton").is_none(),
            "lowercase receiver must not fire: {f:?}"
        );
    }

    #[test]
    fn reflection_class_for_name_fires_without_reflect_import() {
        let src = br#"class A {
            void f() throws Exception { Class<?> c = Class.forName("x.Y"); }
        }"#;
        let f = t_findings(src);
        assert!(fired(&f, "reflection").is_some(), "got {:?}", f);
    }

    #[test]
    fn reflection_method_invoke_requires_reflect_import() {
        // No import → must NOT fire (could be a user method named invoke).
        let src = br#"class A {
            void f(Method m, Object t) throws Exception { m.invoke(t); }
            interface Method { Object invoke(Object t) throws Exception; }
        }"#;
        let f = t_findings(src);
        assert!(
            fired(&f, "reflection").is_none(),
            "Method.invoke without reflect import must not fire: {f:?}"
        );

        // With import → fires.
        let src = br#"import java.lang.reflect.Method;
        class A { void f(Method m, Object t) throws Exception { m.invoke(t); } }"#;
        let f = t_findings(src);
        assert!(fired(&f, "reflection").is_some(), "got {:?}", f);
    }

    // ----- Bad-line-weighted attribution unit tests --------------------

    #[test]
    fn classify_control_flow_recognises_keywords() {
        assert!(classify_control_flow("if (x > 0) {"));
        assert!(classify_control_flow("for (int i = 0; i < n; i++) {"));
        assert!(classify_control_flow("while (true) {"));
        assert!(classify_control_flow("switch (x) {"));
        assert!(classify_control_flow("case 1:"));
        assert!(classify_control_flow("} catch (Exception e) {"));
        assert!(classify_control_flow("a && b"));
        assert!(classify_control_flow("a || b"));
    }

    #[test]
    fn classify_control_flow_silent_on_plain_lines() {
        assert!(!classify_control_flow("int x = 1;"));
        assert!(!classify_control_flow("return result;"));
        assert!(!classify_control_flow("doStuff(arg);"));
    }

    #[test]
    fn strip_line_for_keyword_scan_drops_string_contents() {
        let s = strip_line_for_keyword_scan(r#"String x = "if you read this";"#);
        assert!(!s.contains("if you"), "stripped: {s}");
        // Identifier `x` survives.
        assert!(s.contains("x"));
    }

    #[test]
    fn strip_line_for_keyword_scan_drops_line_comments() {
        let s = strip_line_for_keyword_scan("int x = 1; // if you see this it's a bug");
        assert!(!s.contains("if you"));
        assert!(s.contains("int x"));
    }

    #[test]
    fn parse_offending_lines_extracts_line_numbers_from_detail() {
        let lines = parse_offending_lines("L42: calls now(); L77: instantiates Date");
        assert_eq!(lines, vec![42, 77]);
    }

    #[test]
    fn line_badness_offending_overrides_control_flow_classification() {
        // line 2 is `if (x) {` — would be ControlFlow, but offending list
        // pins it as Offending.
        let file: Vec<String> = vec![
            "void f() {".to_string(),
            "if (x) {".to_string(),
            "}".to_string(),
        ];
        let m = line_badness_map(&file, 1, 3, &[2]);
        assert_eq!(m.get(&2), Some(&LineBadness::Offending));
    }

    #[test]
    fn line_badness_skips_blank_and_brace_only_lines() {
        let file: Vec<String> = vec![
            "void f() {".to_string(),
            "".to_string(),
            "  {".to_string(),
            "  }".to_string(),
            "}".to_string(),
        ];
        let m = line_badness_map(&file, 1, 5, &[]);
        // Only line 1 (`void f() {`) is non-trivial in this snippet — but
        // it ends in `{`. We deliberately classify lines like that as
        // Plain (the trim isn't `{`-only).
        // The blank line and the standalone `{` / `}` lines must be absent.
        assert!(!m.contains_key(&2));
        assert!(!m.contains_key(&3));
        assert!(!m.contains_key(&4));
        assert!(!m.contains_key(&5));
    }

    // ----- End-to-end attribution against a real git repo --------------

    use rusqlite::Connection;
    use sprint_grader_core::db::apply_schema;
    use std::fs;
    use std::path::Path as StdPath;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(cwd: &StdPath, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git invocation");
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    fn init_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_path_buf();
        run_git(&path, &["init", "-q", "-b", "main"]);
        run_git(&path, &["config", "user.email", "alice@example.com"]);
        run_git(&path, &["config", "user.name", "Alice"]);
        (tmp, path)
    }

    fn commit_file(repo: &StdPath, rel: &str, body: &str, email: &str, name: &str, msg: &str) {
        let target = repo.join(rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, body).unwrap();
        run_git(repo, &["config", "user.email", email]);
        run_git(repo, &["config", "user.name", name]);
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-q", "-m", msg]);
    }

    fn seed_db(conn: &Connection) {
        conn.execute(
            "INSERT OR REPLACE INTO projects (id, slug, name) VALUES (1, 'p', 'P')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sprints (id, project_id, name, start_date, end_date)
             VALUES (1, 1, 's1', '2026-01-01T00:00:00Z', '2026-02-01T00:00:00Z')",
            [],
        )
        .unwrap();
        for (sid, email, login, full) in [
            ("alice", "alice@example.com", "alice", "Alice"),
            ("bob", "bob@example.com", "bob", "Bob"),
        ] {
            conn.execute(
                "INSERT OR REPLACE INTO students
                    (id, username, github_login, full_name, email, team_project_id)
                 VALUES (?, ?, ?, ?, ?, 1)",
                params![sid, login, login, full, email],
            )
            .unwrap();
            // Production code reads identities from student_github_identity
            // only; TrackDev's stored github_login is no longer trusted.
            conn.execute(
                "INSERT OR IGNORE INTO student_github_identity
                    (student_id, identity_kind, identity_value, weight, confidence)
                 VALUES (?, 'login', ?, 1.0, 1.0),
                        (?, 'email', ?, 1.0, 1.0)",
                params![sid, login, sid, email],
            )
            .unwrap();
        }
    }

    fn insert_finding(
        conn: &Connection,
        repo_full_name: &str,
        file_path: &str,
        rule: &str,
        severity: &str,
        start: i64,
        end: i64,
        detail: &str,
    ) -> i64 {
        conn.execute(
            "INSERT INTO method_complexity_findings
                (project_id, repo_full_name, file_path, class_name,
                 method_name, start_line, end_line, rule_key, severity,
                 measured_value, threshold, detail)
             VALUES (1, ?, ?, 'A', 'f', ?, ?, ?, ?, NULL, NULL, ?)",
            params![
                repo_full_name,
                file_path,
                start,
                end,
                rule,
                severity,
                detail
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn attr_for(conn: &Connection, finding_id: i64) -> Vec<(String, i64, f64, f64)> {
        let mut stmt = conn
            .prepare(
                "SELECT student_id, lines_attributed, weighted_lines, weight
                 FROM method_complexity_attribution WHERE finding_id = ?
                 ORDER BY student_id",
            )
            .unwrap();
        stmt.query_map([finding_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    #[test]
    fn single_author_gets_full_weight_on_classic_finding() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);

        let (_g, repo) = init_repo();
        let body = r#"class A {
    void f(int x) {
        int y = 0;
        if (x > 0) {
            y = x;
        }
    }
}
"#;
        commit_file(&repo, "A.java", body, "alice@example.com", "Alice", "init");

        let fid = insert_finding(&conn, "udg/x", "A.java", "cyclomatic", "WARNING", 2, 7, "");
        let n = attribute_findings_for_repo(&conn, &repo, "udg/x").unwrap();
        assert!(n > 0);
        let rows = attr_for(&conn, fid);
        assert_eq!(rows.len(), 1, "got {rows:?}");
        let (sid, _, _, weight) = &rows[0];
        assert_eq!(sid, "alice");
        assert!((weight - 1.0).abs() < 1e-9, "weight {weight}");
    }

    #[test]
    fn bad_line_weighting_concentrates_on_offending_author() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);

        let (_g, repo) = init_repo();
        // Method with a broad catch on lines 6–8. Alice writes the entire
        // method first.
        let v1 = "class A {
    void f(int x) {
        try {
            doStuff(x);
            doMore(x);
            doYetMore(x);
            int y = 0;
            int z = 0;
        } catch (Exception e) {
            // swallow it
            return;
        }
    }
}
";
        commit_file(&repo, "A.java", v1, "alice@example.com", "Alice", "init");

        // Bob renames a single local variable on line 7 — `int y = 0;`
        // becomes `int yy = 0;`. `git blame -w` will reattribute that
        // single line to Bob.
        let v2 = v1.replace("int y = 0;", "int yy = 0;");
        commit_file(&repo, "A.java", &v2, "bob@example.com", "Bob", "rename");

        // Finding is broad-catch on lines 3..=12 with offending line 9
        // (the `} catch (Exception e) {` header).
        let fid = insert_finding(
            &conn,
            "udg/x",
            "A.java",
            "broad-catch",
            "WARNING",
            3,
            12,
            "L9: catches Exception",
        );
        attribute_findings_for_repo(&conn, &repo, "udg/x").unwrap();
        let rows = attr_for(&conn, fid);
        assert_eq!(rows.len(), 2, "alice + bob: got {rows:?}");

        let alice = rows.iter().find(|r| r.0 == "alice").unwrap();
        let bob = rows.iter().find(|r| r.0 == "bob").unwrap();

        // Bob touched 1 line; alice touched the rest. Bob's line is a
        // plain assignment (`int yy = 0;`) → weight 1. Alice's lines
        // include the catch header (offending → weight 3) plus several
        // plain lines. Alice's weight must dominate.
        assert!(
            alice.3 > 0.85 && bob.3 < 0.15,
            "expected Alice >> Bob, got alice={alice:?} bob={bob:?}"
        );
        // Lines counted (not weighted): bob = 1.
        assert_eq!(bob.1, 1, "bob touched exactly one line");
        assert!(alice.1 >= 5, "alice should own most non-blank lines");
        // Weights sum to 1.
        assert!((alice.3 + bob.3 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rerun_replaces_attribution_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);

        let (_g, repo) = init_repo();
        let body = "class A {\n    void f() {\n        int x = 1;\n    }\n}\n";
        commit_file(&repo, "A.java", body, "alice@example.com", "Alice", "init");

        let fid = insert_finding(&conn, "udg/x", "A.java", "long-method", "WARNING", 2, 4, "");
        attribute_findings_for_repo(&conn, &repo, "udg/x").unwrap();
        let n1: i64 = conn
            .query_row(
                "SELECT count(*) FROM method_complexity_attribution WHERE finding_id = ?",
                [fid],
                |r| r.get(0),
            )
            .unwrap();
        attribute_findings_for_repo(&conn, &repo, "udg/x").unwrap();
        let n2: i64 = conn
            .query_row(
                "SELECT count(*) FROM method_complexity_attribution WHERE finding_id = ?",
                [fid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n1, n2, "rerun must not duplicate rows");
    }

    // ----- End-to-end repo scan ----------------------------------------

    #[test]
    fn scan_repo_to_db_writes_findings_metrics_attribution_and_runs_row() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);

        let (_g, repo) = init_repo();
        // Production source — must be scanned.
        let main_src = "class Foo {\n    void heavy() {\n        try {\n            doStuff();\n        } catch (Exception e) {\n            return;\n        }\n    }\n}\n";
        commit_file(
            &repo,
            "src/main/java/com/x/Foo.java",
            main_src,
            "alice@example.com",
            "Alice",
            "init",
        );
        // Test source under src/test — must be skipped.
        let test_src = "class FooTest { void t() { try { } catch (Exception e) {} } }\n";
        commit_file(
            &repo,
            "src/test/java/com/x/FooTest.java",
            test_src,
            "alice@example.com",
            "Alice",
            "tests",
        );
        // Generated source — must be skipped.
        let gen_src = "class Foo_Impl { void g() { try { } catch (Exception e) {} } }\n";
        commit_file(
            &repo,
            "src/main/java/com/x/Foo_Impl.java",
            gen_src,
            "alice@example.com",
            "Alice",
            "generated",
        );

        let th = DetectorThresholdsConfig::default();
        let n = scan_repo_to_db(&conn, &repo, "udg/x", 1, 1, &th).unwrap();
        assert!(n > 0, "expected at least one finding");

        // Must contain a broad-catch finding for Foo.heavy and NOT for
        // FooTest or Foo_Impl. (T-P3.4: findings table is sprint-free.)
        let files: Vec<String> = conn
            .prepare(
                "SELECT DISTINCT file_path FROM method_complexity_findings
                 WHERE repo_full_name = 'udg/x'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(files, vec!["src/main/java/com/x/Foo.java".to_string()]);

        // method_metrics populated for the same method.
        let mm: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM method_metrics WHERE sprint_id = 1
                 AND file_path = 'src/main/java/com/x/Foo.java'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(mm >= 1, "method_metrics row must be written");
        // Line range columns populated.
        let (start, end): (i64, i64) = conn
            .query_row(
                "SELECT start_line, end_line FROM method_metrics
                 WHERE method_name = 'heavy' AND sprint_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(start >= 1 && end >= start, "line range must be set");

        // Attribution row written for alice (single author). T-P3.4:
        // attribution has no sprint_id; scope by student.
        let attr: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM method_complexity_attribution
                 WHERE student_id = 'alice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(attr >= 1);

        // method_complexity_runs row records OK status with a head SHA.
        let (status, head): (String, Option<String>) = conn
            .query_row(
                "SELECT status, head_sha FROM method_complexity_runs
                 WHERE repo_full_name = 'udg/x'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "OK");
        assert!(head.unwrap().len() >= 7, "head sha must be persisted");
    }

    #[test]
    fn rerun_with_same_head_sha_short_circuits() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);

        let (_g, repo) = init_repo();
        let body = "class Foo {\n    void heavy() {\n        try { doStuff(); } catch (Exception e) { return; }\n    }\n}\n";
        commit_file(
            &repo,
            "src/main/java/com/x/Foo.java",
            body,
            "alice@example.com",
            "Alice",
            "init",
        );
        let th = DetectorThresholdsConfig::default();
        let first = scan_repo_to_db(&conn, &repo, "udg/x", 1, 1, &th).unwrap();
        assert!(first > 0);

        // Second run with no commit must short-circuit.
        let second = scan_repo_to_db(&conn, &repo, "udg/x", 1, 1, &th).unwrap();
        assert_eq!(first, second, "cached findings count must match");
        let status: String = conn
            .query_row(
                "SELECT status FROM method_complexity_runs WHERE repo_full_name = 'udg/x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "SKIPPED_HEAD_UNCHANGED");
    }

    #[test]
    fn rerun_after_new_commit_re_scans() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);

        let (_g, repo) = init_repo();
        let body = "class Foo {\n    void heavy() {\n        try { doStuff(); } catch (Exception e) { return; }\n    }\n}\n";
        commit_file(
            &repo,
            "src/main/java/com/x/Foo.java",
            body,
            "alice@example.com",
            "Alice",
            "init",
        );
        let th = DetectorThresholdsConfig::default();
        scan_repo_to_db(&conn, &repo, "udg/x", 1, 1, &th).unwrap();
        // New commit → head SHA moves → must re-scan, status OK.
        let body2 = "class Foo {\n    void heavy() {\n        try { doStuffMore(); } catch (Exception e) { return; }\n    }\n}\n";
        commit_file(
            &repo,
            "src/main/java/com/x/Foo.java",
            body2,
            "alice@example.com",
            "Alice",
            "edit",
        );
        scan_repo_to_db(&conn, &repo, "udg/x", 1, 1, &th).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM method_complexity_runs WHERE repo_full_name = 'udg/x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "OK", "must re-scan when head changes");
    }

    #[test]
    fn empty_project_records_skipped_no_sources_status() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);
        let (_g, repo) = init_repo();
        // No java files committed.
        commit_file(
            &repo,
            "README.md",
            "# x\n",
            "alice@example.com",
            "Alice",
            "init",
        );
        let th = DetectorThresholdsConfig::default();
        let n = scan_repo_to_db(&conn, &repo, "udg/x", 1, 1, &th).unwrap();
        assert_eq!(n, 0);
        let status: String = conn
            .query_row(
                "SELECT status FROM method_complexity_runs WHERE repo_full_name = 'udg/x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "SKIPPED_NO_SOURCES");
    }

    #[test]
    fn finding_with_zero_line_range_is_skipped() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        seed_db(&conn);
        let (_g, repo) = init_repo();
        commit_file(
            &repo,
            "A.java",
            "class A {}\n",
            "alice@example.com",
            "Alice",
            "init",
        );
        // start_line = 0 → SQL filter excludes the row.
        insert_finding(&conn, "udg/x", "A.java", "cyclomatic", "WARNING", 0, 0, "");
        let n = attribute_findings_for_repo(&conn, &repo, "udg/x").unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn line_range_set_on_each_finding() {
        let src = b"class A {\n  void f(int x) {\n    if (x > 0) { if (x > 1) { if (x > 2) { if (x > 3) { if (x > 4) {} } } } }\n  }\n}\n";
        let f = classic_findings_for_file("A.java", src, &th());
        assert!(!f.is_empty());
        for finding in &f {
            assert!(
                finding.start_line >= 2 && finding.end_line >= finding.start_line,
                "line range invalid: {finding:?}"
            );
        }
    }
}
