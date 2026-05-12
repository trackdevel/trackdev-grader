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
use tracing::warn;
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
    /// Match by class-name suffix (e.g. `"ViewModel"` matches
    /// `PostsViewModel`). Useful for Android rules where the convention is
    /// suffix-based rather than annotation-based.
    pub name_suffix: Option<String>,
    /// Negative match: fail the class match if the extends clause names
    /// this class. Used by `VIEWMODEL_HOLDS_CONTEXT` to skip
    /// `AndroidViewModel` subclasses.
    pub not_extends: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawAstRule {
    pub name: String,
    #[serde(default)]
    pub class_match: RawClassMatch,
    pub kind: String,
    /// Used by `forbidden_field_type`, `forbidden_constructor_param`,
    /// `forbidden_return_type`, `forbidden_method_param`,
    /// `must_null_in_lifecycle`, `forbidden_constructor_call`,
    /// `field_count_with_type_pattern`.
    #[serde(default)]
    pub type_regex: Option<String>,
    /// Used by `forbidden_method_call`. Matched against the *callee* string
    /// reconstructed from the AST (e.g. `userRepository.findAll`).
    #[serde(default)]
    pub call_regex: Option<String>,
    /// Used by `max_method_statements`, `field_count_with_type_pattern`.
    #[serde(default)]
    pub max: Option<usize>,
    /// Visibility filter for method-shaped rules
    /// (`forbidden_method_param`, `forbidden_return_type`). Accepts
    /// `"public"`, `"non-private"`, `"any"`, or absent (default: `any`).
    #[serde(default)]
    pub visibility: Option<String>,
    /// Used by `forbidden_import` (W1.3) — matched against each file-level
    /// import line.
    #[serde(default)]
    pub import_regex: Option<String>,
    /// Used by `must_null_in_lifecycle` (W1.4) — the lifecycle method name
    /// (e.g. `onDestroyView`) that should null the matching field.
    #[serde(default)]
    pub method_name: Option<String>,
    /// Used by `forbidden_call_source` (W1.5) — matched against the
    /// full source-text slice of a `method_invocation` (arguments
    /// included).
    #[serde(default)]
    pub source_regex: Option<String>,
    /// Used by `forbidden_field_type` (W1.6) — when set to `"static"`,
    /// only emit when the field carries that modifier.
    #[serde(default)]
    pub required_modifier: Option<String>,
    /// Used by `class_has_forbidden_annotation` (W2.1) and
    /// `method_annotation_visibility_mismatch` (W2.2). Matched against
    /// the annotation's simple name (no `@`).
    #[serde(default)]
    pub annotation_regex: Option<String>,
    /// `class_has_forbidden_annotation` (W2.1) scope. `"class"` matches
    /// only class-level annotations; `"class_or_method"` also matches
    /// annotations on method declarations. Default: `"class"`.
    #[serde(default)]
    pub scope: Option<String>,
    /// `method_annotation_visibility_mismatch` (W2.2) — the visibility
    /// that the annotated method MUST have. Fires when the method's
    /// visibility doesn't match.
    #[serde(default)]
    pub required_visibility: Option<String>,
    /// `forbidden_method_call` (W2.3) — when set, the call must have
    /// exactly this many arguments to fire.
    #[serde(default)]
    pub arg_count: Option<usize>,
    /// `forbidden_constructor_call` (W2.4) — when set, the constructor
    /// call must pass at least this many arguments to fire.
    #[serde(default)]
    pub min_args: Option<usize>,
    /// `parameter_annotation_requires_companion` (W2.5).
    #[serde(default)]
    pub trigger_annotation: Option<String>,
    /// `parameter_annotation_requires_companion` (W2.5).
    #[serde(default)]
    pub required_annotation_regex: Option<String>,
    /// `class_requires_annotation` (W3.1) — the class-level annotation
    /// the matched class MUST carry once the trigger condition fires
    /// (e.g. `"HiltViewModel"` for `MISSING_HILT_VIEWMODEL`).
    #[serde(default)]
    pub required_annotation: Option<String>,
    /// `class_requires_annotation` (W3.1) — the constructor-level
    /// annotation whose presence enables the rule (e.g. `"Inject"`).
    /// Without this trigger the rule stays silent.
    #[serde(default)]
    pub trigger_constructor_annotation: Option<String>,
    /// 3-stage non-DTO gate (W2.x). Suppresses a field/param/return-type
    /// finding when the offending type's simple name matches (e.g.
    /// stdlib value types: `String`, `UUID`, ...).
    #[serde(default)]
    pub type_allowlist: Option<String>,
    /// 3-stage non-DTO gate (W2.x). Suppresses a field/param/return-type
    /// finding when the resolved package of the offending type matches
    /// (e.g. `(?i)\.dto\.`). The package is resolved via the file's
    /// imports; types referenced without an explicit import resolve to
    /// the file's own package.
    #[serde(default)]
    pub type_package_allowlist: Option<String>,
    /// Wave A of the legacy-rule deprecation path: mark a rule
    /// `deprecated = true` and the loader emits a one-shot warning per
    /// rule name. The rule still fires — the goal is to surface the
    /// fact that a successor rule (named in `replaced_by`) is now the
    /// authoritative one. Wave B will run the DELETE migration that
    /// removes legacy rows from `architecture_violations`; Wave C
    /// removes the `[[ast_rule]]` block itself.
    #[serde(default)]
    pub deprecated: bool,
    /// Optional human-readable hint naming the rubric peer that
    /// supersedes a deprecated rule. Surfaced in the deprecation
    /// warning; has no behavioural effect.
    #[serde(default)]
    pub replaced_by: Option<String>,
    #[serde(default = "default_severity")]
    pub severity: String,
}

fn default_severity() -> String {
    "WARNING".to_string()
}

/// Method-visibility filter / expectation. `Any` is the default and
/// matches every method. `Public` matches only methods explicitly declared
/// `public`. `NonPrivate` matches public, protected, and package-private
/// (anything that isn't `private`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    NonPrivate,
    Any,
}

impl Visibility {
    fn parse(s: Option<&str>) -> anyhow::Result<Self> {
        match s {
            None => Ok(Visibility::Any),
            Some(v) => match v.trim() {
                "public" => Ok(Visibility::Public),
                "non-private" => Ok(Visibility::NonPrivate),
                "any" => Ok(Visibility::Any),
                other => anyhow::bail!(
                    "unknown visibility '{other}' — expected one of: public, non-private, any"
                ),
            },
        }
    }
}

/// Concrete declared visibility of a method (read from its `modifiers`
/// node). Used internally only; `Visibility` above is the filter type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclaredVisibility {
    Public,
    Protected,
    Private,
    PackagePrivate,
}

impl DeclaredVisibility {
    fn matches_filter(self, filter: Visibility) -> bool {
        match filter {
            Visibility::Any => true,
            Visibility::Public => matches!(self, DeclaredVisibility::Public),
            Visibility::NonPrivate => !matches!(self, DeclaredVisibility::Private),
        }
    }
}

/// `class_has_forbidden_annotation` (W2.1) scope. `Class` matches only
/// class-level annotations; `ClassOrMethod` also walks method
/// declarations and matches annotations on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassAnnotationScope {
    Class,
    ClassOrMethod,
}

impl ClassAnnotationScope {
    fn parse(s: Option<&str>) -> anyhow::Result<Self> {
        match s {
            None | Some("class") => Ok(ClassAnnotationScope::Class),
            Some("class_or_method") => Ok(ClassAnnotationScope::ClassOrMethod),
            Some(other) => {
                anyhow::bail!("unknown scope '{other}' — expected 'class' or 'class_or_method'")
            }
        }
    }
}

/// Optional type-shape suppressors shared by field / param / return-type
/// rules (W2.x non-DTO gate). Each suppressor is independent: if any one
/// matches, the finding is dropped.
///
/// Note on regex flavour: Rust's `regex` crate doesn't support
/// lookarounds. The plan's first sketch used `(?i)^(?!.*(dto|request|
/// response)).+$` in `type_regex` to express "fire when the name is
/// **not** DTO-shaped"; that pattern doesn't compile. The TOML design
/// instead folds the "type-name suggests DTO" signal into
/// `type_allowlist` (alongside stdlib value types), so it becomes one
/// more positive suppressor. `type_regex` then stays a simple "match
/// anything that looks like a type" filter.
#[derive(Debug, Clone, Default)]
pub struct TypeSuppressors {
    /// Name-based suppressor (stages 2 + 3 from the plan, combined).
    /// Suppresses when the offending type's simple name matches — DTO
    /// shape (`(?i).*(dto|request|response).*`) and stdlib value types
    /// (`^String$`, `^UUID$`, …) live here.
    pub type_allowlist: Option<Regex>,
    /// Package-name suppressor (stage 1). Suppresses when the resolved
    /// source package of the offending type matches (e.g.
    /// `(?i)(^|\.)(dto|dtos)(\.|$)`).
    pub type_package_allowlist: Option<Regex>,
}

impl TypeSuppressors {
    fn from_raw(raw: &RawAstRule) -> anyhow::Result<Self> {
        Ok(TypeSuppressors {
            type_allowlist: match raw.type_allowlist.as_deref() {
                Some(s) => Some(Regex::new(s)?),
                None => None,
            },
            type_package_allowlist: match raw.type_package_allowlist.as_deref() {
                Some(s) => Some(Regex::new(s)?),
                None => None,
            },
        })
    }

    /// Returns true when this finding should be suppressed (i.e. a
    /// suppressor matches).
    fn suppresses(
        &self,
        type_name: &str,
        own_package: &str,
        imports: &[(String, Node<'_>)],
    ) -> bool {
        if let Some(r) = &self.type_allowlist {
            if r.is_match(type_name) {
                return true;
            }
        }
        if let Some(r) = &self.type_package_allowlist {
            let pkg = resolve_type_package(type_name, own_package, imports);
            if r.is_match(pkg) {
                return true;
            }
        }
        false
    }
}

/// Resolve the source package of a type referenced inside a file. Looks
/// for an `import` line whose last identifier equals `type_name`; if
/// found, returns the leading dotted prefix (`import com.x.dto.UserDto`
/// → `com.x.dto`). When no matching import exists, the reference is
/// same-package and the file's own package is returned.
fn resolve_type_package<'a>(
    type_name: &str,
    own_package: &'a str,
    imports: &'a [(String, Node<'_>)],
) -> &'a str {
    for (text, _node) in imports {
        let stripped = text.strip_suffix(".*").unwrap_or(text);
        if let Some((pkg, last)) = stripped.rsplit_once('.') {
            if last == type_name {
                return pkg;
            }
        }
    }
    own_package
}

#[derive(Debug, Clone)]
pub struct ClassMatcher {
    pub annotation: Option<String>,
    pub extends: Option<String>,
    pub implements: Option<String>,
    pub package_glob: Option<PackagePattern>,
    pub name_suffix: Option<String>,
    pub not_extends: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AstRuleKind {
    ForbiddenFieldType {
        type_regex: Regex,
        required_modifier: Option<String>,
        suppressors: TypeSuppressors,
    },
    ForbiddenConstructorParam {
        type_regex: Regex,
    },
    ForbiddenMethodCall {
        call_regex: Regex,
        arg_count: Option<usize>,
    },
    ForbiddenReturnType {
        type_regex: Regex,
        visibility: Visibility,
        suppressors: TypeSuppressors,
    },
    MaxMethodStatements {
        max: usize,
    },
    ForbiddenMethodParam {
        type_regex: Regex,
        visibility: Visibility,
        suppressors: TypeSuppressors,
    },
    ForbiddenImport {
        import_regex: Regex,
    },
    MustNullInLifecycle {
        type_regex: Regex,
        method_name: String,
    },
    ForbiddenCallSource {
        regex: Regex,
    },
    /// W2.1 — class (or class/method) carries a forbidden annotation.
    ClassHasForbiddenAnnotation {
        annotation_regex: Regex,
        scope: ClassAnnotationScope,
    },
    /// W2.2 — method carries `annotation_regex` but its visibility does
    /// not match `required_visibility` (e.g. `@Transactional` on a
    /// non-`public` method).
    MethodAnnotationVisibilityMismatch {
        annotation_regex: Regex,
        required_visibility: Visibility,
    },
    /// W2.4 — `new <Type>(...)` constructor calls matching `type_regex`,
    /// optionally requiring at least `min_args` arguments.
    ForbiddenConstructorCall {
        type_regex: Regex,
        min_args: Option<usize>,
    },
    /// W2.5 — parameter carries `trigger_annotation` but none of its
    /// other annotations match `required_annotation_regex`.
    ParameterAnnotationRequiresCompanion {
        trigger_annotation: String,
        required_annotation_regex: Regex,
    },
    /// W2.6 — the class declares more than `max` fields whose type
    /// matches `type_regex`.
    FieldCountWithTypePattern {
        type_regex: Regex,
        max: usize,
    },
    /// W3.1 — when any constructor carries `trigger_constructor_annotation`,
    /// the class itself MUST carry `required_annotation` as a class-level
    /// annotation; otherwise fires. Used by `MISSING_HILT_VIEWMODEL`.
    ClassRequiresAnnotation {
        required_annotation: String,
        trigger_constructor_annotation: String,
    },
}

impl AstRuleKind {
    pub fn label(&self) -> &'static str {
        match self {
            AstRuleKind::ForbiddenFieldType { .. } => "ast_forbidden_field_type",
            AstRuleKind::ForbiddenConstructorParam { .. } => "ast_forbidden_constructor_param",
            AstRuleKind::ForbiddenMethodCall { .. } => "ast_forbidden_method_call",
            AstRuleKind::ForbiddenReturnType { .. } => "ast_forbidden_return_type",
            AstRuleKind::MaxMethodStatements { .. } => "ast_max_method_statements",
            AstRuleKind::ForbiddenMethodParam { .. } => "ast_forbidden_method_param",
            AstRuleKind::ForbiddenImport { .. } => "ast_forbidden_import",
            AstRuleKind::MustNullInLifecycle { .. } => "ast_must_null_in_lifecycle",
            AstRuleKind::ForbiddenCallSource { .. } => "ast_forbidden_call_source",
            AstRuleKind::ClassHasForbiddenAnnotation { .. } => "ast_class_has_forbidden_annotation",
            AstRuleKind::MethodAnnotationVisibilityMismatch { .. } => {
                "ast_method_annotation_visibility_mismatch"
            }
            AstRuleKind::ForbiddenConstructorCall { .. } => "ast_forbidden_constructor_call",
            AstRuleKind::ParameterAnnotationRequiresCompanion { .. } => {
                "ast_parameter_annotation_requires_companion"
            }
            AstRuleKind::FieldCountWithTypePattern { .. } => "ast_field_count_with_type_pattern",
            AstRuleKind::ClassRequiresAnnotation { .. } => "ast_class_requires_annotation",
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
        // Wave A: surface deprecated rules so reviewers see them in logs
        // long before Wave B's DELETE migration runs.
        if raw.deprecated {
            match raw.replaced_by.as_deref() {
                Some(peer) => warn!(
                    rule = %raw.name,
                    replaced_by = peer,
                    "deprecated architecture rule loaded; the named successor is authoritative — \
                     this block will be removed in a later wave"
                ),
                None => warn!(
                    rule = %raw.name,
                    "deprecated architecture rule loaded with no `replaced_by` hint; \
                     this block will be removed in a later wave"
                ),
            }
        }
        // Compile cross-kind state before destructuring the RawClassMatch
        // so the resulting borrow of `raw` is still whole.
        let visibility = Visibility::parse(raw.visibility.as_deref())?;
        let suppressors = TypeSuppressors::from_raw(&raw)?;
        let class_match = ClassMatcher {
            annotation: raw.class_match.annotation.clone(),
            extends: raw.class_match.extends.clone(),
            implements: raw.class_match.implements.clone(),
            package_glob: raw
                .class_match
                .package_glob
                .as_deref()
                .map(PackagePattern::new),
            name_suffix: raw.class_match.name_suffix.clone(),
            not_extends: raw.class_match.not_extends.clone(),
        };
        let kind = match raw.kind.as_str() {
            "forbidden_field_type" => AstRuleKind::ForbiddenFieldType {
                type_regex: compile_regex(raw.type_regex.clone(), "type_regex", &raw.name)?,
                required_modifier: raw.required_modifier.clone(),
                suppressors: suppressors.clone(),
            },
            "forbidden_constructor_param" => AstRuleKind::ForbiddenConstructorParam {
                type_regex: compile_regex(raw.type_regex.clone(), "type_regex", &raw.name)?,
            },
            "forbidden_method_call" => AstRuleKind::ForbiddenMethodCall {
                call_regex: compile_regex(raw.call_regex.clone(), "call_regex", &raw.name)?,
                arg_count: raw.arg_count,
            },
            "forbidden_return_type" => AstRuleKind::ForbiddenReturnType {
                type_regex: compile_regex(raw.type_regex.clone(), "type_regex", &raw.name)?,
                visibility,
                suppressors: suppressors.clone(),
            },
            "max_method_statements" => AstRuleKind::MaxMethodStatements {
                max: raw.max.ok_or_else(|| {
                    anyhow::anyhow!(
                        "ast_rule '{}' kind=max_method_statements requires `max`",
                        raw.name
                    )
                })?,
            },
            "forbidden_method_param" => AstRuleKind::ForbiddenMethodParam {
                type_regex: compile_regex(raw.type_regex.clone(), "type_regex", &raw.name)?,
                visibility,
                suppressors,
            },
            "forbidden_import" => AstRuleKind::ForbiddenImport {
                import_regex: compile_regex(raw.import_regex.clone(), "import_regex", &raw.name)?,
            },
            "must_null_in_lifecycle" => AstRuleKind::MustNullInLifecycle {
                type_regex: compile_regex(raw.type_regex.clone(), "type_regex", &raw.name)?,
                method_name: raw.method_name.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "ast_rule '{}' kind=must_null_in_lifecycle requires `method_name`",
                        raw.name
                    )
                })?,
            },
            "forbidden_call_source" => AstRuleKind::ForbiddenCallSource {
                regex: compile_regex(raw.source_regex.clone(), "source_regex", &raw.name)?,
            },
            "class_has_forbidden_annotation" => AstRuleKind::ClassHasForbiddenAnnotation {
                annotation_regex: compile_regex(
                    raw.annotation_regex.clone(),
                    "annotation_regex",
                    &raw.name,
                )?,
                scope: ClassAnnotationScope::parse(raw.scope.as_deref())?,
            },
            "method_annotation_visibility_mismatch" => {
                AstRuleKind::MethodAnnotationVisibilityMismatch {
                    annotation_regex: compile_regex(
                        raw.annotation_regex.clone(),
                        "annotation_regex",
                        &raw.name,
                    )?,
                    required_visibility: Visibility::parse(raw.required_visibility.as_deref())?,
                }
            }
            "forbidden_constructor_call" => AstRuleKind::ForbiddenConstructorCall {
                type_regex: compile_regex(raw.type_regex.clone(), "type_regex", &raw.name)?,
                min_args: raw.min_args,
            },
            "parameter_annotation_requires_companion" => {
                AstRuleKind::ParameterAnnotationRequiresCompanion {
                    trigger_annotation: raw.trigger_annotation.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "ast_rule '{}' kind=parameter_annotation_requires_companion \
                             requires `trigger_annotation`",
                            raw.name
                        )
                    })?,
                    required_annotation_regex: compile_regex(
                        raw.required_annotation_regex.clone(),
                        "required_annotation_regex",
                        &raw.name,
                    )?,
                }
            }
            "field_count_with_type_pattern" => AstRuleKind::FieldCountWithTypePattern {
                type_regex: compile_regex(raw.type_regex.clone(), "type_regex", &raw.name)?,
                max: raw.max.ok_or_else(|| {
                    anyhow::anyhow!(
                        "ast_rule '{}' kind=field_count_with_type_pattern requires `max`",
                        raw.name
                    )
                })?,
            },
            "class_requires_annotation" => AstRuleKind::ClassRequiresAnnotation {
                required_annotation: raw.required_annotation.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "ast_rule '{}' kind=class_requires_annotation requires `required_annotation`",
                        raw.name
                    )
                })?,
                trigger_constructor_annotation: raw
                    .trigger_constructor_annotation
                    .clone()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "ast_rule '{}' kind=class_requires_annotation requires \
                             `trigger_constructor_annotation`",
                            raw.name
                        )
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

/// File-scope context shared by every rule application within one file.
/// Collected once at the top of `check_java_file` so the visitor doesn't
/// re-parse imports per rule.
struct FileContext<'a> {
    #[allow(dead_code)]
    package: &'a str,
    /// `(import-text, import-declaration node)` pairs. Import text is
    /// stripped of the `import` keyword and trailing semicolon.
    imports: Vec<(String, Node<'a>)>,
}

/// Walk the top of the parse tree once to gather `import_declaration`
/// children. tree-sitter-java emits these as direct children of the
/// program root (before the first class).
fn collect_imports<'a>(root: Node<'a>, source: &'a [u8]) -> Vec<(String, Node<'a>)> {
    let mut out = Vec::new();
    for c in children(root) {
        if c.kind() != "import_declaration" {
            continue;
        }
        // `import_declaration` text is e.g. `import com.x.Foo;` — strip
        // the keyword and trailing semicolon to mirror what
        // `scanner::parse_java` returns elsewhere in the architecture
        // crate.
        let raw = node_text(c, source);
        let trimmed = raw
            .trim()
            .strip_prefix("import")
            .unwrap_or(raw.trim())
            .trim();
        let cleaned = trimmed.trim_end_matches(';').trim();
        let cleaned = cleaned.strip_prefix("static ").unwrap_or(cleaned).trim();
        if !cleaned.is_empty() {
            out.push((cleaned.to_string(), c));
        }
    }
    out
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

    let root = tree.root_node();
    let file_ctx = FileContext {
        package: package_name,
        imports: collect_imports(root, source),
    };

    let mut out = Vec::new();
    visit_classes(root, source, &mut |class_node| {
        let info = ClassInfo::new(class_node, source);
        for rule in rules {
            if !class_matches(&rule.class_match, &info, package_name) {
                continue;
            }
            apply_rule(rule, &info, &file_ctx, source, rel_path, &mut out);
        }
    });
    out
}

fn visit_classes<F: FnMut(Node)>(node: Node, _source: &[u8], cb: &mut F) {
    let kind = node.kind();
    if kind == "class_declaration" || kind == "interface_declaration" || kind == "enum_declaration"
    {
        cb(node);
    }
    for child in children(node) {
        visit_classes(child, _source, cb);
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
        children(self.node)
            .into_iter()
            .find(|c| c.kind() == "class_body")
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
    if let Some(suffix) = matcher.name_suffix.as_deref() {
        if !info.name.ends_with(suffix) {
            return false;
        }
    }
    if let Some(forbidden) = matcher.not_extends.as_deref() {
        if info.extends.as_deref() == Some(forbidden) {
            return false;
        }
    }
    true
}

fn apply_rule(
    rule: &AstRule,
    info: &ClassInfo,
    file_ctx: &FileContext,
    source: &[u8],
    rel_path: &str,
    out: &mut Vec<Violation>,
) {
    let body = match info.class_body() {
        Some(b) => b,
        None => return,
    };
    match &rule.kind {
        AstRuleKind::ForbiddenFieldType {
            type_regex,
            required_modifier,
            suppressors,
        } => {
            for member in children(body) {
                if member.kind() != "field_declaration" {
                    continue;
                }
                if let Some(want) = required_modifier.as_deref() {
                    if !field_has_modifier(member, source, want) {
                        continue;
                    }
                }
                let ty = type_text_of_field(member, source);
                if let Some(t) = ty {
                    if !type_regex.is_match(&t) {
                        continue;
                    }
                    if suppressors.suppresses(&t, file_ctx.package, &file_ctx.imports) {
                        continue;
                    }
                    out.push(make_violation(
                        rel_path,
                        rule,
                        &format!("{}::{}", info.name, t),
                        member,
                    ));
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
        AstRuleKind::ForbiddenMethodCall {
            call_regex,
            arg_count,
        } => {
            for member in children(body) {
                if member.kind() != "method_declaration"
                    && member.kind() != "constructor_declaration"
                {
                    continue;
                }
                let mut hits: Vec<(String, Node)> = Vec::new();
                collect_method_invocations(member, source, &mut hits);
                for (callee, n) in hits {
                    if !call_regex.is_match(&callee) {
                        continue;
                    }
                    if let Some(want) = arg_count {
                        if count_call_args(n) != *want {
                            continue;
                        }
                    }
                    out.push(make_violation(
                        rel_path,
                        rule,
                        &format!("{}::call({})", info.name, callee),
                        n,
                    ));
                }
            }
        }
        AstRuleKind::ForbiddenReturnType {
            type_regex,
            visibility,
            suppressors,
        } => {
            for member in children(body) {
                if member.kind() != "method_declaration" {
                    continue;
                }
                if !method_visibility(member, source).matches_filter(*visibility) {
                    continue;
                }
                let ty = method_return_type(member, source);
                if let Some(t) = ty {
                    if !type_regex.is_match(&t) {
                        continue;
                    }
                    if suppressors.suppresses(&t, file_ctx.package, &file_ctx.imports) {
                        continue;
                    }
                    out.push(make_violation(
                        rel_path,
                        rule,
                        &format!("{}::return({})", info.name, t),
                        member,
                    ));
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
        AstRuleKind::ForbiddenMethodParam {
            type_regex,
            visibility,
            suppressors,
        } => {
            for member in children(body) {
                if member.kind() != "method_declaration" {
                    continue;
                }
                if !method_visibility(member, source).matches_filter(*visibility) {
                    continue;
                }
                let m_name = method_name(member, source).unwrap_or_else(|| "<anon>".into());
                for param in formal_parameters(member) {
                    let ty = type_text_of_param(param, source);
                    if let Some(t) = ty {
                        if !type_regex.is_match(&t) {
                            continue;
                        }
                        if suppressors.suppresses(&t, file_ctx.package, &file_ctx.imports) {
                            continue;
                        }
                        out.push(make_violation(
                            rel_path,
                            rule,
                            &format!("{}::{}::param({})", info.name, m_name, t),
                            param,
                        ));
                    }
                }
            }
        }
        AstRuleKind::ForbiddenImport { import_regex } => {
            // File-level scan gated by the rule's class match (the visitor
            // already filtered us to a matching class). If multiple
            // matched classes share the file, the inserted rows collide
            // on the violations PK (which includes `offending_import` +
            // `start_line` set from the import node), so `INSERT OR
            // REPLACE` collapses duplicates downstream.
            for (text, node) in &file_ctx.imports {
                if import_regex.is_match(text) {
                    out.push(make_violation(rel_path, rule, text, *node));
                }
            }
        }
        AstRuleKind::MustNullInLifecycle {
            type_regex,
            method_name: lifecycle,
        } => {
            // Find matching fields by type, then check whether the
            // lifecycle method exists and nulls each one.
            let matching_fields: Vec<(Node, String)> = children(body)
                .into_iter()
                .filter(|m| m.kind() == "field_declaration")
                .filter_map(|m| {
                    let ty = type_text_of_field(m, source)?;
                    if type_regex.is_match(&ty) {
                        let names = field_variable_names(m, source);
                        Some(names.into_iter().map(move |n| (m, n)).collect::<Vec<_>>())
                    } else {
                        None
                    }
                })
                .flatten()
                .collect();
            if matching_fields.is_empty() {
                return;
            }
            let lifecycle_method = children(body).into_iter().find(|m| {
                m.kind() == "method_declaration"
                    && method_name(*m, source).as_deref() == Some(lifecycle.as_str())
            });
            for (field_node, field_name) in &matching_fields {
                match lifecycle_method {
                    None => out.push(make_violation(
                        rel_path,
                        rule,
                        &format!("{}::{}::no-{}", info.name, field_name, lifecycle),
                        *field_node,
                    )),
                    Some(m) => {
                        if !method_assigns_field_to_null(m, source, field_name) {
                            out.push(make_violation(
                                rel_path,
                                rule,
                                &format!("{}::{}::not-nulled", info.name, field_name),
                                *field_node,
                            ));
                        }
                    }
                }
            }
        }
        AstRuleKind::ForbiddenCallSource { regex } => {
            for member in children(body) {
                if member.kind() != "method_declaration"
                    && member.kind() != "constructor_declaration"
                {
                    continue;
                }
                let m_name = method_name(member, source).unwrap_or_else(|| "<anon>".into());
                let mut hits: Vec<Node> = Vec::new();
                collect_method_invocation_nodes(member, &mut hits);
                for inv in hits {
                    let start = inv.start_byte();
                    let end = inv.end_byte().min(source.len());
                    let src = std::str::from_utf8(&source[start..end]).unwrap_or("");
                    if regex.is_match(src) {
                        let line = inv.start_position().row as u32 + 1;
                        out.push(make_violation(
                            rel_path,
                            rule,
                            &format!("{}::{}::call-line-{}", info.name, m_name, line),
                            inv,
                        ));
                    }
                }
            }
        }
        AstRuleKind::ClassHasForbiddenAnnotation {
            annotation_regex,
            scope,
        } => {
            // Class-level annotations.
            if info
                .annotations
                .iter()
                .any(|a| annotation_regex.is_match(a))
            {
                let anchor = class_identifier(info.node).unwrap_or(info.node);
                let line = anchor.start_position().row as u32 + 1;
                out.push(make_violation(
                    rel_path,
                    rule,
                    &format!("{}::class-annot@L{}", info.name, line),
                    anchor,
                ));
            }
            // Method-level annotations (when scope allows).
            if matches!(scope, ClassAnnotationScope::ClassOrMethod) {
                for member in children(body) {
                    if member.kind() != "method_declaration" {
                        continue;
                    }
                    let annots = method_annotations(member, source);
                    let hit = annots.iter().any(|a| annotation_regex.is_match(a));
                    if hit {
                        let m_name = method_name(member, source).unwrap_or_else(|| "<anon>".into());
                        let anchor = method_identifier(member).unwrap_or(member);
                        let line = anchor.start_position().row as u32 + 1;
                        out.push(make_violation(
                            rel_path,
                            rule,
                            &format!("{}::{}::annot@L{}", info.name, m_name, line),
                            anchor,
                        ));
                    }
                }
            }
        }
        AstRuleKind::MethodAnnotationVisibilityMismatch {
            annotation_regex,
            required_visibility,
        } => {
            for member in children(body) {
                if member.kind() != "method_declaration" {
                    continue;
                }
                let annots = method_annotations(member, source);
                if !annots.iter().any(|a| annotation_regex.is_match(a)) {
                    continue;
                }
                let actual = method_visibility(member, source);
                if actual.matches_filter(*required_visibility) {
                    continue;
                }
                let m_name = method_name(member, source).unwrap_or_else(|| "<anon>".into());
                let anchor = method_identifier(member).unwrap_or(member);
                out.push(make_violation(
                    rel_path,
                    rule,
                    &format!("{}::{}::visibility-mismatch", info.name, m_name),
                    anchor,
                ));
            }
        }
        AstRuleKind::ForbiddenConstructorCall {
            type_regex,
            min_args,
        } => {
            // Walk every method/constructor body for `object_creation_expression`
            // nodes matching the type regex.
            for member in children(body) {
                if member.kind() != "method_declaration"
                    && member.kind() != "constructor_declaration"
                {
                    continue;
                }
                let mut hits: Vec<Node> = Vec::new();
                collect_object_creations(member, &mut hits);
                for n in hits {
                    let ty = object_creation_type_name(n, source);
                    if let Some(t) = ty {
                        if !type_regex.is_match(&t) {
                            continue;
                        }
                        if let Some(min) = min_args {
                            if count_object_creation_args(n) < *min {
                                continue;
                            }
                        }
                        out.push(make_violation(
                            rel_path,
                            rule,
                            &format!("{}::new({})", info.name, t),
                            n,
                        ));
                    }
                }
            }
        }
        AstRuleKind::ParameterAnnotationRequiresCompanion {
            trigger_annotation,
            required_annotation_regex,
        } => {
            for member in children(body) {
                if member.kind() != "method_declaration"
                    && member.kind() != "constructor_declaration"
                {
                    continue;
                }
                let m_name = method_name(member, source).unwrap_or_else(|| "<anon>".into());
                for param in formal_parameters(member) {
                    let annots = param_annotations(param, source);
                    if !annots.iter().any(|a| a == trigger_annotation) {
                        continue;
                    }
                    let has_companion =
                        annots.iter().any(|a| required_annotation_regex.is_match(a));
                    if has_companion {
                        continue;
                    }
                    let pty = type_text_of_param(param, source).unwrap_or_else(|| "?".into());
                    out.push(make_violation(
                        rel_path,
                        rule,
                        &format!(
                            "{}::{}::param({})::missing-{}-companion",
                            info.name, m_name, pty, trigger_annotation
                        ),
                        param,
                    ));
                }
            }
        }
        AstRuleKind::FieldCountWithTypePattern { type_regex, max } => {
            let mut count = 0usize;
            for member in children(body) {
                if member.kind() != "field_declaration" {
                    continue;
                }
                if let Some(t) = type_text_of_field(member, source) {
                    if type_regex.is_match(&t) {
                        count += 1;
                    }
                }
            }
            if count > *max {
                let anchor = class_identifier(info.node).unwrap_or(info.node);
                out.push(make_violation(
                    rel_path,
                    rule,
                    &format!("{}::field-count::{}>{}", info.name, count, max),
                    anchor,
                ));
            }
        }
        AstRuleKind::ClassRequiresAnnotation {
            required_annotation,
            trigger_constructor_annotation,
        } => {
            // Trigger: any constructor carries the trigger annotation.
            // `method_annotations` walks the modifier list and works for
            // constructor_declaration too (same AST shape).
            let has_trigger = children(body).into_iter().any(|m| {
                m.kind() == "constructor_declaration"
                    && method_annotations(m, source)
                        .iter()
                        .any(|a| a == trigger_constructor_annotation)
            });
            if !has_trigger {
                return;
            }
            if info.annotations.iter().any(|a| a == required_annotation) {
                return;
            }
            let anchor = class_identifier(info.node).unwrap_or(info.node);
            out.push(make_violation(
                rel_path,
                rule,
                &format!("{}::missing-@{}", info.name, required_annotation),
                anchor,
            ));
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
        severity: Some(rule.severity.clone()),
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

/// Generic-type "wrappers" whose outer name does not encode the
/// semantically interesting type. `List<UserDto>` is a list-of-DTO at
/// the API boundary; the rubric treats it as a DTO return. `Map` is
/// intentionally absent — it has two type parameters and unwrapping is
/// ambiguous; it stays in the stdlib allowlist by its outer name.
const GENERIC_WRAPPERS: &[&str] = &[
    // Spring / reactive
    "ResponseEntity",
    "Mono",
    "Flux",
    // JDK collections + value containers
    "Optional",
    "List",
    "Collection",
    "Set",
    "Iterable",
    "Iterator",
    "Stream",
    "Queue",
    "Deque",
    // Spring Data pagination
    "Page",
    "Slice",
    "PageImpl",
    // async
    "CompletableFuture",
    "Future",
    "Callable",
];

fn is_generic_wrapper(name: &str) -> bool {
    GENERIC_WRAPPERS.contains(&name)
}

/// For a `generic_type` node whose outer name is in `GENERIC_WRAPPERS`,
/// return the inner type-argument node so callers can keep drilling
/// toward the meaningful inner type. Returns `None` when the node
/// isn't a recognised wrapper or has no type-argument child.
fn unwrap_generic_wrapper<'a>(node: Node<'a>, source: &[u8]) -> Option<Node<'a>> {
    if node.kind() != "generic_type" {
        return None;
    }
    let outer = simple_type_name(node, source)?;
    if !is_generic_wrapper(&outer) {
        return None;
    }
    let args = children(node)
        .into_iter()
        .find(|c| c.kind() == "type_arguments")?;
    children(args).into_iter().find(|ta| {
        let k = ta.kind();
        k == "type_identifier"
            || k == "scoped_type_identifier"
            || k == "generic_type"
            || k.ends_with("_type")
    })
}

/// "Effective" simple type name for a type node, used by the suppressor
/// path of `forbidden_field_type` / `forbidden_method_param` /
/// `forbidden_return_type`. Unwraps recognised generic wrappers so
/// `List<UserDto>` is tested as `UserDto`, matching the rubric's prose
/// promise that "generic wrappers are stripped before the type is
/// tested". Non-wrapped types pass through `simple_type_name`
/// unchanged.
fn effective_simple_type_name(node: Node, source: &[u8]) -> Option<String> {
    if let Some(inner) = unwrap_generic_wrapper(node, source) {
        return effective_simple_type_name(inner, source);
    }
    simple_type_name(node, source)
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
    // Returns the *effective* type name — generic wrappers like
    // `List<UserDto>` / `Optional<UserDto>` are stripped so the inner
    // type drives the rule decision.
    for c in children(node) {
        let k = c.kind();
        if k == "modifiers" {
            continue;
        }
        if k.ends_with("_type")
            || k == "type_identifier"
            || k == "scoped_type_identifier"
            || k == "generic_type"
            || k == "array_type"
        {
            return effective_simple_type_name(c, source);
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
    // Same wrapper-stripping policy as `type_text_of_field`.
    for c in children(node) {
        let k = c.kind();
        if k == "modifiers" {
            continue;
        }
        if k.ends_with("_type")
            || k == "type_identifier"
            || k == "scoped_type_identifier"
            || k == "generic_type"
            || k == "array_type"
        {
            return effective_simple_type_name(c, source);
        }
    }
    None
}

fn method_return_type(method: Node, source: &[u8]) -> Option<String> {
    // method_declaration: modifiers? type_parameters? type identifier formal_parameters ...
    // The return type is the first type-shaped child after `modifiers` /
    // `type_parameters`. `void_type` is excluded — there is no type name
    // to match a blacklist against. Same wrapper-stripping policy as
    // `type_text_of_field`: `List<UserDto>` reports `UserDto`.
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
            return effective_simple_type_name(c, source);
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

/// Collect every `method_invocation` node inside `node` (no callee
/// reconstruction; used by `forbidden_call_source` which inspects the raw
/// source slice).
fn collect_method_invocation_nodes<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    if node.kind() == "method_invocation" {
        out.push(node);
    }
    for c in children(node) {
        collect_method_invocation_nodes(c, out);
    }
}

/// Returns the declared visibility of a method by scanning its `modifiers`
/// node for `public`/`protected`/`private` keywords. Absence of any is
/// package-private.
fn method_visibility(method: Node, source: &[u8]) -> DeclaredVisibility {
    for c in children(method) {
        if c.kind() != "modifiers" {
            continue;
        }
        for m in children(c) {
            match m.kind() {
                "public" => return DeclaredVisibility::Public,
                "protected" => return DeclaredVisibility::Protected,
                "private" => return DeclaredVisibility::Private,
                "modifier" => {
                    // Some grammar versions wrap keywords in a `modifier`
                    // node. Fall back to comparing the raw text.
                    let t = node_text(m, source);
                    match t.trim() {
                        "public" => return DeclaredVisibility::Public,
                        "protected" => return DeclaredVisibility::Protected,
                        "private" => return DeclaredVisibility::Private,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    DeclaredVisibility::PackagePrivate
}

/// Returns whether a `field_declaration` has the named modifier (e.g.
/// `"static"`, `"final"`) as a direct keyword child of its `modifiers`
/// node.
fn field_has_modifier(field: Node, source: &[u8], want: &str) -> bool {
    for c in children(field) {
        if c.kind() != "modifiers" {
            continue;
        }
        for m in children(c) {
            // tree-sitter-java may emit `static` as a leaf node-kind, OR
            // wrap it as a generic `modifier` whose text is the keyword.
            if m.kind() == want {
                return true;
            }
            if m.kind() == "modifier" && node_text(m, source).trim() == want {
                return true;
            }
        }
    }
    false
}

/// Return the simple identifier names declared by a
/// `field_declaration`. `private Foo a, b, c;` → `["a", "b", "c"]`.
fn field_variable_names(field: Node, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in children(field) {
        if c.kind() != "variable_declarator" {
            continue;
        }
        for vc in children(c) {
            if vc.kind() == "identifier" {
                out.push(node_text(vc, source));
                break;
            }
        }
    }
    out
}

/// Returns whether the method body contains an `assignment_expression`
/// of the shape `<field_name> = null` (with or without `this.` prefix).
fn method_assigns_field_to_null(method: Node, source: &[u8], field_name: &str) -> bool {
    let body = match children(method).into_iter().find(|c| c.kind() == "block") {
        Some(b) => b,
        None => return false,
    };
    let mut found = false;
    visit_assignment_expressions(body, &mut |assign| {
        if assignment_targets_field_with_null(assign, source, field_name) {
            found = true;
        }
    });
    found
}

fn visit_assignment_expressions<F: FnMut(Node)>(node: Node, cb: &mut F) {
    if node.kind() == "assignment_expression" {
        cb(node);
    }
    for c in children(node) {
        visit_assignment_expressions(c, cb);
    }
}

/// Collect annotation names declared directly on a `method_declaration`'s
/// modifier list. Strips leading `@` and any package qualifier (returns
/// the simple name only, e.g. `Transactional`).
fn method_annotations(method: Node, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in children(method) {
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

/// Collect annotation names declared directly on a `formal_parameter`'s
/// modifier list.
fn param_annotations(param: Node, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in children(param) {
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

/// Number of *real* argument nodes (commas/parentheses excluded) inside
/// a `method_invocation`'s `argument_list`.
fn count_call_args(invocation: Node) -> usize {
    children(invocation)
        .into_iter()
        .find(|c| c.kind() == "argument_list")
        .map(count_argument_list_args)
        .unwrap_or(0)
}

fn count_argument_list_args(arg_list: Node) -> usize {
    children(arg_list)
        .into_iter()
        .filter(|c| {
            let k = c.kind();
            // tree-sitter-java keeps `(`, `,`, `)` as anonymous siblings;
            // anything else is a real argument expression.
            k != "(" && k != ")" && k != ","
        })
        .count()
}

/// Collect every `object_creation_expression` descendant of `node`.
fn collect_object_creations<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    if node.kind() == "object_creation_expression" {
        out.push(node);
    }
    for c in children(node) {
        collect_object_creations(c, out);
    }
}

/// Pull the simple type name from an `object_creation_expression` —
/// `new com.x.Foo<Bar>(...)` → `Foo`.
fn object_creation_type_name(node: Node, source: &[u8]) -> Option<String> {
    for c in children(node) {
        let k = c.kind();
        if k == "type_identifier"
            || k == "scoped_type_identifier"
            || k == "generic_type"
            || k.ends_with("_type")
        {
            return simple_type_name(c, source);
        }
    }
    None
}

fn count_object_creation_args(node: Node) -> usize {
    children(node)
        .into_iter()
        .find(|c| c.kind() == "argument_list")
        .map(count_argument_list_args)
        .unwrap_or(0)
}

/// Find the class's identifier node (the `class_declaration` carries one
/// direct `identifier` child for the class name).
fn class_identifier(node: Node) -> Option<Node> {
    children(node)
        .into_iter()
        .find(|c| c.kind() == "identifier")
}

/// Find the method's identifier node — same scan as `method_name` but
/// returning the `Node` so callers can anchor a violation at the name
/// rather than the whole method.
fn method_identifier(method: Node) -> Option<Node> {
    let mut saw_type = false;
    for c in children(method) {
        let k = c.kind();
        if k == "modifiers" || k == "type_parameters" {
            continue;
        }
        if !saw_type
            && (k.ends_with("_type")
                || k == "void_type"
                || k == "type_identifier"
                || k == "scoped_type_identifier"
                || k == "generic_type"
                || k == "array_type")
        {
            saw_type = true;
            continue;
        }
        if k == "identifier" {
            return Some(c);
        }
    }
    None
}

fn assignment_targets_field_with_null(assign: Node, source: &[u8], field_name: &str) -> bool {
    // assignment_expression: <lhs> = <rhs> — grammar emits the children in
    // textual order: lhs, '=' (or compound op), rhs.
    let kids = children(assign);
    if kids.len() < 2 {
        return false;
    }
    let lhs = kids.first();
    let rhs = kids.last();
    let lhs_ok = lhs.is_some_and(|n| {
        let txt = node_text(*n, source);
        let t = txt.trim();
        t == field_name || t == format!("this.{field_name}")
    });
    let rhs_ok = rhs.is_some_and(|n| {
        let txt = node_text(*n, source);
        txt.trim() == "null"
    });
    lhs_ok && rhs_ok
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

    // ---------- W1.1: forbidden_method_param ----------

    #[test]
    fn forbidden_method_param_visibility_public_filters_to_public_methods() {
        let r = rule(
            r#"
            name = "service-no-entity-param"
            class_match.annotation = "Service"
            kind = "forbidden_method_param"
            type_regex = "^User$"
            visibility = "public"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            public class S {
                public User create(User u) { return u; }
                User loadInternal(Long id) { return null; }
                private User helper(User u) { return u; }
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(
            v.len(),
            1,
            "only the public `create(User)` parameter should fire, got: {v:?}"
        );
        assert!(v[0].offending_import.contains("create"));
        assert!(v[0].offending_import.contains("param(User)"));
    }

    #[test]
    fn forbidden_method_param_default_visibility_any_fires_on_all_methods() {
        let r = rule(
            r#"
            name = "service-no-entity-param-any"
            class_match.annotation = "Service"
            kind = "forbidden_method_param"
            type_regex = "^User$"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            public class S {
                public User create(User u) { return u; }
                User loadInternal(User u) { return u; }
                private User helper(User u) { return u; }
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(
            v.len(),
            3,
            "default visibility = Any should fire on every method, got: {v:?}"
        );
    }

    #[test]
    fn forbidden_method_param_disambiguates_methods_in_offending_import() {
        let r = rule(
            r#"
            name = "service-no-entity-param-uniq"
            class_match.annotation = "Service"
            kind = "forbidden_method_param"
            type_regex = "^User$"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            public class S {
                public void a(User u) {}
                public void b(User u) {}
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 2);
        // PK uniqueness: descriptors carry the method name.
        let descriptors: Vec<&str> = v.iter().map(|x| x.offending_import.as_str()).collect();
        assert!(descriptors.iter().any(|d| d.contains("::a::param")));
        assert!(descriptors.iter().any(|d| d.contains("::b::param")));
    }

    // ---------- W1.2: forbidden_return_type visibility ----------

    #[test]
    fn forbidden_return_type_visibility_public_skips_package_private_methods() {
        let r = rule(
            r#"
            name = "service-no-entity-return"
            class_match.annotation = "Service"
            kind = "forbidden_return_type"
            type_regex = "^User$"
            visibility = "public"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            public class S {
                public User create() { return null; }
                User loadInternal() { return null; }
                private User helper() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 1, "only the public method should fire: {v:?}");
        assert!(v[0].offending_import.contains("return(User)"));
    }

    #[test]
    fn forbidden_return_type_without_visibility_is_backward_compatible() {
        // No `visibility` field — defaults to Any, must fire on every
        // matching return type (including the package-private helper).
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
                public User a() { return null; }
                User b() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 2);
    }

    // ---------- W1.3: forbidden_import (AST file-level scan) ----------

    #[test]
    fn forbidden_import_fires_only_when_class_shape_matches() {
        let r = rule(
            r#"
            name = "entity-no-javax"
            class_match.annotation = "Entity"
            kind = "forbidden_import"
            import_regex = "^javax\\.(persistence|validation)\\."
            "#,
        );
        // Class IS @Entity → fires.
        let src_bad = r#"
            package com.x.model;
            import javax.persistence.Entity;
            import javax.validation.constraints.NotNull;
            @Entity
            public class Post {}
        "#;
        let v = check_java_file(
            std::slice::from_ref(&r),
            "Post.java",
            "com.x.model",
            src_bad.as_bytes(),
        );
        assert_eq!(v.len(), 2, "two offending imports → two rows: {v:?}");
        assert!(v
            .iter()
            .any(|x| x.offending_import.contains("javax.persistence.Entity")));
        assert!(v.iter().any(|x| x
            .offending_import
            .contains("javax.validation.constraints.NotNull")));

        // Class NOT @Entity → no fire (class-shape gate).
        let src_good = r#"
            package com.x.model;
            import javax.persistence.Entity;
            public class Post {}
        "#;
        let v2 = check_java_file(&[r], "Post.java", "com.x.model", src_good.as_bytes());
        assert!(
            v2.is_empty(),
            "class-shape gate must block the rule: {v2:?}"
        );
    }

    #[test]
    fn forbidden_import_anchors_each_violation_to_its_own_import_line() {
        let r = rule(
            r#"
            name = "entity-no-javax-anchor"
            class_match.annotation = "Entity"
            kind = "forbidden_import"
            import_regex = "^javax\\."
            "#,
        );
        let src = "package com.x.model;\n\
                   import javax.persistence.Entity;\n\
                   import javax.validation.constraints.NotNull;\n\
                   @Entity\n\
                   public class Post {}\n";
        let v = check_java_file(&[r], "Post.java", "com.x.model", src.as_bytes());
        assert_eq!(v.len(), 2);
        let lines: Vec<u32> = v.iter().filter_map(|x| x.start_line).collect();
        assert!(lines.contains(&2));
        assert!(lines.contains(&3));
    }

    // ---------- W1.4: must_null_in_lifecycle ----------

    #[test]
    fn must_null_in_lifecycle_fires_when_method_absent() {
        let r = rule(
            r#"
            name = "fragment-binding-leak"
            class_match.extends = "Fragment"
            kind = "must_null_in_lifecycle"
            type_regex = ".*Binding$"
            method_name = "onDestroyView"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class HomeFragment extends Fragment {
                private FragmentHomeBinding binding;
            }
        "#;
        let v = check_java_file(&[r], "HomeFragment.java", "com.x.home", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert!(v[0].offending_import.contains("binding"));
        assert!(v[0].offending_import.contains("no-onDestroyView"));
    }

    #[test]
    fn must_null_in_lifecycle_fires_when_method_does_not_null_field() {
        let r = rule(
            r#"
            name = "fragment-binding-leak"
            class_match.extends = "Fragment"
            kind = "must_null_in_lifecycle"
            type_regex = ".*Binding$"
            method_name = "onDestroyView"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class HomeFragment extends Fragment {
                private FragmentHomeBinding binding;
                @Override
                public void onDestroyView() {
                    super.onDestroyView();
                }
            }
        "#;
        let v = check_java_file(&[r], "HomeFragment.java", "com.x.home", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert!(v[0].offending_import.contains("not-nulled"));
    }

    #[test]
    fn must_null_in_lifecycle_silent_when_method_nulls_field() {
        let r = rule(
            r#"
            name = "fragment-binding-ok"
            class_match.extends = "Fragment"
            kind = "must_null_in_lifecycle"
            type_regex = ".*Binding$"
            method_name = "onDestroyView"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class HomeFragment extends Fragment {
                private FragmentHomeBinding binding;
                @Override
                public void onDestroyView() {
                    super.onDestroyView();
                    binding = null;
                }
            }
        "#;
        let v = check_java_file(&[r], "HomeFragment.java", "com.x.home", src.as_bytes());
        assert!(
            v.is_empty(),
            "binding=null in onDestroyView is the fix: {v:?}"
        );
    }

    #[test]
    fn must_null_in_lifecycle_silent_when_no_matching_field() {
        let r = rule(
            r#"
            name = "fragment-binding-empty"
            class_match.extends = "Fragment"
            kind = "must_null_in_lifecycle"
            type_regex = ".*Binding$"
            method_name = "onDestroyView"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class HomeFragment extends Fragment {
                private String unrelated;
            }
        "#;
        let v = check_java_file(&[r], "HomeFragment.java", "com.x.home", src.as_bytes());
        assert!(
            v.is_empty(),
            "no binding field → vacuously satisfied: {v:?}"
        );
    }

    // ---------- W1.5: forbidden_call_source ----------

    #[test]
    fn forbidden_call_source_fires_on_observe_this_pattern() {
        let r = rule(
            r#"
            name = "fragment-observe-this"
            class_match.extends = "Fragment"
            kind = "forbidden_call_source"
            source_regex = "\\.observe\\(\\s*this\\s*,"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class HomeFragment extends Fragment {
                void f() {
                    viewModel.getUsers().observe(this, x -> render(x));
                }
            }
        "#;
        let v = check_java_file(&[r], "HomeFragment.java", "com.x.home", src.as_bytes());
        assert!(!v.is_empty(), "observe(this, …) should fire: {v:?}");
    }

    #[test]
    fn forbidden_call_source_silent_on_view_lifecycle_owner() {
        let r = rule(
            r#"
            name = "fragment-observe-vlo-ok"
            class_match.extends = "Fragment"
            kind = "forbidden_call_source"
            source_regex = "\\.observe\\(\\s*this\\s*,"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class HomeFragment extends Fragment {
                void f() {
                    viewModel.getUsers().observe(getViewLifecycleOwner(), x -> render(x));
                }
            }
        "#;
        let v = check_java_file(&[r], "HomeFragment.java", "com.x.home", src.as_bytes());
        assert!(v.is_empty(), "getViewLifecycleOwner() is correct: {v:?}");
    }

    #[test]
    fn forbidden_call_source_disambiguates_two_calls_in_one_method() {
        let r = rule(
            r#"
            name = "fragment-observe-this-dup"
            class_match.extends = "Fragment"
            kind = "forbidden_call_source"
            source_regex = "\\.observe\\(\\s*this\\s*,"
            "#,
        );
        let src = "package com.x.home;\n\
                   public class HomeFragment extends Fragment {\n\
                       void f() {\n\
                           a.observe(this, x -> {});\n\
                           b.observe(this, x -> {});\n\
                       }\n\
                   }\n";
        let v = check_java_file(&[r], "HomeFragment.java", "com.x.home", src.as_bytes());
        assert_eq!(v.len(), 2);
        let descriptors: Vec<&str> = v.iter().map(|x| x.offending_import.as_str()).collect();
        // Descriptors must be distinct so the PK doesn't collapse them.
        assert_ne!(descriptors[0], descriptors[1], "got: {descriptors:?}");
    }

    // ---------- W1.6: forbidden_field_type required_modifier ----------

    #[test]
    fn forbidden_field_type_required_modifier_only_fires_on_static_field() {
        let r = rule(
            r#"
            name = "no-static-view-field"
            kind = "forbidden_field_type"
            type_regex = "^(Context|Activity|View)$"
            required_modifier = "static"
            "#,
        );
        let src = r#"
            package com.x.holder;
            public class Holder {
                private static Activity sActivity;
                private Activity activity;
                private static final String NAME = "x";
            }
        "#;
        let v = check_java_file(&[r], "Holder.java", "com.x.holder", src.as_bytes());
        assert_eq!(
            v.len(),
            1,
            "only the static Activity field should fire: {v:?}"
        );
        assert!(v[0].offending_import.contains("Activity"));
    }

    #[test]
    fn forbidden_field_type_without_required_modifier_is_backward_compatible() {
        // Pre-W1.6 rules without `required_modifier` must keep firing
        // regardless of `static`.
        let r = rule(
            r#"
            name = "controller-no-repo-field-bc"
            class_match.annotation = "RestController"
            kind = "forbidden_field_type"
            type_regex = ".*Repository$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                private UserRepository repo;
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
    }

    // ---------- W1.7: class_match name_suffix / not_extends ----------

    #[test]
    fn class_match_name_suffix_matches_without_annotation() {
        let r = rule(
            r#"
            name = "viewmodel-shape-by-name"
            class_match.name_suffix = "ViewModel"
            kind = "forbidden_field_type"
            type_regex = "^Context$"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class PostsViewModel {
                private Context context;
            }
        "#;
        let v = check_java_file(&[r], "PostsViewModel.java", "com.x.home", src.as_bytes());
        assert_eq!(v.len(), 1, "suffix-only class match should fire: {v:?}");
    }

    #[test]
    fn class_match_not_extends_excludes_subclass() {
        // Pair name_suffix with not_extends so the suffix accepts both
        // classes; not_extends is what makes the difference for the
        // AndroidViewModel subclass.
        let r = rule(
            r#"
            name = "viewmodel-shape-with-exclude"
            class_match.name_suffix = "ViewModel"
            class_match.not_extends = "AndroidViewModel"
            kind = "forbidden_field_type"
            type_regex = "^Context$"
            "#,
        );
        // Name matches AND extends ViewModel (not the excluded one) — fires.
        let src1 = r#"
            package com.x.home;
            public class PostsViewModel extends ViewModel {
                private Context context;
            }
        "#;
        assert_eq!(
            check_java_file(
                std::slice::from_ref(&r),
                "PostsViewModel.java",
                "com.x.home",
                src1.as_bytes(),
            )
            .len(),
            1
        );
        // Name still matches, but `not_extends` excludes it.
        let src2 = r#"
            package com.x.home;
            public class PostsViewModel extends AndroidViewModel {
                private Context context;
            }
        "#;
        assert!(
            check_java_file(&[r], "PostsViewModel.java", "com.x.home", src2.as_bytes()).is_empty(),
            "not_extends should suppress the rule"
        );
    }

    // ---------- W2.1: class_has_forbidden_annotation ----------

    #[test]
    fn class_has_forbidden_annotation_class_level_fires_once() {
        let r = rule(
            r#"
            name = "controller-no-transactional"
            class_match.annotation = "RestController"
            kind = "class_has_forbidden_annotation"
            annotation_regex = "^Transactional$"
            scope = "class_or_method"
            severity = "CRITICAL"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            @Transactional
            public class C {
                public void f() {}
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind.as_str(), "ast_class_has_forbidden_annotation");
        assert_eq!(v[0].severity.as_deref(), Some("CRITICAL"));
    }

    #[test]
    fn class_has_forbidden_annotation_method_level_fires_only_with_class_or_method_scope() {
        let r_method_scope = rule(
            r#"
            name = "controller-no-transactional-cm"
            class_match.annotation = "RestController"
            kind = "class_has_forbidden_annotation"
            annotation_regex = "^Transactional$"
            scope = "class_or_method"
            "#,
        );
        let r_class_only = rule(
            r#"
            name = "controller-no-transactional-c"
            class_match.annotation = "RestController"
            kind = "class_has_forbidden_annotation"
            annotation_regex = "^Transactional$"
            scope = "class"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                @Transactional
                public void f() {}
            }
        "#;
        let v1 = check_java_file(
            &[r_method_scope],
            "C.java",
            "com.x.controller",
            src.as_bytes(),
        );
        assert_eq!(
            v1.len(),
            1,
            "class_or_method should pick up the @Transactional method"
        );
        let v2 = check_java_file(
            &[r_class_only],
            "C.java",
            "com.x.controller",
            src.as_bytes(),
        );
        assert!(v2.is_empty(), "class scope must ignore method annotations");
    }

    #[test]
    fn class_has_forbidden_annotation_silent_when_no_match() {
        let r = rule(
            r#"
            name = "controller-no-transactional-silent"
            class_match.annotation = "RestController"
            kind = "class_has_forbidden_annotation"
            annotation_regex = "^Transactional$"
            scope = "class_or_method"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                public void f() {}
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty(), "no @Transactional → no fire: {v:?}");
    }

    #[test]
    fn class_has_forbidden_annotation_entity_lombok_data_matches() {
        let r = rule(
            r#"
            name = "entity-no-lombok-data"
            class_match.annotation = "Entity"
            kind = "class_has_forbidden_annotation"
            annotation_regex = "^(Data|EqualsAndHashCode|ToString)$"
            scope = "class"
            "#,
        );
        let src = r#"
            package com.x.entity;
            @Entity
            @Data
            public class Post {}
        "#;
        let v = check_java_file(&[r], "Post.java", "com.x.entity", src.as_bytes());
        assert_eq!(v.len(), 1);
    }

    // ---------- W2.2: method_annotation_visibility_mismatch ----------

    #[test]
    fn transactional_on_private_method_fires() {
        let r = rule(
            r#"
            name = "transactional-must-be-public"
            kind = "method_annotation_visibility_mismatch"
            annotation_regex = "^Transactional$"
            required_visibility = "public"
            severity = "CRITICAL"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            class S {
                @Transactional
                private void f() {}
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].severity.as_deref(), Some("CRITICAL"));
    }

    #[test]
    fn transactional_on_package_private_method_fires() {
        let r = rule(
            r#"
            name = "transactional-must-be-public"
            kind = "method_annotation_visibility_mismatch"
            annotation_regex = "^Transactional$"
            required_visibility = "public"
            "#,
        );
        let src = r#"
            package com.x.service;
            class S {
                @Transactional
                void f() {}
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn transactional_on_public_method_does_not_fire() {
        let r = rule(
            r#"
            name = "transactional-must-be-public"
            kind = "method_annotation_visibility_mismatch"
            annotation_regex = "^Transactional$"
            required_visibility = "public"
            "#,
        );
        let src = r#"
            package com.x.service;
            class S {
                @Transactional
                public void f() {}
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert!(v.is_empty());
    }

    #[test]
    fn transactional_annotation_with_args_still_matches_by_simple_name() {
        let r = rule(
            r#"
            name = "transactional-args"
            kind = "method_annotation_visibility_mismatch"
            annotation_regex = "^Transactional$"
            required_visibility = "public"
            "#,
        );
        let src = r#"
            package com.x.service;
            class S {
                @Transactional(readOnly = true)
                void f() {}
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 1, "annotation with arguments must still match");
    }

    // ---------- W2.3: forbidden_method_call arg_count ----------

    #[test]
    fn forbidden_method_call_arg_count_zero_filters_to_no_arg_calls() {
        let r = rule(
            r#"
            name = "unbounded-findAll"
            class_match.annotation = "Service"
            kind = "forbidden_method_call"
            call_regex = "\\.findAll$"
            arg_count = 0
            severity = "CRITICAL"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            class S {
                Object a() { return userRepository.findAll(); }
                Object b() { return userRepository.findAll(pageable); }
                Object c() { return userRepository.findAll(spec, pageable); }
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 1, "only the zero-arg findAll should fire: {v:?}");
        assert_eq!(v[0].severity.as_deref(), Some("CRITICAL"));
    }

    #[test]
    fn forbidden_method_call_without_arg_count_keeps_existing_semantics() {
        let r = rule(
            r#"
            name = "any-findAll"
            class_match.annotation = "Service"
            kind = "forbidden_method_call"
            call_regex = "\\.findAll$"
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            class S {
                Object a() { return userRepository.findAll(); }
                Object b() { return userRepository.findAll(pageable); }
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 2, "no arg_count → fires regardless of arity");
    }

    // ---------- W2.4: forbidden_constructor_call ----------

    #[test]
    fn forbidden_constructor_call_fires_on_new_response_with_two_args() {
        let r = rule(
            r#"
            name = "manual-dto-mapping"
            class_match.annotation = "RestController"
            kind = "forbidden_constructor_call"
            type_regex = "^\\w+(Dto|Response|Request)$"
            min_args = 2
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                Object handle(User user) {
                    return new UserResponse(user.getId(), user.getEmail());
                }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert!(v[0].offending_import.contains("new(UserResponse)"));
    }

    #[test]
    fn forbidden_constructor_call_silent_with_one_arg_when_min_two() {
        let r = rule(
            r#"
            name = "manual-dto-mapping-min2"
            class_match.annotation = "RestController"
            kind = "forbidden_constructor_call"
            type_regex = "^\\w+(Dto|Response|Request)$"
            min_args = 2
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                Object handle(User user) {
                    return new UserResponse(user);
                }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(
            v.is_empty(),
            "single-arg copy constructor must not fire: {v:?}"
        );
    }

    #[test]
    fn forbidden_constructor_call_silent_when_mapper_used() {
        let r = rule(
            r#"
            name = "manual-dto-mapping-mapper"
            class_match.annotation = "RestController"
            kind = "forbidden_constructor_call"
            type_regex = "^\\w+(Dto|Response|Request)$"
            min_args = 2
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                Object handle(User user) {
                    return userMapper.toResponse(user);
                }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty());
    }

    // ---------- W2.5: parameter_annotation_requires_companion ----------

    #[test]
    fn missing_valid_on_request_body_fires() {
        let r = rule(
            r#"
            name = "missing-valid"
            class_match.annotation = "RestController"
            kind = "parameter_annotation_requires_companion"
            trigger_annotation = "RequestBody"
            required_annotation_regex = "^(Valid|Validated)$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public void create(@RequestBody CreateXRequest req) {}
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn valid_companion_on_request_body_suppresses() {
        let r = rule(
            r#"
            name = "missing-valid-good"
            class_match.annotation = "RestController"
            kind = "parameter_annotation_requires_companion"
            trigger_annotation = "RequestBody"
            required_annotation_regex = "^(Valid|Validated)$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public void create(@Valid @RequestBody CreateXRequest req) {}
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty());
    }

    #[test]
    fn validated_companion_on_request_body_suppresses() {
        let r = rule(
            r#"
            name = "missing-validated-good"
            class_match.annotation = "RestController"
            kind = "parameter_annotation_requires_companion"
            trigger_annotation = "RequestBody"
            required_annotation_regex = "^(Valid|Validated)$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public void create(@RequestBody @Validated CreateXRequest req) {}
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty());
    }

    #[test]
    fn parameter_without_trigger_annotation_does_not_fire() {
        let r = rule(
            r#"
            name = "missing-valid-noop"
            class_match.annotation = "RestController"
            kind = "parameter_annotation_requires_companion"
            trigger_annotation = "RequestBody"
            required_annotation_regex = "^(Valid|Validated)$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public void noop(String s) {}
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty());
    }

    // ---------- W2.6: field_count_with_type_pattern ----------

    #[test]
    fn service_with_two_repositories_fires_once() {
        let r = rule(
            r#"
            name = "service-multi-repo"
            class_match.annotation = "Service"
            kind = "field_count_with_type_pattern"
            type_regex = ".*Repository$"
            max = 1
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            class OrderService {
                private final OrderRepository orderRepository;
                private final UserRepository userRepository;
            }
        "#;
        let v = check_java_file(&[r], "OrderService.java", "com.x.service", src.as_bytes());
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn service_with_one_repository_silent() {
        let r = rule(
            r#"
            name = "service-multi-repo"
            class_match.annotation = "Service"
            kind = "field_count_with_type_pattern"
            type_regex = ".*Repository$"
            max = 1
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            class OrderService {
                private final OrderRepository orderRepository;
            }
        "#;
        let v = check_java_file(&[r], "OrderService.java", "com.x.service", src.as_bytes());
        assert!(v.is_empty());
    }

    #[test]
    fn service_with_three_repositories_still_one_violation() {
        let r = rule(
            r#"
            name = "service-multi-repo"
            class_match.annotation = "Service"
            kind = "field_count_with_type_pattern"
            type_regex = ".*Repository$"
            max = 1
            "#,
        );
        let src = r#"
            package com.x.service;
            @Service
            class OrderService {
                private final OrderRepository a;
                private final UserRepository b;
                private final ItemRepository c;
            }
        "#;
        let v = check_java_file(&[r], "OrderService.java", "com.x.service", src.as_bytes());
        assert_eq!(
            v.len(),
            1,
            "one class-level violation regardless of field count"
        );
    }

    // ---------- W2.x non-DTO 3-stage gate ----------
    //
    // Rust's `regex` crate has no lookaround, so the plan's recommended
    // `type_regex = "(?i)^(?!.*(dto|request|response)).+$"` does not
    // compile. The working design folds the "type name is DTO-shaped"
    // signal into `type_allowlist` alongside stdlib value types. The
    // shared regex pair used in these tests:
    //
    //   type_regex             = ".+"   (match every return type)
    //   type_allowlist         = "(?i).*(dto|request|response).*|^(Void|Boolean|Integer|Long|Float|Double|String|UUID|BigDecimal|LocalDate|LocalDateTime|Instant)$"
    //   type_package_allowlist = "(?i)(^|\\.)(dto|dtos)(\\.|$)"

    const NON_DTO_RETURN_RULE: &str = r#"
        name = "controller-non-dto-return"
        class_match.annotation = "RestController"
        kind = "forbidden_return_type"
        visibility = "public"
        type_regex = ".+"
        type_allowlist = "(?i).*(dto|request|response).*|^(Void|Boolean|Integer|Long|Float|Double|String|UUID|BigDecimal|LocalDate|LocalDateTime|Instant)$"
        type_package_allowlist = "(?i)(^|\\.)(dto|dtos)(\\.|$)"
    "#;

    #[test]
    fn return_type_suppressor_drops_dto_package_finding() {
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            import com.example.app.dto.UserView;
            @RestController
            class C {
                public UserView get() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty(), "DTO package allowlist should suppress: {v:?}");
    }

    #[test]
    fn return_type_suppressor_fires_on_domain_imported_type() {
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            import com.example.app.domain.User;
            @RestController
            class C {
                public User get() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1, "domain-imported type should fire: {v:?}");
    }

    #[test]
    fn return_type_suppressor_drops_dto_named_type_in_sloppy_package() {
        let r = rule(NON_DTO_RETURN_RULE);
        // Domain package but name contains "Dto" — name allowlist catches
        // it (stage 2 in the plan, now folded into `type_allowlist`).
        let src = r#"
            package com.x.controller;
            import com.example.app.domain.UserDto;
            @RestController
            class C {
                public UserDto get() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(
            v.is_empty(),
            "DTO-named type is suppressed by name allowlist: {v:?}"
        );
    }

    #[test]
    fn return_type_suppressor_drops_stdlib_value_type() {
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public Long getId() { return 1L; }
                public String getName() { return ""; }
                public UUID getUuid() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty(), "stdlib types should be suppressed: {v:?}");
    }

    #[test]
    fn method_param_suppressor_drops_dto_package_finding() {
        let r = rule(
            r#"
            name = "service-non-dto-param"
            class_match.annotation = "Service"
            kind = "forbidden_method_param"
            visibility = "public"
            type_regex = ".+"
            type_allowlist = "(?i).*(dto|request|response).*|^(Void|Boolean|Integer|Long|Float|Double|String|UUID|BigDecimal|LocalDate|LocalDateTime|Instant)$"
            type_package_allowlist = "(?i)(^|\\.)(dto|dtos)(\\.|$)"
            "#,
        );
        let src = r#"
            package com.x.service;
            import com.example.app.dto.UserView;
            @Service
            class S {
                public void create(UserView u) {}
            }
        "#;
        let v = check_java_file(&[r], "S.java", "com.x.service", src.as_bytes());
        assert!(v.is_empty(), "DTO-package param should not fire: {v:?}");
    }

    // ---------- Severity routing ----------

    #[test]
    fn ast_rule_severity_is_carried_on_the_violation() {
        let r = rule(
            r#"
            name = "controller-no-repo"
            class_match.annotation = "RestController"
            kind = "forbidden_field_type"
            type_regex = ".*Repository$"
            severity = "CRITICAL"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            class C { private UserRepository repo; }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0].severity.as_deref(),
            Some("CRITICAL"),
            "the rule's severity must travel on the Violation: {v:?}"
        );
    }

    // ---------- W3.1: class_requires_annotation ----------

    #[test]
    fn missing_hilt_viewmodel_fires_when_inject_constructor_lacks_class_annotation() {
        let r = rule(
            r#"
            name = "MISSING_HILT_VIEWMODEL"
            class_match.extends = "ViewModel"
            kind = "class_requires_annotation"
            required_annotation = "HiltViewModel"
            trigger_constructor_annotation = "Inject"
            severity = "WARNING"
            "#,
        );
        let src = r#"
            package com.x.home;
            public class HomeViewModel extends ViewModel {
                @Inject HomeViewModel(UserRepository repo) {}
            }
        "#;
        let v = check_java_file(&[r], "HomeViewModel.java", "com.x.home", src.as_bytes());
        assert_eq!(v.len(), 1, "missing @HiltViewModel should fire: {v:?}");
        assert!(v[0].offending_import.contains("missing-@HiltViewModel"));
        assert_eq!(v[0].severity.as_deref(), Some("WARNING"));
    }

    #[test]
    fn missing_hilt_viewmodel_silent_when_class_carries_required_annotation() {
        let r = rule(
            r#"
            name = "MISSING_HILT_VIEWMODEL"
            class_match.extends = "ViewModel"
            kind = "class_requires_annotation"
            required_annotation = "HiltViewModel"
            trigger_constructor_annotation = "Inject"
            "#,
        );
        let src = r#"
            package com.x.home;
            @HiltViewModel
            public class HomeViewModel extends ViewModel {
                @Inject HomeViewModel(UserRepository repo) {}
            }
        "#;
        let v = check_java_file(&[r], "HomeViewModel.java", "com.x.home", src.as_bytes());
        assert!(v.is_empty(), "@HiltViewModel present → no fire: {v:?}");
    }

    #[test]
    fn missing_hilt_viewmodel_silent_without_trigger_constructor() {
        let r = rule(
            r#"
            name = "MISSING_HILT_VIEWMODEL"
            class_match.extends = "ViewModel"
            kind = "class_requires_annotation"
            required_annotation = "HiltViewModel"
            trigger_constructor_annotation = "Inject"
            "#,
        );
        // No @Inject constructor — the trigger doesn't fire, so the rule
        // stays silent regardless of @HiltViewModel.
        let src = r#"
            package com.x.home;
            public class HomeViewModel extends ViewModel {
                HomeViewModel() {}
            }
        "#;
        let v = check_java_file(&[r], "HomeViewModel.java", "com.x.home", src.as_bytes());
        assert!(v.is_empty(), "no @Inject trigger → no fire: {v:?}");
    }

    #[test]
    fn missing_hilt_viewmodel_silent_when_class_shape_doesnt_match() {
        let r = rule(
            r#"
            name = "MISSING_HILT_VIEWMODEL"
            class_match.extends = "ViewModel"
            kind = "class_requires_annotation"
            required_annotation = "HiltViewModel"
            trigger_constructor_annotation = "Inject"
            "#,
        );
        // Not a ViewModel — class_match gates the rule out before the
        // trigger is ever evaluated.
        let src = r#"
            package com.x.home;
            public class HomeService {
                @Inject HomeService() {}
            }
        "#;
        let v = check_java_file(&[r], "HomeService.java", "com.x.home", src.as_bytes());
        assert!(v.is_empty(), "non-ViewModel must not fire: {v:?}");
    }

    // ---------- generic-wrapper unwrapping ----------

    #[test]
    fn return_type_strips_list_wrapper_to_inner_dto_by_name() {
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public List<UserDto> get() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(
            v.is_empty(),
            "List<UserDto> must unwrap and suppress by name: {v:?}"
        );
    }

    #[test]
    fn return_type_strips_optional_wrapper() {
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public Optional<UserResponse> get() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(v.is_empty(), "Optional<UserResponse> must not fire: {v:?}");
    }

    #[test]
    fn return_type_strips_nested_wrappers() {
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public ResponseEntity<List<UserDto>> get() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(
            v.is_empty(),
            "ResponseEntity<List<UserDto>> must unwrap recursively: {v:?}"
        );
    }

    #[test]
    fn return_type_unwrap_still_fires_on_non_dto_inner() {
        // The unwrap is not a blanket pass — `List<User>` from a
        // domain package still exposes a non-DTO inner type.
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            import com.example.app.domain.User;
            @RestController
            class C {
                public List<User> list() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(
            v.len(),
            1,
            "List<User> with domain import must still fire: {v:?}"
        );
    }

    #[test]
    fn return_type_unwrap_is_off_for_non_wrapper_outer() {
        // `UserDto<Foo>` is not a recognised wrapper, so the outer
        // (`UserDto`) is tested directly and matched by the name
        // allowlist.
        let r = rule(NON_DTO_RETURN_RULE);
        let src = r#"
            package com.x.controller;
            @RestController
            class C {
                public UserDto<Foo> get() { return null; }
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert!(
            v.is_empty(),
            "outer-DTO-named generic must still be suppressed by name: {v:?}"
        );
    }

    #[test]
    fn forbidden_field_type_unwraps_list_for_repository_match() {
        // The repository-field rule benefits too: a `List<UserRepository>`
        // field on a controller is just as bad as a bare `UserRepository`.
        let r = rule(
            r#"
            name = "controller-no-repo-field"
            class_match.annotation = "RestController"
            kind = "forbidden_field_type"
            type_regex = ".*Repository$"
            "#,
        );
        let src = r#"
            package com.x.controller;
            @RestController
            public class C {
                private List<UserRepository> repos;
            }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1, "List<UserRepository> field must fire: {v:?}");
    }

    // ---------- Wave A: deprecated flag ----------

    #[test]
    fn deprecated_flag_round_trips_through_toml_and_rule_still_loads() {
        // The deprecated flag must not block the rule from compiling
        // and firing. Wave A's job is only to surface the warning; the
        // delete migration is Wave B.
        let raw: RawAstRule = toml::from_str(
            r#"
            name = "controller-no-repo-field-legacy"
            class_match.annotation = "RestController"
            kind = "forbidden_field_type"
            type_regex = ".*Repository$"
            deprecated = true
            replaced_by = "CONTROLLER_USES_REPOSITORY"
            "#,
        )
        .expect("deprecated/replaced_by must deserialise");
        assert!(raw.deprecated);
        assert_eq!(
            raw.replaced_by.as_deref(),
            Some("CONTROLLER_USES_REPOSITORY")
        );
        let r = AstRule::from_raw(raw).expect("deprecated rule must still compile");
        let src = r#"
            package com.x.controller;
            @RestController
            public class C { private UserRepository repo; }
        "#;
        let v = check_java_file(&[r], "C.java", "com.x.controller", src.as_bytes());
        assert_eq!(v.len(), 1, "deprecated rule must still fire");
    }

    #[test]
    fn deprecated_defaults_to_false_when_absent() {
        let raw: RawAstRule = toml::from_str(
            r#"
            name = "CONTROLLER_USES_REPOSITORY"
            class_match.annotation = "RestController"
            kind = "forbidden_field_type"
            type_regex = ".*Repository$"
            "#,
        )
        .expect("rule with no deprecated key must parse");
        assert!(!raw.deprecated);
        assert!(raw.replaced_by.is_none());
    }
}
