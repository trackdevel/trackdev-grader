//! Per-PR data-quality signals accumulated in `pull_requests.attribution_errors`.
//!
//! See T-P1.5: the column existed in the schema but was barely used. Each
//! trigger site (base-sha fallback in survival, null-author/HTTP failure in
//! collect, stale fetch in collect) appends a single JSON entry rather than
//! overwriting. These are *informational* signals — never grading penalties.

use chrono::Utc;
use serde_json::{json, Value};

pub const ATTR_ERR_BASE_SHA_FALLBACK: &str = "base_sha_fallback";
pub const ATTR_ERR_NO_BASE_CANDIDATE: &str = "no_base_candidate";
pub const ATTR_ERR_NULL_AUTHOR: &str = "null_author_login";
pub const ATTR_ERR_STALE_FETCH: &str = "stale_github_fetch";
pub const ATTR_ERR_HTTP_FAILURE: &str = "github_http_error";

/// Hard cap on accumulated entries. Long-lived PRs that re-trip the same
/// signal across many runs would otherwise grow this column unbounded; we
/// keep the most recent N entries (drop oldest).
const MAX_ENTRIES: usize = 20;

/// Append `(kind, detail)` to the existing JSON array stored in
/// `pull_requests.attribution_errors` and return the updated JSON string.
///
/// `existing` is the current column value (may be `None`, an empty `[]`, or
/// a previously-written array). Garbage non-array JSON is treated as if the
/// column were empty so the caller doesn't have to special-case migration.
///
/// The new entry shape is:
/// ```json
/// { "kind": "base_sha_fallback",
///   "detail": "merge-base failed for PR-1234, used first_sha^1",
///   "observed_at": "2026-04-26T14:32:18Z" }
/// ```
pub fn merge_attribution_errors(existing: Option<&str>, kind: &str, detail: &str) -> String {
    let mut entries: Vec<Value> = match existing {
        Some(s) if !s.trim().is_empty() => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    entries.push(json!({
        "kind": kind,
        "detail": detail,
        "observed_at": Utc::now().to_rfc3339(),
    }));
    if entries.len() > MAX_ENTRIES {
        let drop = entries.len() - MAX_ENTRIES;
        entries.drain(..drop);
    }
    serde_json::to_string(&Value::Array(entries)).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_none_creates_one_entry() {
        let s = merge_attribution_errors(None, ATTR_ERR_BASE_SHA_FALLBACK, "x");
        let v: Value = serde_json::from_str(&s).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], ATTR_ERR_BASE_SHA_FALLBACK);
        assert_eq!(arr[0]["detail"], "x");
        assert!(arr[0]["observed_at"].as_str().is_some());
    }

    #[test]
    fn merge_into_empty_array_creates_one_entry() {
        let s = merge_attribution_errors(Some("[]"), ATTR_ERR_NULL_AUTHOR, "y");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[test]
    fn merge_appends_without_overwriting() {
        let first = merge_attribution_errors(None, ATTR_ERR_BASE_SHA_FALLBACK, "a");
        let second = merge_attribution_errors(Some(&first), ATTR_ERR_NULL_AUTHOR, "b");
        let third = merge_attribution_errors(Some(&second), ATTR_ERR_HTTP_FAILURE, "c");
        let v: Value = serde_json::from_str(&third).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["kind"], ATTR_ERR_BASE_SHA_FALLBACK);
        assert_eq!(arr[1]["kind"], ATTR_ERR_NULL_AUTHOR);
        assert_eq!(arr[2]["kind"], ATTR_ERR_HTTP_FAILURE);
    }

    #[test]
    fn merge_caps_at_max_entries_dropping_oldest() {
        let mut s = String::from("[]");
        for i in 0..MAX_ENTRIES + 5 {
            s = merge_attribution_errors(Some(&s), ATTR_ERR_HTTP_FAILURE, &format!("{i}"));
        }
        let v: Value = serde_json::from_str(&s).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), MAX_ENTRIES);
        // Oldest dropped: first entry's detail must be "5" (entries 0..4 dropped).
        assert_eq!(arr[0]["detail"], "5");
    }

    #[test]
    fn merge_recovers_from_garbage_existing_value() {
        // Pre-T-P1.5 collector wrote a serialised Vec<String> instead of an
        // array of objects. Treat as empty rather than crash.
        let legacy = "[\"PR linked to tasks with different assignees\"]";
        let s = merge_attribution_errors(Some(legacy), ATTR_ERR_HTTP_FAILURE, "z");
        let v: Value = serde_json::from_str(&s).unwrap();
        let arr = v.as_array().unwrap();
        // Legacy strings are preserved as-is in the array (Value::String); the
        // new entry is appended.
        assert!(!arr.is_empty());
        let last = arr.last().unwrap();
        assert_eq!(last["kind"], ATTR_ERR_HTTP_FAILURE);
    }
}
