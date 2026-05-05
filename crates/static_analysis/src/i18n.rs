//! Single source of truth for the English-language strings the report
//! shows for the static-analysis section. Keeping them here makes a
//! future per-rule i18n pass mechanical: this file is the only place
//! to translate, callers reference constants only. Rule messages stay
//! in their tool-native language (English, mostly).

pub const SECTION_HEADER: &str = "Static code analysis (informational — does not affect the grade)";

pub const PER_STUDENT_SUBHEADER: &str = "Per student (attributed via `git blame`)";

pub const DISCLAIMER: &str = concat!(
    "> These findings are informational and do not affect the assignment grade.\n",
    "> Attribution via `git blame -w --ignore-revs-file`: a 1-line typo fix on a\n",
    "> 30-line method weighs ~3 %, not 50 %."
);

pub const NO_FINDINGS: &str = "_No findings for this sprint._";

pub const SEVERITY_CRITICAL_PLURAL: &str = "critical";
pub const SEVERITY_WARNING_PLURAL: &str = "warnings";
pub const SEVERITY_INFO_PLURAL: &str = "suggestions";

pub const WEIGHT_LABEL: &str = "weight";
pub const MORE_LABEL: &str = "more";
