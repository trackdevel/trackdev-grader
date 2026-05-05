//! Single source of truth for the English-language strings the report
//! shows for the static-analysis section. Keeping them here makes a
//! future per-rule i18n pass mechanical: this file is the only place
//! to translate, callers reference constants only. Rule messages stay
//! in their tool-native language (English, mostly).

pub const BLOCK_HEADER: &str = "Static analysis";

pub const TEAM_TALLY_LABEL: &str = "Static analysis (team)";

pub const GLOSSARY_BODY: &str = concat!(
    "Findings from PMD / Checkstyle / SpotBugs, attributed to authors via\n",
    "`git blame -w --ignore-revs-file`. **Informational only** — these do not\n",
    "affect the assignment grade. The `weight` reflects how much of each\n",
    "offending region a student authored: a 1-line typo fix on a 30-line\n",
    "method weighs ~3 %, not 50 %."
);

pub const SEVERITY_CRITICAL_PLURAL: &str = "critical";
pub const SEVERITY_WARNING_PLURAL: &str = "warning";
pub const SEVERITY_INFO_PLURAL: &str = "info";

pub const WEIGHT_LABEL: &str = "weight";
pub const MORE_LABEL: &str = "more";
