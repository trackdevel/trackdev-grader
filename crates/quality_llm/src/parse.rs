//! Parse LLM JSON replies into advisory flag rows.

use anyhow::{Context, Result};
use serde_json::Value;
use sprint_grader_evaluate::extract_json_object;

const ALLOWED_CATEGORIES: &[&str] = &[
    "readability",
    "error_handling",
    "testing",
    "complexity",
    "duplication",
    "naming",
    "validation",
    "dead_code",
    "other",
];

const ALLOWED_SEVERITIES: &[&str] = &["INFO", "WARNING", "CRITICAL"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFlag {
    pub category: String,
    pub severity: String,
    pub summary: String,
    pub detail: Option<String>,
    pub student_id: Option<String>,
}

pub fn parse_quality_flags_json(raw: &str) -> Result<Vec<ParsedFlag>> {
    let root = extract_json_object(raw).context("LLM reply did not contain a JSON object")?;
    let flags = root
        .get("flags")
        .and_then(Value::as_array)
        .context("LLM JSON missing `flags` array")?;

    let mut out = Vec::new();
    for (i, item) in flags.iter().enumerate() {
        let obj = item
            .as_object()
            .with_context(|| format!("flags[{i}] is not an object"))?;
        let category = normalize_category(
            obj.get("category")
                .and_then(Value::as_str)
                .unwrap_or("other"),
        );
        let severity = normalize_severity(
            obj.get("severity")
                .and_then(Value::as_str)
                .context(format!("flags[{i}] missing severity"))?,
        )?;
        let summary = obj
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .context(format!("flags[{i}] missing summary"))?
            .to_string();
        let detail = obj
            .get("detail")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let student_id = obj
            .get("student_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.push(ParsedFlag {
            category,
            severity,
            summary,
            detail,
            student_id,
        });
    }
    Ok(out)
}

fn normalize_category(raw: &str) -> String {
    let key = raw.trim().to_lowercase().replace('-', "_");
    if ALLOWED_CATEGORIES.contains(&key.as_str()) {
        key
    } else {
        "other".to_string()
    }
}

fn normalize_severity(raw: &str) -> Result<String> {
    let upper = raw.trim().to_uppercase();
    if ALLOWED_SEVERITIES.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        anyhow::bail!("unknown severity {raw:?}; expected INFO, WARNING, or CRITICAL")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_array() {
        let raw = r#"{"flags":[{"category":"error_handling","severity":"WARNING","summary":"Swallows exception","detail":"Line 42 catches Exception and logs only."}]}"#;
        let flags = parse_quality_flags_json(raw).unwrap();
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].category, "error_handling");
        assert_eq!(flags[0].severity, "WARNING");
    }

    #[test]
    fn empty_flags_is_ok() {
        assert!(parse_quality_flags_json(r#"{"flags":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn fenced_json_is_accepted() {
        let raw = "```json\n{\"flags\":[]}\n```";
        assert!(parse_quality_flags_json(raw).unwrap().is_empty());
    }

    #[test]
    fn optional_student_id_parses() {
        let raw = r#"{"flags":[{"category":"testing","severity":"INFO","summary":"Low tests","student_id":"alice"}]}"#;
        let flags = parse_quality_flags_json(raw).unwrap();
        assert_eq!(flags[0].student_id.as_deref(), Some("alice"));
    }
}
