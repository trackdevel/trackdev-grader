//! Java source scanner backed by tree-sitter-java.
//!
//! Produces one [`ScannedFile`] per `.java` file in the repo, carrying
//! the file's declared `package`, its `import` declarations, the
//! original source bytes, and the parsed tree. Consumers — the legacy
//! layered / forbidden-import engine in [`crate::checker`] and the
//! AST-driven engine in [`crate::ast_rules`] — both read from the same
//! `ScannedFile`, so each file is read off disk and parsed exactly
//! once per scan.
//!
//! The previous line-based parser missed common Java declarations that
//! don't start with `class` / `public class` / `interface` (e.g.
//! `abstract class`, `final class`, `sealed class`, `record`, `enum`)
//! and didn't handle multi-line `import` declarations. tree-sitter
//! parses all of them correctly, and the resulting tree is reused by
//! the AST-rule pass so there's no second parse.

use std::path::Path;

use once_cell::sync::Lazy;
use tree_sitter::{Node, Parser, Tree};
use walkdir::WalkDir;

static JAVA_LANG: Lazy<tree_sitter::Language> = Lazy::new(|| tree_sitter_java::LANGUAGE.into());

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

/// One captured `import` declaration with its 1-based line range.
/// Multi-line imports preserve the full span; single-line imports
/// report `start_line == end_line`. The text is the canonical
/// `<package>.<name>` (or `<package>.*`) form, stripped of the
/// `import` keyword, the optional `static` modifier, the trailing
/// `;`, and intra-statement whitespace (which keeps multi-line
/// imports from leaking line breaks into the captured text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportLine {
    pub text: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// A parsed Java source file. Owns its source bytes and tree-sitter
/// `Tree`; consumers extract `Node`s on-demand via [`ScannedFile::root`].
pub struct ScannedFile {
    /// Path relative to the repo root, in the original separator form.
    pub rel_path: String,
    /// Declared package, e.g. `com.example.domain.user`. `ScannedFile`
    /// is not constructed for files without a `package` declaration —
    /// they belong to the default package and have no home in the
    /// layered model.
    pub package: String,
    /// All `import` declarations in declaration order.
    pub imports: Vec<ImportLine>,
    source: Vec<u8>,
    tree: Tree,
}

impl ScannedFile {
    /// Parse an in-memory file. Returns `None` when the source has no
    /// `package` declaration (default-package files are excluded from
    /// the layered model). Used by unit tests to fabricate a file
    /// without touching the filesystem; the production scanning path
    /// goes through [`scan_repo`].
    pub fn from_inline(rel_path: &str, source: &[u8]) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&JAVA_LANG).ok()?;
        let tree = parser.parse(source, None)?;
        let root = tree.root_node();
        let package = extract_package(root, source)?;
        let imports = extract_imports(root, source);
        Some(Self {
            rel_path: rel_path.to_string(),
            package,
            imports,
            source: source.to_vec(),
            tree,
        })
    }

    /// Root of the parsed tree; lifetime is tied to `&self`.
    pub fn root(&self) -> Node<'_> {
        self.tree.root_node()
    }

    /// Raw source bytes (UTF-8) backing the parse.
    pub fn source(&self) -> &[u8] {
        &self.source
    }
}

/// Walk a repo and return one [`ScannedFile`] per `.java` file with a
/// declared `package`. Files in build/dependency/VCS directories are
/// skipped via [`SKIP_DIRS`]. Files that fail to read or parse are
/// dropped silently (tree-sitter does error recovery, so a parse
/// failure on a real Java file is extraordinary).
pub fn scan_repo(repo_path: &Path) -> Vec<ScannedFile> {
    let mut parser = Parser::new();
    if parser.set_language(&JAVA_LANG).is_err() {
        return Vec::new();
    }
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
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Some(tree) = parser.parse(&bytes, None) else {
            continue;
        };
        let root = tree.root_node();
        let Some(package) = extract_package(root, &bytes) else {
            // Default-package file — not addressable by the layered rules.
            continue;
        };
        let imports = extract_imports(root, &bytes);
        out.push(ScannedFile {
            rel_path: rel.to_string_lossy().into_owned(),
            package,
            imports,
            source: bytes,
            tree,
        });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

/// Extract the file's `package` declaration. Returns `None` for
/// default-package files. Annotations on the package (`package-info.java`'s
/// `@Deprecated package …`) are ignored; only the dotted name component
/// is captured.
pub fn extract_package(root: Node, source: &[u8]) -> Option<String> {
    let mut cursor = root.walk();
    for c in root.children(&mut cursor) {
        if c.kind() != "package_declaration" {
            continue;
        }
        let mut inner = c.walk();
        for sub in c.children(&mut inner) {
            match sub.kind() {
                "identifier" | "scoped_identifier" => {
                    return Some(collapse_whitespace(&node_text(sub, source)));
                }
                _ => {}
            }
        }
    }
    None
}

/// Extract all `import` declarations in declaration order.
pub fn extract_imports(root: Node, source: &[u8]) -> Vec<ImportLine> {
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for c in root.children(&mut cursor) {
        if c.kind() != "import_declaration" {
            continue;
        }
        let raw = node_text(c, source);
        let trimmed = raw
            .trim()
            .strip_prefix("import")
            .unwrap_or(raw.trim())
            .trim();
        let cleaned = trimmed.trim_end_matches(';').trim();
        let cleaned = cleaned.strip_prefix("static ").unwrap_or(cleaned).trim();
        let text = collapse_whitespace(cleaned);
        if text.is_empty() {
            continue;
        }
        let start_line = c.start_position().row as u32 + 1;
        let end_line = c.end_position().row as u32 + 1;
        out.push(ImportLine {
            text,
            start_line,
            end_line,
        });
    }
    out
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_texts(imports: &[ImportLine]) -> Vec<&str> {
        imports.iter().map(|i| i.text.as_str()).collect()
    }

    #[test]
    fn extracts_package_and_imports() {
        let src = "package com.example.app;\n\
                   import java.util.List;\n\
                   import static org.junit.Assert.*;\n\
                   public class Foo {}\n";
        let f = ScannedFile::from_inline("Foo.java", src.as_bytes()).unwrap();
        assert_eq!(f.package, "com.example.app");
        assert_eq!(
            import_texts(&f.imports),
            vec!["java.util.List", "org.junit.Assert.*"]
        );
        assert_eq!(f.imports[0].start_line, 2);
        assert_eq!(f.imports[1].start_line, 3);
    }

    #[test]
    fn block_comment_imports_are_ignored_by_tree_sitter() {
        let src = "package com.x;\n\
                   /* import java.io.File; */\n\
                   import java.util.List;\n\
                   public class A {}";
        let f = ScannedFile::from_inline("A.java", src.as_bytes()).unwrap();
        assert_eq!(import_texts(&f.imports), vec!["java.util.List"]);
        assert_eq!(f.imports[0].start_line, 3);
    }

    #[test]
    fn no_package_returns_none() {
        let src = "import java.util.List;\npublic class A {}";
        assert!(ScannedFile::from_inline("A.java", src.as_bytes()).is_none());
    }

    #[test]
    fn multiline_import_is_collapsed_and_spans_full_range() {
        let src = "package com.x;\n\
                   import\n\
                       com.example\n\
                       .Foo;\n\
                   public class A {}";
        let f = ScannedFile::from_inline("A.java", src.as_bytes()).unwrap();
        assert_eq!(import_texts(&f.imports), vec!["com.example.Foo"]);
        assert_eq!(f.imports[0].start_line, 2);
        assert_eq!(f.imports[0].end_line, 4);
    }

    #[test]
    fn record_declaration_does_not_swallow_following_imports() {
        // The pre-tree-sitter scanner's keyword list missed `record`;
        // tree-sitter recognises it natively.
        let src = "package com.x;\n\
                   import java.util.List;\n\
                   public record User(String name) {}\n";
        let f = ScannedFile::from_inline("User.java", src.as_bytes()).unwrap();
        assert_eq!(import_texts(&f.imports), vec!["java.util.List"]);
    }

    #[test]
    fn enum_declaration_does_not_swallow_following_imports() {
        let src = "package com.x;\n\
                   import java.util.List;\n\
                   public enum Color { RED, GREEN }\n";
        let f = ScannedFile::from_inline("Color.java", src.as_bytes()).unwrap();
        assert_eq!(import_texts(&f.imports), vec!["java.util.List"]);
    }

    #[test]
    fn abstract_or_sealed_class_keywords_are_handled() {
        // Pre-tree-sitter, `abstract class` / `sealed class` weren't in
        // the keyword list that halted the scan. They're now structural
        // — tree-sitter parses them as `class_declaration`.
        let src = "package com.x;\n\
                   import java.util.List;\n\
                   public abstract sealed class Base permits Sub {}\n";
        let f = ScannedFile::from_inline("Base.java", src.as_bytes()).unwrap();
        assert_eq!(import_texts(&f.imports), vec!["java.util.List"]);
    }

    #[test]
    fn scan_repo_walks_filesystem_and_drops_default_package_files() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/main/java/com/x")).unwrap();
        std::fs::write(
            root.join("src/main/java/com/x/A.java"),
            "package com.x;\nimport java.util.List;\npublic class A {}",
        )
        .unwrap();
        // Default-package file — dropped.
        std::fs::write(
            root.join("src/main/java/Default.java"),
            "import java.util.List;\npublic class Default {}",
        )
        .unwrap();
        // Build dir — dropped.
        std::fs::create_dir_all(root.join("build/generated")).unwrap();
        std::fs::write(
            root.join("build/generated/G.java"),
            "package com.gen;\nimport java.util.List;\npublic class G {}",
        )
        .unwrap();

        let files = scan_repo(root);
        let names: Vec<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(names, vec!["src/main/java/com/x/A.java"]);
    }
}
