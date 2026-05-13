//! Architecture rules loader (T-P2.2).
//!
//! Two rule kinds:
//! - **Layered rules** — each named layer matches a set of package globs
//!   and declares which other layers it `may_depend_on`. An import from a
//!   file in layer A targeting a package in layer B (where B is not in
//!   A's allow-list) is a `layer_dependency` violation.
//! - **Forbidden imports** — a package glob lists imports that must not
//!   appear in matching files. Useful for "domain layer must not see
//!   Spring web annotations" and similar.
//!
//! TOML format (chosen over YAML to avoid a new transitive dep — same
//! intent, no `serde_yaml`):
//!
//! ```toml
//! [[layers]]
//! name = "domain"
//! packages = ["**/domain/**"]
//! may_depend_on = []
//!
//! [[layers]]
//! name = "application"
//! packages = ["**/application/**", "**/service/**"]
//! may_depend_on = ["domain"]
//!
//! [[forbidden]]
//! from = "**/domain/**"
//! must_not_match = ["org.springframework.web.*", "org.springframework.data.*"]
//! ```

use std::path::Path;

use serde::Deserialize;

use crate::ast_rules::{AstRule, RawAstRule};
use crate::glob::PackagePattern;

#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub packages: Vec<PackagePattern>,
    pub may_depend_on: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Forbidden {
    /// Files whose package matches this pattern have the `must_not_match`
    /// list applied.
    pub from: PackagePattern,
    pub must_not_match: Vec<PackagePattern>,
    /// Human-readable label used as `rule_name` in the violations table.
    /// Falls back to `format!("forbidden-{i}")` when not set in the file.
    pub label: String,
}

#[derive(Debug, Clone, Default)]
pub struct ArchitectureRules {
    pub layers: Vec<Layer>,
    pub forbidden: Vec<Forbidden>,
    /// AST-driven rules (T-P3.1). Loaded from `[[ast_rule]]` blocks; empty
    /// when the rules file predates the AST extension.
    pub ast_rules: Vec<AstRule>,
    pub severity: String,
    /// Package roots that the LAYER classifier in `checker.rs` will
    /// never assign to a STUDENT layer (framework / JDK packages stay
    /// off the layered map; cross-layer policy for them belongs in
    /// `[[forbidden]]` blocks). Defaults to a built-in list; TOML can
    /// append more via `[external_packages] extras = […]`. Replacement
    /// is intentionally not supported — a misconfiguration that emptied
    /// the list would silently re-enable the JPA-entity false-positive
    /// the guard was introduced to fix.
    pub external_package_prefixes: Vec<String>,
    /// Generic wrapper type names whose inner type argument is the
    /// semantically-interesting type for the AST suppressors (e.g.
    /// `List<UserDto>` is a DTO at the API boundary). Defaults +
    /// TOML extras via `[generic_wrappers] extras = […]`.
    pub generic_wrappers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawRules {
    #[serde(default)]
    layers: Vec<RawLayer>,
    #[serde(default)]
    forbidden: Vec<RawForbidden>,
    #[serde(default, rename = "ast_rule")]
    ast_rule: Vec<RawAstRule>,
    /// Severity used for every emitted violation. Default `WARNING`.
    #[serde(default = "default_severity")]
    severity: String,
    #[serde(default)]
    external_packages: RawExternalPackages,
    #[serde(default)]
    generic_wrappers: RawGenericWrappers,
}

fn default_severity() -> String {
    "WARNING".to_string()
}

/// `[external_packages]` block. `extras` is appended to
/// [`default_external_package_prefixes`].
#[derive(Debug, Default, Deserialize)]
struct RawExternalPackages {
    #[serde(default)]
    extras: Vec<String>,
}

/// `[generic_wrappers]` block. `extras` is appended to
/// [`default_generic_wrappers`].
#[derive(Debug, Default, Deserialize)]
struct RawGenericWrappers {
    #[serde(default)]
    extras: Vec<String>,
}

/// Built-in package roots treated as "framework / JDK, never a STUDENT
/// layer". Returns a fresh `Vec<String>` each call; the loader extends
/// it with any TOML `[external_packages] extras`.
pub fn default_external_package_prefixes() -> Vec<String> {
    [
        "java.",
        "javax.",
        "jakarta.",
        "kotlin.",
        "kotlinx.",
        "scala.",
        "groovy.",
        "android.",
        "androidx.",
        "com.android.",
        "com.google.",
        "com.fasterxml.",
        "com.squareup.",
        "org.springframework.",
        "org.hibernate.",
        "org.apache.",
        "org.junit.",
        "org.mockito.",
        "org.slf4j.",
        "org.aspectj.",
        "org.jetbrains.",
        "io.micrometer.",
        "io.swagger.",
        "lombok.",
        "retrofit2.",
        "okhttp3.",
        "dagger.",
        "hilt.",
        "ch.qos.",
        "reactor.",
        "rx.",
        "io.reactivex.",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// Built-in generic-wrapper outer-type names. The loader extends this
/// with any TOML `[generic_wrappers] extras`. `Map` is intentionally
/// absent — it has two type parameters and unwrapping is ambiguous;
/// keep it in the stdlib allowlist by its outer name.
pub fn default_generic_wrappers() -> Vec<String> {
    [
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
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[derive(Debug, Deserialize)]
struct RawLayer {
    name: String,
    packages: Vec<String>,
    #[serde(default)]
    may_depend_on: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawForbidden {
    from: String,
    must_not_match: Vec<String>,
    #[serde(default)]
    name: Option<String>,
}

impl ArchitectureRules {
    pub fn from_toml_str(text: &str) -> anyhow::Result<Self> {
        let raw: RawRules = toml::from_str(text)?;
        let layers = raw
            .layers
            .into_iter()
            .map(|l| Layer {
                name: l.name,
                packages: l.packages.iter().map(|p| PackagePattern::new(p)).collect(),
                may_depend_on: l.may_depend_on,
            })
            .collect();
        let forbidden = raw
            .forbidden
            .into_iter()
            .enumerate()
            .map(|(i, f)| Forbidden {
                from: PackagePattern::new(&f.from),
                must_not_match: f
                    .must_not_match
                    .iter()
                    .map(|p| PackagePattern::new(p))
                    .collect(),
                label: f.name.unwrap_or_else(|| format!("forbidden-{i}")),
            })
            .collect();
        let mut ast_rules = Vec::with_capacity(raw.ast_rule.len());
        for raw_ast in raw.ast_rule {
            ast_rules.push(AstRule::from_raw(raw_ast)?);
        }
        let mut external_package_prefixes = default_external_package_prefixes();
        external_package_prefixes.extend(raw.external_packages.extras);
        let mut generic_wrappers = default_generic_wrappers();
        generic_wrappers.extend(raw.generic_wrappers.extras);
        Ok(Self {
            layers,
            forbidden,
            ast_rules,
            severity: raw.severity,
            external_package_prefixes,
            generic_wrappers,
        })
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_toml_str(&text)
    }

    /// Returns the layer name that owns the given package (first match
    /// wins). `None` when no layer claims it — such files are excluded
    /// from layered-rule checking but still subject to `forbidden`.
    pub fn layer_of(&self, package: &str) -> Option<&str> {
        self.layers
            .iter()
            .find(|l| l.packages.iter().any(|p| p.matches(package)))
            .map(|l| l.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
severity = "WARNING"

[[layers]]
name = "domain"
packages = ["**/domain/**"]
may_depend_on = []

[[layers]]
name = "application"
packages = ["**/application/**", "**/service/**"]
may_depend_on = ["domain"]

[[layers]]
name = "infrastructure"
packages = ["**/infrastructure/**", "**/repository/**"]
may_depend_on = ["domain", "application"]

[[forbidden]]
name = "domain-no-spring-web"
from = "**/domain/**"
must_not_match = ["org/springframework/web/**", "org/springframework/data/**"]
"#;

    #[test]
    fn parses_layers_and_forbidden_blocks() {
        let r = ArchitectureRules::from_toml_str(SAMPLE).unwrap();
        assert_eq!(r.layers.len(), 3);
        assert_eq!(r.layers[0].name, "domain");
        assert_eq!(r.forbidden.len(), 1);
        assert_eq!(r.forbidden[0].label, "domain-no-spring-web");
        assert_eq!(r.severity, "WARNING");
    }

    #[test]
    fn layer_of_finds_first_matching_layer() {
        let r = ArchitectureRules::from_toml_str(SAMPLE).unwrap();
        assert_eq!(r.layer_of("com.x.domain.user"), Some("domain"));
        assert_eq!(r.layer_of("com.x.application.svc"), Some("application"));
        assert_eq!(r.layer_of("com.x.repository.foo"), Some("infrastructure"));
        assert_eq!(r.layer_of("com.x.config"), None);
    }

    #[test]
    fn missing_severity_defaults_to_warning() {
        let r =
            ArchitectureRules::from_toml_str("[[layers]]\nname = \"a\"\npackages = []\n").unwrap();
        assert_eq!(r.severity, "WARNING");
    }

    /// Smoke test: the production `config/architecture.toml` must parse
    /// cleanly. Anything that breaks here would silently disable the
    /// architecture stage in CI, since the loader bails on the first
    /// regex / unknown-kind error.
    #[test]
    fn production_config_loads_cleanly() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/architecture.toml");
        if !path.exists() {
            eprintln!("skipping: {} not present", path.display());
            return;
        }
        let r = ArchitectureRules::load(&path).expect("architecture.toml must parse");
        assert!(
            !r.ast_rules.is_empty(),
            "expected at least one [[ast_rule]] in production config"
        );

        // Every Spring v8 rubric rule ID should be wired in at least once.
        let names: std::collections::HashSet<&str> =
            r.ast_rules.iter().map(|x| x.name.as_str()).collect();
        for rule_id in &[
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
        ] {
            assert!(
                names.contains(rule_id),
                "expected Spring v8 rule '{rule_id}' in architecture.toml"
            );
        }
        // Every Android v1 rubric rule ID should also be wired in.
        for rule_id in &[
            "VIEWMODEL_IMPORTS_ANDROID_UI",
            "VIEWMODEL_HOLDS_CONTEXT",
            "FRAGMENT_BYPASSES_VIEWMODEL",
            "REPOSITORY_DEPENDS_ON_VIEW_LAYER",
            "ASYNCTASK_USAGE",
            "STATIC_VIEW_OR_CONTEXT_FIELD",
            "FRAGMENT_BINDING_NOT_NULLED",
            "LIVEDATA_OBSERVED_WITH_FRAGMENT_THIS",
            "VIEWMODEL_BYPASSES_REPOSITORY",
            "FINDVIEWBYID_USAGE",
            "NAVIGATION_VIA_FRAGMENT_TRANSACTION",
            "FRAGMENT_CASTS_PARENT_ACTIVITY",
            "RAW_THREAD_FOR_BACKGROUND_WORK",
            "MUTABLELIVEDATA_EXPOSED_PUBLICLY",
            "FAT_FRAGMENT_OR_ACTIVITY_METHOD",
            "MISSING_HILT_VIEWMODEL",
        ] {
            assert!(
                names.contains(rule_id),
                "expected Android v1 rule '{rule_id}' in architecture.toml"
            );
        }
    }

    #[test]
    fn external_package_prefixes_default_when_section_absent() {
        let r = ArchitectureRules::from_toml_str("severity = \"WARNING\"\n").unwrap();
        let defaults = default_external_package_prefixes();
        assert_eq!(
            r.external_package_prefixes, defaults,
            "missing [external_packages] block must yield the built-in defaults verbatim"
        );
    }

    #[test]
    fn external_package_prefixes_toml_extras_are_appended_to_defaults() {
        const TOML: &str = r#"
severity = "WARNING"

[external_packages]
extras = ["io.netty.", "com.example.legacy."]
"#;
        let r = ArchitectureRules::from_toml_str(TOML).unwrap();
        let defaults = default_external_package_prefixes();
        assert!(
            r.external_package_prefixes.len() == defaults.len() + 2,
            "extras must append, not replace; got: {:?}",
            r.external_package_prefixes
        );
        // Defaults stay in place …
        assert!(r.external_package_prefixes.iter().any(|p| p == "jakarta."));
        // … and the extras land at the end.
        assert!(r.external_package_prefixes.iter().any(|p| p == "io.netty."));
        assert!(r
            .external_package_prefixes
            .iter()
            .any(|p| p == "com.example.legacy."));
    }

    #[test]
    fn generic_wrappers_default_when_section_absent() {
        let r = ArchitectureRules::from_toml_str("severity = \"WARNING\"\n").unwrap();
        let defaults = default_generic_wrappers();
        assert_eq!(r.generic_wrappers, defaults);
    }

    #[test]
    fn generic_wrappers_toml_extras_are_appended_to_defaults() {
        const TOML: &str = r#"
severity = "WARNING"

[generic_wrappers]
extras = ["Either", "Result"]
"#;
        let r = ArchitectureRules::from_toml_str(TOML).unwrap();
        let defaults = default_generic_wrappers();
        assert_eq!(r.generic_wrappers.len(), defaults.len() + 2);
        // Defaults stay in place …
        assert!(r.generic_wrappers.iter().any(|w| w == "Optional"));
        assert!(r.generic_wrappers.iter().any(|w| w == "ResponseEntity"));
        // … extras land at the end.
        assert!(r.generic_wrappers.iter().any(|w| w == "Either"));
        assert!(r.generic_wrappers.iter().any(|w| w == "Result"));
    }
}
