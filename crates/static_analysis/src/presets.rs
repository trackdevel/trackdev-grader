//! Embedded analyzer rulesets, materialised to disk on demand.
//!
//! All three analyzers (PMD now, Checkstyle in T3, SpotBugs in T6) take a
//! ruleset XML *path* on the command line — never stdin. So the presets
//! ship as `include_str!` blobs and are written to a per-invocation temp
//! file when the analyzer needs them. The temp file's lifetime is bound to
//! the returned `tempfile::NamedTempFile` — drop it and the file is gone.
//!
//! The embedded XMLs live under `src/presets/<analyzer>/<preset>.xml`.

use std::io::Write;

use anyhow::{anyhow, Result};
use tempfile::NamedTempFile;

// --- PMD --------------------------------------------------------------------

const PMD_BEGINNER_XML: &str = include_str!("presets/pmd/beginner.xml");
const PMD_STANDARD_XML: &str = include_str!("presets/pmd/standard.xml");
const PMD_STRICT_XML: &str = include_str!("presets/pmd/strict.xml");

/// Resolve a PMD `preset` reference (`"beginner" | "standard" | "strict"`)
/// to a freshly-materialised on-disk XML file. The returned `NamedTempFile`
/// must outlive the PMD process — typical usage is to bind it in the
/// caller's stack frame for the duration of `Command::status`.
pub fn resolve_pmd_ruleset(preset: &str) -> Result<NamedTempFile> {
    let body = match preset {
        "beginner" => PMD_BEGINNER_XML,
        "standard" => PMD_STANDARD_XML,
        "strict" => PMD_STRICT_XML,
        other => {
            return Err(anyhow!(
                "unknown PMD preset '{}'; expected one of: beginner, standard, strict",
                other
            ));
        }
    };
    write_to_temp(body, ".xml")
}

fn write_to_temp(body: &str, suffix: &str) -> Result<NamedTempFile> {
    let mut tmp = tempfile::Builder::new()
        .prefix("trackdev-static-analysis-")
        .suffix(suffix)
        .tempfile()?;
    tmp.write_all(body.as_bytes())?;
    tmp.flush()?;
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmd_presets_are_non_empty_xml() {
        assert!(PMD_BEGINNER_XML.contains("<ruleset"));
        assert!(PMD_STANDARD_XML.contains("<ruleset"));
        assert!(PMD_STRICT_XML.contains("<ruleset"));
    }

    #[test]
    fn resolve_pmd_ruleset_writes_to_disk() {
        let tmp = resolve_pmd_ruleset("standard").unwrap();
        let body = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(body.contains("<ruleset"));
    }

    #[test]
    fn unknown_preset_is_rejected() {
        let err = resolve_pmd_ruleset("nonsense").unwrap_err();
        assert!(err.to_string().contains("unknown PMD preset"));
    }
}
