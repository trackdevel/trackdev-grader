//! Apply rules to scanned facts (T-P2.2).
//!
//! For every Java file, derive the layer that owns it (if any) and check
//! every import against:
//! - the layered allow-list (`may_depend_on`),
//! - every applicable `forbidden` block.
//!
//! Imports that don't land in any known layer (e.g. `java.util.List`) are
//! silently allowed — the rules describe what *internal* layers may
//! depend on each other, not what stdlib calls are permitted.

use crate::rules::ArchitectureRules;
use crate::scanner::JavaFileFacts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub file_path: String,
    pub rule_name: String,
    pub kind: ViolationKind,
    pub offending_import: String,
    /// 1-based line range covering the offending construct. The legacy
    /// package-glob path fills these with the import-statement's line; the
    /// AST path fills them with the offending field/method/parameter span;
    /// the LLM path (T-P3.3) fills them from the model response.
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViolationKind {
    LayerDependency,
    ForbiddenImport,
    /// AST-rule kind label (e.g. `ast_forbidden_field_type`). Stored as a
    /// `String` so the variant doesn't have to enumerate every kind known to
    /// `ast_rules.rs` — keeps additive rule-kind growth a one-file change.
    AstRule(String),
}

impl ViolationKind {
    pub fn as_str(&self) -> &str {
        match self {
            ViolationKind::LayerDependency => "layer_dependency",
            ViolationKind::ForbiddenImport => "forbidden_import",
            ViolationKind::AstRule(label) => label.as_str(),
        }
    }
}

/// Turn an import like `com.example.user.UserService` into the package
/// component (`com.example.user`). If there's no `.` we assume the whole
/// thing is already a package name.
fn import_to_package(import: &str) -> String {
    // Drop a trailing `.*` first.
    let without_star = import.strip_suffix(".*").unwrap_or(import);
    // Heuristic: a bare name starting with uppercase is a class; the
    // leading dotted prefix is the package. `import com.x.Y` →
    // package = `com.x`. `import com.x.y.z.Foo` → `com.x.y.z`.
    if let Some((head, last)) = without_star.rsplit_once('.') {
        let starts_upper = last
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false);
        if starts_upper {
            head.to_string()
        } else {
            without_star.to_string()
        }
    } else {
        without_star.to_string()
    }
}

pub fn check_file(rules: &ArchitectureRules, facts: &JavaFileFacts) -> Vec<Violation> {
    let mut out = Vec::new();
    let own_layer = rules.layer_of(&facts.package);

    for imp in &facts.imports {
        let raw_import = &imp.text;
        let imp_pkg = import_to_package(raw_import);
        let line = imp.line;
        if let Some(own) = own_layer {
            if let Some(target_layer) = rules.layer_of(&imp_pkg) {
                if target_layer != own {
                    let allowed = rules
                        .layers
                        .iter()
                        .find(|l| l.name == own)
                        .map(|l| l.may_depend_on.iter().any(|n| n == target_layer))
                        .unwrap_or(false);
                    if !allowed {
                        out.push(Violation {
                            file_path: facts.rel_path.clone(),
                            rule_name: format!("{own}->!{target_layer}"),
                            kind: ViolationKind::LayerDependency,
                            offending_import: raw_import.clone(),
                            start_line: line,
                            end_line: line,
                        });
                    }
                }
            }
        }

        for f in &rules.forbidden {
            if f.from.matches(&facts.package)
                && f.must_not_match.iter().any(|p| p.matches(&imp_pkg))
            {
                out.push(Violation {
                    file_path: facts.rel_path.clone(),
                    rule_name: f.label.clone(),
                    kind: ViolationKind::ForbiddenImport,
                    offending_import: raw_import.clone(),
                    start_line: line,
                    end_line: line,
                });
            }
        }
    }
    out
}

pub fn check_repo<'a>(
    rules: &'a ArchitectureRules,
    files: &'a [JavaFileFacts],
) -> impl Iterator<Item = Violation> + 'a {
    files.iter().flat_map(move |f| check_file(rules, f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ArchitectureRules;

    const RULES: &str = r#"
[[layers]]
name = "domain"
packages = ["**/domain/**"]
may_depend_on = []

[[layers]]
name = "application"
packages = ["**/application/**"]
may_depend_on = ["domain"]

[[layers]]
name = "infrastructure"
packages = ["**/repository/**"]
may_depend_on = ["domain", "application"]

[[layers]]
name = "presentation"
packages = ["**/controller/**"]
may_depend_on = ["application", "domain"]

[[forbidden]]
name = "domain-no-spring-web"
from = "**/domain/**"
must_not_match = ["org/springframework/web/**"]
"#;

    fn rules() -> ArchitectureRules {
        ArchitectureRules::from_toml_str(RULES).unwrap()
    }

    #[test]
    fn import_to_package_strips_class_suffix() {
        assert_eq!(import_to_package("com.x.UserService"), "com.x");
        assert_eq!(import_to_package("com.x.y.z.Foo"), "com.x.y.z");
        assert_eq!(
            import_to_package("com.x.subpkg.something"),
            "com.x.subpkg.something"
        );
        assert_eq!(import_to_package("com.x.*"), "com.x");
    }

    #[test]
    fn controller_importing_repository_is_a_violation() {
        let r = rules();
        let f = JavaFileFacts {
            rel_path: "src/main/java/UserController.java".into(),
            package: "com.x.controller".into(),
            imports: vec![crate::scanner::ImportLine {
                text: "com.x.repository.UserRepository".into(),
                line: Some(2),
            }],
        };
        let vs = check_file(&r, &f);
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].kind, ViolationKind::LayerDependency);
        assert_eq!(vs[0].offending_import, "com.x.repository.UserRepository");
        assert_eq!(vs[0].rule_name, "presentation->!infrastructure");
    }

    #[test]
    fn application_importing_domain_is_allowed() {
        let r = rules();
        let f = JavaFileFacts {
            rel_path: "Svc.java".into(),
            package: "com.x.application.svc".into(),
            imports: vec![crate::scanner::ImportLine {
                text: "com.x.domain.user.User".into(),
                line: Some(2),
            }],
        };
        assert!(check_file(&r, &f).is_empty());
    }

    #[test]
    fn forbidden_import_fires_independently_of_layer() {
        let r = rules();
        let f = JavaFileFacts {
            rel_path: "Domain.java".into(),
            package: "com.x.domain.user".into(),
            imports: vec![crate::scanner::ImportLine {
                text: "org.springframework.web.bind.annotation.RestController".into(),
                line: Some(2),
            }],
        };
        let vs = check_file(&r, &f);
        assert!(vs
            .iter()
            .any(|v| matches!(v.kind, ViolationKind::ForbiddenImport)));
    }

    #[test]
    fn java_stdlib_import_is_allowed() {
        let r = rules();
        let f = JavaFileFacts {
            rel_path: "Domain.java".into(),
            package: "com.x.domain.user".into(),
            imports: vec![crate::scanner::ImportLine {
                text: "java.util.List".into(),
                line: Some(2),
            }],
        };
        assert!(check_file(&r, &f).is_empty());
    }
}
