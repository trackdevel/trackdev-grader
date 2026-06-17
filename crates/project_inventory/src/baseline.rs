//! Baseline inventory manifest (EXTRA_TECH).
//!
//! The course starter repos are constant across a cohort, so we capture their
//! inventory once into a checked-in `config/inventory_baseline.toml` (generated
//! by the `inventory-baseline` CLI from the two reference repos) and diff every
//! student repo against it at scan time.
//!
//! Missing file → an empty baseline (everything counts as extra) rather than a
//! hard failure, so a fresh checkout still runs.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::catalog::Stack;

/// One stack's baseline: the starter's dependency coordinates plus the feature
/// metric values it already ships (so `extra = max(0, student - baseline)`).
///
/// Field order matters for TOML serialization: scalars/arrays must precede the
/// `feature_metrics` table, otherwise the emitted `[stack.feature_metrics]`
/// sub-table would swallow any following keys.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct StackBaseline {
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub source_commit: Option<String>,
    #[serde(default)]
    pub feature_metrics: std::collections::BTreeMap<String, f64>,
}

impl StackBaseline {
    /// Dependency coordinates as a set for fast difference.
    pub fn dep_set(&self) -> BTreeSet<String> {
        self.dependencies
            .iter()
            .map(|d| d.trim().to_ascii_lowercase())
            .collect()
    }

    /// Baseline value for a feature metric key (0.0 when absent).
    pub fn feature(&self, key: &str) -> f64 {
        self.feature_metrics.get(key).copied().unwrap_or(0.0)
    }
}

/// The full manifest: one baseline per stack.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct InventoryBaseline {
    #[serde(default)]
    pub android: StackBaseline,
    #[serde(default)]
    pub spring: StackBaseline,
}

impl InventoryBaseline {
    /// Load from a TOML path. Missing/unparseable → empty baseline (logged).
    pub fn load(path: &Path) -> InventoryBaseline {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<InventoryBaseline>(&text).unwrap_or_else(|e| {
                warn!(path = %path.display(), error = %e,
                    "inventory_baseline.toml parse failed; using empty baseline");
                InventoryBaseline::default()
            }),
            Err(_) => InventoryBaseline::default(),
        }
    }

    pub fn for_stack(&self, stack: Stack) -> &StackBaseline {
        match stack {
            Stack::Android => &self.android,
            Stack::Spring => &self.spring,
        }
    }

    /// Serialize to a TOML document (used by the `inventory-baseline` tool).
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_empty_baseline() {
        let b = InventoryBaseline::load(Path::new("/no/such/inventory_baseline.toml"));
        assert!(b.for_stack(Stack::Android).dep_set().is_empty());
        assert_eq!(
            b.for_stack(Stack::Spring).feature("email_send_site_count"),
            0.0
        );
    }

    #[test]
    fn parses_manifest_and_normalizes_dep_case() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("baseline.toml");
        std::fs::write(
            &p,
            r#"
[android]
dependencies = ["androidx.room:room-runtime", "COM.Squareup.Retrofit2:Retrofit"]
source_commit = "abc123"

[android.feature_metrics]
av_usage_count = 0.0

[spring]
dependencies = ["org.springframework.boot:spring-boot-starter-web"]
"#,
        )
        .unwrap();
        let b = InventoryBaseline::load(&p);
        let android = b.for_stack(Stack::Android);
        assert!(android.dep_set().contains("androidx.room:room-runtime"));
        // case-normalized
        assert!(android
            .dep_set()
            .contains("com.squareup.retrofit2:retrofit"));
        assert_eq!(android.source_commit.as_deref(), Some("abc123"));
        assert!(b
            .for_stack(Stack::Spring)
            .dep_set()
            .contains("org.springframework.boot:spring-boot-starter-web"));
    }

    #[test]
    fn to_toml_round_trips() {
        let mut b = InventoryBaseline::default();
        b.android.dependencies = vec!["androidx.room:room-runtime".into()];
        b.android.source_commit = Some("deadbeef".into());
        b.android
            .feature_metrics
            .insert("av_usage_count".into(), 0.0);
        b.spring.dependencies = vec!["io.minio:minio".into()];
        let text = b.to_toml_string().expect("serialize");
        let back: InventoryBaseline = toml::from_str(&text).expect("parse back");
        assert_eq!(b, back);
    }
}
