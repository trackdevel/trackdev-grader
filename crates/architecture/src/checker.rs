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
use crate::scanner::ScannedFile;

/// Package roots we never classify as belonging to a student layer.
///
/// Without this guard, a permissive layer pattern like `**/persistence/**`
/// matches `jakarta.persistence` (the JPA API surface — not the student's
/// own `infrastructure` package), producing false-positive
/// `layer_dependency` violations for a domain entity that legitimately
/// imports `jakarta.persistence.Entity`. `[[forbidden]]` rules are
/// authoritative for "framework imports forbidden in this package", so
/// excluding these prefixes from the *layer* classifier doesn't hide any
/// real policy — it just stops the layer system from misreading framework
/// imports as another internal layer.
const EXTERNAL_PACKAGE_PREFIXES: &[&str] = &[
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
];

fn is_external_package(pkg: &str) -> bool {
    EXTERNAL_PACKAGE_PREFIXES
        .iter()
        .any(|prefix| pkg.starts_with(prefix))
}

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
    /// Per-rule severity override. AST rules set this from their TOML
    /// `severity` field; the legacy layered + forbidden-import paths
    /// leave it `None` and fall back to `ArchitectureRules::severity`.
    pub severity: Option<String>,
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

pub fn check_file(rules: &ArchitectureRules, file: &ScannedFile) -> Vec<Violation> {
    let mut out = Vec::new();
    let own_layer = rules.layer_of(&file.package);

    for imp in &file.imports {
        let raw_import = &imp.text;
        let imp_pkg = import_to_package(raw_import);
        let start = Some(imp.start_line);
        let end = Some(imp.end_line);
        if let Some(own) = own_layer {
            // Framework / JDK imports never belong to a *student* layer, even
            // when their package happens to match a layer glob (e.g.
            // `jakarta.persistence` matching `**/persistence/**`). Skip them
            // here; `[[forbidden]]` rules below remain authoritative.
            if is_external_package(&imp_pkg) {
                // fall through to forbidden-rule evaluation
            } else if let Some(target_layer) = rules.layer_of(&imp_pkg) {
                if target_layer != own {
                    let allowed = rules
                        .layers
                        .iter()
                        .find(|l| l.name == own)
                        .map(|l| l.may_depend_on.iter().any(|n| n == target_layer))
                        .unwrap_or(false);
                    if !allowed {
                        out.push(Violation {
                            file_path: file.rel_path.clone(),
                            rule_name: format!("{own}->!{target_layer}"),
                            kind: ViolationKind::LayerDependency,
                            offending_import: raw_import.clone(),
                            start_line: start,
                            end_line: end,
                            severity: None,
                        });
                    }
                }
            }
        }

        for f in &rules.forbidden {
            if f.from.matches(&file.package) && f.must_not_match.iter().any(|p| p.matches(&imp_pkg))
            {
                out.push(Violation {
                    file_path: file.rel_path.clone(),
                    rule_name: f.label.clone(),
                    kind: ViolationKind::ForbiddenImport,
                    offending_import: raw_import.clone(),
                    start_line: start,
                    end_line: end,
                    severity: None,
                });
            }
        }
    }
    out
}

pub fn check_repo<'a>(
    rules: &'a ArchitectureRules,
    files: &'a [ScannedFile],
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

    fn scan(rel: &str, src: &str) -> ScannedFile {
        ScannedFile::from_inline(rel, src.as_bytes()).expect("inline source must declare a package")
    }

    #[test]
    fn controller_importing_repository_is_a_violation() {
        let r = rules();
        let f = scan(
            "src/main/java/UserController.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class UserController {}\n",
        );
        let vs = check_file(&r, &f);
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].kind, ViolationKind::LayerDependency);
        assert_eq!(vs[0].offending_import, "com.x.repository.UserRepository");
        assert_eq!(vs[0].rule_name, "presentation->!infrastructure");
    }

    #[test]
    fn application_importing_domain_is_allowed() {
        let r = rules();
        let f = scan(
            "Svc.java",
            "package com.x.application.svc;\n\
             import com.x.domain.user.User;\n\
             public class Svc {}\n",
        );
        assert!(check_file(&r, &f).is_empty());
    }

    #[test]
    fn forbidden_import_fires_independently_of_layer() {
        let r = rules();
        let f = scan(
            "Domain.java",
            "package com.x.domain.user;\n\
             import org.springframework.web.bind.annotation.RestController;\n\
             public class Domain {}\n",
        );
        let vs = check_file(&r, &f);
        assert!(vs
            .iter()
            .any(|v| matches!(v.kind, ViolationKind::ForbiddenImport)));
    }

    #[test]
    fn java_stdlib_import_is_allowed() {
        let r = rules();
        let f = scan(
            "Domain.java",
            "package com.x.domain.user;\n\
             import java.util.List;\n\
             public class Domain {}\n",
        );
        assert!(check_file(&r, &f).is_empty());
    }

    #[test]
    fn jakarta_persistence_does_not_classify_as_infrastructure_layer() {
        // Regression: the production config matches `**/persistence/**` for
        // the `infrastructure` layer. Without the external-prefix guard,
        // `jakarta.persistence.Entity` (a framework import that *every* JPA
        // entity needs) was misread as crossing into the infrastructure
        // layer and produced a bogus `domain->!infrastructure` violation.
        const RULES: &str = r#"
[[layers]]
name = "domain"
packages = ["**/domain/**", "**/model/**"]
may_depend_on = []

[[layers]]
name = "infrastructure"
packages = ["**/infrastructure/**", "**/repository/**", "**/persistence/**"]
may_depend_on = ["domain"]
"#;
        let r = ArchitectureRules::from_toml_str(RULES).unwrap();
        let f = scan(
            "src/main/java/com/x/model/Comment.java",
            "package com.x.model;\n\
             \n\
             import jakarta.persistence.Entity;\n\
             import jakarta.persistence.*;\n\
             import javax.persistence.Id;\n\
             import org.springframework.data.jpa.repository.Repository;\n\
             @Entity public class Comment {}\n",
        );
        let vs = check_file(&r, &f);
        assert!(
            vs.iter()
                .all(|v| !matches!(v.kind, ViolationKind::LayerDependency)),
            "framework imports must not trigger layer_dependency violations: {vs:?}"
        );
    }

    #[test]
    fn external_prefix_guard_does_not_mask_internal_layer_violation() {
        // The guard must be a no-op for genuine cross-layer dependencies
        // between student packages.
        let r = rules();
        let f = scan(
            "Bad.java",
            "package com.x.controller;\n\
             import com.x.repository.UserRepository;\n\
             public class Bad {}\n",
        );
        let vs = check_file(&r, &f);
        assert_eq!(vs.len(), 1);
        assert!(matches!(vs[0].kind, ViolationKind::LayerDependency));
    }

    #[test]
    fn external_prefix_guard_does_not_mask_explicit_forbidden_rule() {
        // `[[forbidden]] domain-no-jpa` MUST still fire on jakarta.persistence —
        // forbidden rules are authoritative for explicit policy.
        const RULES_WITH_JPA_BAN: &str = r#"
[[layers]]
name = "domain"
packages = ["**/domain/**"]
may_depend_on = []

[[forbidden]]
name = "domain-no-jpa"
from = "**/domain/**"
must_not_match = ["jakarta/persistence/**", "javax/persistence/**"]
"#;
        let r = ArchitectureRules::from_toml_str(RULES_WITH_JPA_BAN).unwrap();
        let f = scan(
            "Domain.java",
            "package com.x.domain.user;\n\
             import jakarta.persistence.Entity;\n\
             public class Domain {}\n",
        );
        let vs = check_file(&r, &f);
        assert_eq!(vs.len(), 1);
        assert!(matches!(vs[0].kind, ViolationKind::ForbiddenImport));
        assert_eq!(vs[0].rule_name, "domain-no-jpa");
    }
}
