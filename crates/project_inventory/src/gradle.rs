//! Gradle dependency scanner (EXTRA_TECH Layer A — breadth).
//!
//! Text/regex scan of `build.gradle` (Groovy, the only style this cohort uses),
//! with defensive handling of `*.gradle.kts` and `gradle/libs.versions.toml`.
//! Extracts `group:artifact` (version dropped, lowercased) from **main**
//! dependency configurations, excluding `test*` / `androidTest*`. The shared
//! Java AST scanner (`architecture::scanner`) is `.java`-only and skips
//! package-less files, so Gradle parsing has no existing home — this is it.

use std::collections::BTreeSet;
use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use walkdir::WalkDir;

const SKIP_DIRS: &[&str] = &["build", ".gradle", ".git", ".idea", "node_modules"];

/// `<config> [(] [platform(] "group:artifact[:version]"` — captures the
/// configuration keyword (g1) and the coordinate (g2). Handles both quote
/// styles and the Kotlin-DSL `impl("…")` / `platform("…")` forms.
static DEP_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?m)^\s*(\w+)\s*\(?\s*(?:platform\s*\(\s*)?["']([A-Za-z0-9_.\-]+:[A-Za-z0-9_.\-]+(?::[^"']*)?)["']"#,
    )
    .expect("dep regex")
});

/// `module = "group:artifact[:version]"` inside a version catalog.
static CATALOG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"module\s*=\s*["']([A-Za-z0-9_.\-]+:[A-Za-z0-9_.\-]+)"#).expect("catalog regex")
});

fn is_main_config(cfg: &str) -> bool {
    if cfg.starts_with("test") || cfg.starts_with("androidTest") {
        return false;
    }
    matches!(
        cfg,
        "implementation"
            | "api"
            | "compileOnly"
            | "runtimeOnly"
            | "annotationProcessor"
            | "kapt"
            | "ksp"
            | "developmentOnly"
            | "debugImplementation"
            | "releaseImplementation"
    )
}

/// Reduce a captured coordinate to lowercased `group:artifact` (version dropped).
fn normalize_coord(raw: &str) -> Option<String> {
    let mut it = raw.split(':');
    let group = it.next()?.trim();
    let artifact = it.next()?.trim();
    if group.is_empty() || artifact.is_empty() {
        return None;
    }
    Some(format!("{}:{}", group, artifact).to_ascii_lowercase())
}

/// Extract coordinates from one Gradle/catalog file's text.
pub fn extract_coords_from_text(text: &str, is_catalog: bool) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if is_catalog {
        for cap in CATALOG_RE.captures_iter(text) {
            if let Some(c) = normalize_coord(&cap[1]) {
                out.insert(c);
            }
        }
        return out;
    }
    for cap in DEP_RE.captures_iter(text) {
        if !is_main_config(&cap[1]) {
            continue;
        }
        if let Some(c) = normalize_coord(&cap[2]) {
            out.insert(c);
        }
    }
    out
}

/// Walk a repo and return the deduped set of main-config `group:artifact`
/// coordinates across every `build.gradle(.kts)` and `libs.versions.toml`
/// outside build/VCS directories.
pub fn scan_gradle_coords(repo_path: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in WalkDir::new(repo_path).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(repo_path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
        {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let is_catalog = name == "libs.versions.toml";
        let is_gradle = name == "build.gradle" || name == "build.gradle.kts";
        if !is_catalog && !is_gradle {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(path) {
            out.extend(extract_coords_from_text(&text, is_catalog));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_both_quote_styles_and_processors() {
        let g = r#"
dependencies {
    implementation 'com.google.firebase:firebase-admin:9.2.0'
    implementation "androidx.room:room-runtime:2.8.4"
    annotationProcessor "androidx.room:room-compiler:2.8.4"
    implementation platform('com.google.firebase:firebase-bom:33.0.0')
}
"#;
        let c = extract_coords_from_text(g, false);
        assert!(c.contains("com.google.firebase:firebase-admin"));
        assert!(c.contains("androidx.room:room-runtime"));
        assert!(c.contains("androidx.room:room-compiler"));
        assert!(c.contains("com.google.firebase:firebase-bom"));
    }

    #[test]
    fn excludes_test_configs_and_project_refs() {
        let g = r#"
dependencies {
    testImplementation 'org.junit.jupiter:junit-jupiter:5.10.0'
    androidTestImplementation "androidx.test.ext:junit:1.1.5"
    implementation project(':core')
    implementation 'com.squareup.retrofit2:retrofit:2.9.0'
}
"#;
        let c = extract_coords_from_text(g, false);
        assert!(c.contains("com.squareup.retrofit2:retrofit"));
        assert!(!c.iter().any(|x| x.contains("junit")));
        assert!(!c.iter().any(|x| x.contains(":core")));
    }

    #[test]
    fn kotlin_dsl_and_catalog() {
        // One dependency per line, as real build files are written (the regex
        // anchors the config keyword to line-start to skip comments).
        let kts = "dependencies {\n    implementation(\"io.minio:minio:8.5.7\")\n}";
        assert!(extract_coords_from_text(kts, false).contains("io.minio:minio"));
        let cat = r#"
[libraries]
retrofit = { module = "com.squareup.retrofit2:retrofit", version.ref = "retrofit" }
"#;
        assert!(extract_coords_from_text(cat, true).contains("com.squareup.retrofit2:retrofit"));
    }

    #[test]
    fn scan_walks_repo_and_skips_build_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(
            root.join("app/build.gradle"),
            "dependencies {\n    implementation 'com.acme:lib:1.0'\n}",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("app/build/generated")).unwrap();
        std::fs::write(
            root.join("app/build/generated/build.gradle"),
            "dependencies {\n    implementation 'should:ignore:1.0'\n}",
        )
        .unwrap();
        let c = scan_gradle_coords(root);
        assert!(c.contains("com.acme:lib"));
        assert!(!c.contains("should:ignore"));
    }
}
