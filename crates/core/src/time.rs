//! Shared timestamp parsing.
//!
//! TrackDev emits the RFC 3339 short form `YYYY-MM-DDThh:mmZ` (no seconds),
//! which strict `DateTime::parse_from_rfc3339` rejects. Python's
//! `datetime.fromisoformat` in 3.11+ accepts it, so the Rust port has to match
//! or entire sprints silently drop to zero in temporal/regularity stages.

use chrono::{DateTime, NaiveDateTime, Utc};

/// Parse an ISO-8601 / RFC 3339 timestamp. Accepts both the strict form and
/// TrackDev's minute-precision `YYYY-MM-DDThh:mmZ` variant. Returns `None`
/// for empty or malformed input.
pub fn parse_iso(ts: &str) -> Option<DateTime<Utc>> {
    if ts.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Some(stem) = ts.strip_suffix('Z') {
        for fmt in ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"] {
            if let Ok(naive) = NaiveDateTime::parse_from_str(stem, fmt) {
                return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
            }
        }
    }
    None
}
