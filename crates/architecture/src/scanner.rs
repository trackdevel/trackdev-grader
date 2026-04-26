//! Java file → (package, imports) extraction (T-P2.2).
//!
//! tree-sitter-java would work but is overkill for the two declarations
//! we need: `package com.foo.bar;` and `import com.foo.Bar;` (or
//! `import static`, or `import com.foo.*`). Both follow strict syntax
//! and appear at the top of the file before any class body. A line-based
//! parser handles every case the course's Java code produces.

use std::path::Path;

use walkdir::WalkDir;

const SKIP_DIRS: &[&str] = &[
    "target",
    "build",
    ".gradle",
    ".idea",
    ".git",
    "node_modules",
    "bin",
    "out",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaFileFacts {
    /// Path relative to the repo root, in the original separator form.
    pub rel_path: String,
    pub package: String,
    pub imports: Vec<String>,
}

/// Walk a repo and return one row per `.java` file. Files in build,
/// dependency, or VCS directories are skipped via [`SKIP_DIRS`]. Files
/// that fail to read or have no `package` declaration are dropped (they
/// belong to the default package which the layered rules can't claim).
pub fn scan_repo(repo_path: &Path) -> Vec<JavaFileFacts> {
    let mut out = Vec::new();
    for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("java") {
            continue;
        }
        let rel = match path.strip_prefix(repo_path) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some((package, imports)) = parse_java(&text) else {
            continue;
        };
        out.push(JavaFileFacts {
            rel_path: rel.to_string_lossy().into_owned(),
            package,
            imports,
        });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

/// Extract `(package, imports)` from a Java source string. `None` when
/// no `package` declaration is present (default-package files have no
/// home in the layered model).
pub fn parse_java(text: &str) -> Option<(String, Vec<String>)> {
    let mut package: Option<String> = None;
    let mut imports: Vec<String> = Vec::new();
    let mut in_block_comment = false;
    for raw in text.lines() {
        let line = strip_comments(raw, &mut in_block_comment);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Stop at the class/interface/record body — both declarations
        // we need always appear before that.
        if trimmed.starts_with("class ")
            || trimmed.starts_with("public class ")
            || trimmed.starts_with("interface ")
            || trimmed.starts_with("public interface ")
            || trimmed.starts_with("@")
        {
            // The annotation line might be a class-level annotation; if
            // so, we've already passed `package` + `import`. Bail.
            if package.is_some() {
                break;
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("package ") {
            package = Some(rest.trim_end_matches(';').trim().to_string());
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("import ") {
            let cleaned = rest.trim_end_matches(';').trim();
            // `import static foo.Bar.baz;` → keep `foo.Bar.baz`
            let cleaned = cleaned.strip_prefix("static ").unwrap_or(cleaned).trim();
            if !cleaned.is_empty() {
                imports.push(cleaned.to_string());
            }
        }
    }
    package.map(|p| (p, imports))
}

fn strip_comments(line: &str, in_block: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if *in_block {
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                *in_block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            break; // rest of line is a line comment
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            *in_block = true;
            i += 2;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_package_and_imports() {
        let src = "package com.example.app;\n\
                   import java.util.List;\n\
                   import static org.junit.Assert.*;\n\
                   public class Foo {}\n";
        let (pkg, imports) = parse_java(src).unwrap();
        assert_eq!(pkg, "com.example.app");
        assert_eq!(imports, vec!["java.util.List", "org.junit.Assert.*"]);
    }

    #[test]
    fn ignores_block_comment_imports() {
        let src = "package com.x;\n\
                   /* import java.io.File; */\n\
                   import java.util.List;\n\
                   public class A {}";
        let (pkg, imports) = parse_java(src).unwrap();
        assert_eq!(pkg, "com.x");
        assert_eq!(imports, vec!["java.util.List"]);
    }

    #[test]
    fn no_package_returns_none() {
        let src = "import java.util.List;\npublic class A {}";
        assert!(parse_java(src).is_none());
    }

    #[test]
    fn class_level_annotation_stops_scan() {
        let src = "package com.x;\n\
                   import java.util.List;\n\
                   @Service\n\
                   public class A {\n\
                     // import nothing.Sneaky;\n\
                   }";
        let (pkg, imports) = parse_java(src).unwrap();
        assert_eq!(pkg, "com.x");
        assert_eq!(imports, vec!["java.util.List"]);
    }
}
