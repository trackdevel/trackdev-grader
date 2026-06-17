//! Technology catalog (EXTRA_TECH Layer A).
//!
//! Maps Gradle dependency coordinates (`group:artifact`, version stripped)
//! to named technologies + a category. Loaded from
//! `config/technology_catalog.toml`; falls back to a built-in default so the
//! pipeline never hard-fails when the file is absent (mirrors the
//! `architecture.toml` "extend, don't replace" idiom).
//!
//! Categories line up with the curated AST detectors so a Gradle coordinate
//! can corroborate an AST finding: `fcm`, `specifications`, `email`,
//! `graphics`, `av`, and the catch-all `dependency` (any other extra lib —
//! still counted as breadth).

use std::path::Path;

use serde::Deserialize;
use tracing::warn;

/// Which stack a repo belongs to. Inferred from the repo name the same way
/// `grade_core::axes::repo_kind` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stack {
    Android,
    Spring,
}

impl Stack {
    pub fn from_repo_name(repo_full_name: &str) -> Stack {
        let lower = repo_full_name.to_lowercase();
        if lower.starts_with("android") || lower.contains("-android") || lower.contains("/android")
        {
            Stack::Android
        } else {
            Stack::Spring
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Stack::Android => "android",
            Stack::Spring => "spring",
        }
    }
}

/// One catalog entry: a named technology and the coordinates that signal it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TechnologyEntry {
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub coordinates: Vec<String>,
}

/// The full catalog (`[[technology]]` array in TOML).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct TechnologyCatalog {
    #[serde(default, rename = "technology")]
    pub entries: Vec<TechnologyEntry>,
}

impl TechnologyCatalog {
    /// Load from a TOML path. Missing or unparseable file → built-in default
    /// (logged), never an error.
    pub fn load(path: &Path) -> TechnologyCatalog {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<TechnologyCatalog>(&text) {
                Ok(c) if !c.entries.is_empty() => c,
                Ok(_) => TechnologyCatalog::default_catalog(),
                Err(e) => {
                    warn!(path = %path.display(), error = %e,
                        "technology_catalog.toml parse failed; using built-in default");
                    TechnologyCatalog::default_catalog()
                }
            },
            Err(_) => TechnologyCatalog::default_catalog(),
        }
    }

    /// Classify a `group:artifact` coordinate. Returns `(technology, category)`
    /// when a catalog entry lists it; `None` when uncategorized.
    pub fn classify(&self, coord: &str) -> Option<(&str, &str)> {
        let coord = coord.trim();
        for e in &self.entries {
            if e.coordinates.iter().any(|c| c.eq_ignore_ascii_case(coord)) {
                return Some((e.name.as_str(), e.category.as_str()));
            }
        }
        None
    }

    /// Built-in seed mirroring the cohort's observed extra libraries.
    pub fn default_catalog() -> TechnologyCatalog {
        fn e(name: &str, category: &str, coords: &[&str]) -> TechnologyEntry {
            TechnologyEntry {
                name: name.to_string(),
                category: category.to_string(),
                coordinates: coords.iter().map(|c| c.to_string()).collect(),
            }
        }
        TechnologyCatalog {
            entries: vec![
                e(
                    "Firebase Cloud Messaging",
                    "fcm",
                    &[
                        "com.google.firebase:firebase-admin",
                        "com.google.firebase:firebase-messaging",
                        "com.google.firebase:firebase-bom",
                    ],
                ),
                e(
                    "Email (Spring Mail)",
                    "email",
                    &["org.springframework.boot:spring-boot-starter-mail"],
                ),
                e(
                    "Media3 / ExoPlayer",
                    "av",
                    &[
                        "androidx.media3:media3-exoplayer",
                        "androidx.media3:media3-ui",
                        "com.google.android.exoplayer:exoplayer",
                    ],
                ),
                // Image-loading libs are breadth (Layer A "dependency"), not graphics.
                e(
                    "Image loading (Glide)",
                    "dependency",
                    &[
                        "com.github.bumptech.glide:glide",
                        "jp.wasabeef:glide-transformations",
                    ],
                ),
                e(
                    "Image loading (Picasso)",
                    "dependency",
                    &["com.squareup.picasso:picasso"],
                ),
                e(
                    "Maps (osmdroid)",
                    "dependency",
                    &["org.osmdroid:osmdroid-android"],
                ),
                e(
                    "QR scanning (ZXing)",
                    "dependency",
                    &["com.journeyapps:zxing-android-embedded"],
                ),
                e(
                    "Payments (Stripe)",
                    "dependency",
                    &["com.stripe:stripe-java", "com.stripe:stripe-android"],
                ),
                e(
                    "Spring AI",
                    "dependency",
                    &["org.springframework.ai:spring-ai-bom"],
                ),
                e("RxJava", "dependency", &["io.reactivex.rxjava2:rxjava"]),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_inference_matches_repo_kind() {
        assert_eq!(Stack::from_repo_name("org/android-team01"), Stack::Android);
        assert_eq!(Stack::from_repo_name("android-pds26_1a"), Stack::Android);
        assert_eq!(Stack::from_repo_name("org/spring-team01"), Stack::Spring);
    }

    #[test]
    fn default_catalog_classifies_known_coordinates() {
        let cat = TechnologyCatalog::default_catalog();
        assert_eq!(
            cat.classify("com.google.firebase:firebase-admin"),
            Some(("Firebase Cloud Messaging", "fcm"))
        );
        assert_eq!(
            cat.classify("org.springframework.boot:spring-boot-starter-mail"),
            Some(("Email (Spring Mail)", "email"))
        );
        // Image loading is breadth, not graphics.
        assert_eq!(
            cat.classify("com.github.bumptech.glide:glide"),
            Some(("Image loading (Glide)", "dependency"))
        );
        assert!(cat.classify("com.unknown:whatever").is_none());
    }

    #[test]
    fn classify_is_case_insensitive_and_trims() {
        let cat = TechnologyCatalog::default_catalog();
        assert_eq!(
            cat.classify("  IO.REACTIVEX.RXJAVA2:RXJAVA "),
            Some(("RxJava", "dependency"))
        );
    }

    #[test]
    fn load_falls_back_to_default_when_missing() {
        let cat = TechnologyCatalog::load(Path::new("/no/such/technology_catalog.toml"));
        assert!(!cat.entries.is_empty());
    }

    #[test]
    fn load_parses_a_toml_override() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cat.toml");
        std::fs::write(
            &p,
            r#"
[[technology]]
name = "Custom Lib"
category = "dependency"
coordinates = ["com.acme:widget"]
"#,
        )
        .unwrap();
        let cat = TechnologyCatalog::load(&p);
        assert_eq!(
            cat.classify("com.acme:widget"),
            Some(("Custom Lib", "dependency"))
        );
    }
}
