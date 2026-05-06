//! Shared timestamp parsing.
//!
//! TrackDev emits the RFC 3339 short form `YYYY-MM-DDThh:mmZ` (no seconds),
//! which strict `DateTime::parse_from_rfc3339` rejects. Python's
//! `datetime.fromisoformat` in 3.11+ accepts it, so the Rust port has to match
//! or entire sprints silently drop to zero in temporal/regularity stages.

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::Connection;

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

// ---- Sprint window helpers (T-P3.4) ----
//
// Shared by every artifact attribution module (architecture, complexity,
// static analysis) so the blame-derived `introduced_sprint_id` column on
// each `*_findings`/`*_violations` table is computed uniformly.

/// `[(sprint_id, start_unix, end_unix)]` ordered by start_date ascending.
/// Use [`load_sprint_windows`] to populate; pass to [`containing_sprint_id`]
/// to look up which sprint a unix timestamp falls inside.
pub type SprintWindows = Vec<(i64, i64, i64)>;

/// Load every sprint with parseable start/end ISO timestamps as a sorted
/// list of `(sprint_id, start_unix, end_unix)`. Sprints whose dates fail
/// to parse via [`parse_iso`] are silently dropped.
pub fn load_sprint_windows(conn: &Connection) -> rusqlite::Result<SprintWindows> {
    let mut stmt = conn.prepare(
        "SELECT id, start_date, end_date FROM sprints
         WHERE start_date IS NOT NULL AND end_date IS NOT NULL
         ORDER BY start_date ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out: SprintWindows = Vec::new();
    for r in rows {
        let (id, start, end) = r?;
        let s = match parse_iso(&start) {
            Some(dt) => dt.timestamp(),
            None => continue,
        };
        let e = match parse_iso(&end) {
            Some(dt) => dt.timestamp(),
            None => continue,
        };
        out.push((id, s, e));
    }
    Ok(out)
}

/// First sprint window whose `[start..=end]` (inclusive) contains the
/// timestamp. Sprint windows can overlap by a few hours at sprint
/// boundaries; we deliberately pick the *earliest* containing window —
/// semantically "the sprint the work was introduced during".
pub fn containing_sprint_id(windows: &SprintWindows, ts: i64) -> Option<i64> {
    for (id, s, e) in windows {
        if ts >= *s && ts <= *e {
            return Some(*id);
        }
    }
    None
}

/// Mutate `slot` so it tracks the minimum positive timestamp seen.
/// No-op when `ts <= 0` (matches the existing convention where a 0 or
/// negative `git blame` author-time signals "missing"). Used by the
/// blame-attribution loops to derive `introduced_sprint_id`.
pub fn track_min_time(slot: &mut Option<i64>, ts: i64) {
    if ts <= 0 {
        return;
    }
    match (*slot, ts) {
        (Some(prev), now) if now < prev => *slot = Some(now),
        (None, now) => *slot = Some(now),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sprints (id INTEGER PRIMARY KEY, start_date TEXT, end_date TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn load_sprint_windows_parses_iso_and_sorts_ascending() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO sprints (id, start_date, end_date) VALUES
                (2, '2026-02-01T00:00:00Z', '2026-02-15T23:59:59Z'),
                (1, '2026-01-01T00:00:00Z', '2026-01-31T23:59:59Z');",
        )
        .unwrap();
        let w = load_sprint_windows(&conn).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].0, 1, "sorted by start_date ASC");
        assert_eq!(w[1].0, 2);
    }

    #[test]
    fn load_sprint_windows_drops_unparseable_dates() {
        let conn = mk_conn();
        conn.execute_batch(
            "INSERT INTO sprints (id, start_date, end_date) VALUES
                (1, 'garbage', '2026-01-31T23:59:59Z'),
                (2, '2026-02-01T00:00:00Z', '2026-02-15T23:59:59Z');",
        )
        .unwrap();
        let w = load_sprint_windows(&conn).unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].0, 2);
    }

    #[test]
    fn containing_sprint_id_picks_first_match_at_boundary() {
        // Two windows with the second starting on the same second the
        // first ends. The earliest-match rule wins.
        let windows: SprintWindows = vec![(1, 100, 200), (2, 200, 300)];
        assert_eq!(containing_sprint_id(&windows, 150), Some(1));
        assert_eq!(containing_sprint_id(&windows, 200), Some(1));
        assert_eq!(containing_sprint_id(&windows, 250), Some(2));
        assert_eq!(containing_sprint_id(&windows, 99), None);
        assert_eq!(containing_sprint_id(&windows, 301), None);
    }

    #[test]
    fn track_min_time_keeps_the_earliest_positive_value() {
        let mut slot: Option<i64> = None;
        track_min_time(&mut slot, 0); // ignored
        assert_eq!(slot, None);
        track_min_time(&mut slot, -5); // ignored
        assert_eq!(slot, None);
        track_min_time(&mut slot, 10);
        assert_eq!(slot, Some(10));
        track_min_time(&mut slot, 20);
        assert_eq!(slot, Some(10));
        track_min_time(&mut slot, 5);
        assert_eq!(slot, Some(5));
    }
}
