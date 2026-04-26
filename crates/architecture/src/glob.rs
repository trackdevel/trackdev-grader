//! Tiny package-glob matcher (T-P2.2).
//!
//! The architecture rules use patterns like `**/domain/**` against Java
//! package names (`com.example.domain.user`). A full glob crate is overkill
//! and pulls a transitive tree we don't need; the syntax we actually use
//! reduces to:
//!
//! - `**` matches any sequence of package segments (including zero).
//! - `*` matches any single package segment.
//! - Anything else matches the segment literally.
//!
//! Patterns are split on `/` (the `architecture.toml` convention so the
//! same file can be reused for filesystem-path matching later) but
//! checked against package names, which we split on `.` first.

/// Compiled package pattern. Cheap to clone and reuse across many checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagePattern {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Star,
    DoubleStar,
}

impl PackagePattern {
    pub fn new(pat: &str) -> Self {
        let segments = pat
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| match s {
                "**" => Segment::DoubleStar,
                "*" => Segment::Star,
                lit => Segment::Literal(lit.to_string()),
            })
            .collect();
        Self { segments }
    }

    /// Matches a package name (`a.b.c`). The split converts dots to the
    /// glob's segment representation. Returns true when the pattern
    /// covers the package.
    pub fn matches(&self, package: &str) -> bool {
        let pkg: Vec<&str> = package.split('.').filter(|s| !s.is_empty()).collect();
        match_segments(&self.segments, &pkg)
    }
}

fn match_segments(pat: &[Segment], pkg: &[&str]) -> bool {
    // Recursive backtracker. Patterns are short (3-5 segments) so the
    // worst-case is bounded; the alternative is a quadratic DP table that
    // would be more code than the recursion is worth.
    if pat.is_empty() {
        return pkg.is_empty();
    }
    match &pat[0] {
        Segment::DoubleStar => {
            // Match zero or more segments. Try matching the remaining
            // pattern against every suffix of `pkg`.
            for i in 0..=pkg.len() {
                if match_segments(&pat[1..], &pkg[i..]) {
                    return true;
                }
            }
            false
        }
        Segment::Star => !pkg.is_empty() && match_segments(&pat[1..], &pkg[1..]),
        Segment::Literal(lit) => {
            !pkg.is_empty() && pkg[0] == lit && match_segments(&pat[1..], &pkg[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(p: &str) -> PackagePattern {
        PackagePattern::new(p)
    }

    #[test]
    fn double_star_matches_any_depth() {
        assert!(pat("**/domain/**").matches("com.example.domain.user"));
        assert!(pat("**/domain/**").matches("com.domain.x"));
        assert!(pat("**/domain/**").matches("domain.user"));
        // domain at the very end is also "0 segments after" — that should match.
        assert!(pat("**/domain/**").matches("com.example.domain"));
    }

    #[test]
    fn double_star_does_not_match_unrelated() {
        assert!(!pat("**/domain/**").matches("com.example.application.user"));
        assert!(!pat("**/domain/**").matches("com.example.controller"));
    }

    #[test]
    fn single_star_matches_exactly_one_segment() {
        assert!(pat("com/*/web").matches("com.example.web"));
        assert!(!pat("com/*/web").matches("com.example.foo.web"));
        assert!(!pat("com/*/web").matches("com.web"));
    }

    #[test]
    fn literal_segments_are_exact() {
        assert!(pat("org/springframework/web").matches("org.springframework.web"));
        assert!(!pat("org/springframework/web").matches("org.springframework"));
        assert!(!pat("org/springframework/web").matches("org.springframework.web.servlet"));
    }

    #[test]
    fn double_star_at_end_also_matches_zero_remaining() {
        // For prefix-based rules.
        assert!(pat("com/example/**").matches("com.example"));
        assert!(pat("com/example/**").matches("com.example.web.x"));
    }

    #[test]
    fn empty_pattern_matches_only_empty_package() {
        assert!(pat("").matches(""));
        assert!(!pat("").matches("com.example"));
    }
}
