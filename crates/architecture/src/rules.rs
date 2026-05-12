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
}

fn default_severity() -> String {
    "WARNING".to_string()
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
        Ok(Self {
            layers,
            forbidden,
            ast_rules,
            severity: raw.severity,
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
    }
}
