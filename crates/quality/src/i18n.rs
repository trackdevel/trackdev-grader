//! Single source of truth for the English-language strings the report
//! shows for the complexity / testability section (T-CX). Mirrors the
//! `static_analysis::i18n` module so a future translation pass can edit
//! both in lockstep.
//!
//! Two audiences live here:
//! - Student-facing strings (always rendered): describe what the rule
//!   noticed, suggest a refactoring direction. No grading numbers.
//! - Professor-only strings (rendered when `--professor-report` is set):
//!   add per-student weighted attribution + the COMPLEXITY_HOTSPOT flag
//!   summary. Step 7 of T-CX wires those.

// ── Student-facing strings ─────────────────────────────────────────────

pub const SECTION_HEADER: &str = "Code complexity & testability";

pub const NO_FINDINGS: &str = "_No complex or hard-to-test methods detected this sprint._";

pub const INTRO_BLURB: &str = concat!(
    "These methods scored above the project's complexity ceilings or matched a\n",
    "rule that makes them hard to test in isolation (broad `catch`, hidden\n",
    "singleton access, inline `new` of a collaborator, non-deterministic time/\n",
    "random calls, reflection). Consider refactoring before the next sprint.\n"
);

// ── Professor-only strings (step 7) ────────────────────────────────────

pub const PROF_PER_STUDENT_HEADER: &str = "Per student (attributed via `git blame`)";
pub const PROF_WEIGHT_LABEL: &str = "weight";
pub const PROF_SCORE_LABEL: &str = "score";
pub const PROF_DISCLAIMER: &str = concat!(
    "> Attribution is bad-line-weighted: lines that introduce the offending\n",
    "> construct count 3×, control-flow lines 2×, plain method lines 1×. So a\n",
    "> typo fix on a long bad method weighs ~3 %, not 50 %.\n"
);
pub const PROF_FLAG_SUMMARY_HEADER: &str = "COMPLEXITY_HOTSPOT band";

// ── Severity labels (shared) ───────────────────────────────────────────

pub const SEVERITY_CRITICAL_PLURAL: &str = "critical";
pub const SEVERITY_WARNING_PLURAL: &str = "warnings";
pub const SEVERITY_INFO_PLURAL: &str = "suggestions";

pub const MORE_LABEL: &str = "more";

/// Map a `rule_key` from `method_complexity_findings.rule_key` to the
/// student-facing prose used in the report. Mirrors
/// `report/src/markdown.rs::KNOWN_RULE_DESCRIPTIONS` but specific to
/// T-CX rules. Unknown keys fall back to a humanised version of the
/// machine key (replace `-` with space, capitalise first letter).
pub fn rule_prose(rule_key: &str) -> &'static str {
    match rule_key {
        "cyclomatic" => "Cyclomatic complexity above the ceiling — too many branches in one method",
        "cognitive" => "Cognitive complexity above the ceiling — nested logic that's hard to follow",
        "nesting" => "Nesting depth above the ceiling — deeply indented control flow",
        "long-method" => "Method body exceeds the line-count ceiling",
        "wide-signature" => "Method takes more parameters than the ceiling allows",
        "broad-catch" => "Catches `Exception`/`Throwable` without rethrowing — swallows errors",
        "non-deterministic-call" => {
            "Reads the system clock, randomness, or `now()` directly — hard to test"
        }
        "inline-collaborator" => {
            "Instantiates a collaborator with `new` inside the method — bypasses dependency injection"
        }
        "static-singleton" => "Reaches a singleton via `getInstance()` or `.INSTANCE.` — hidden coupling",
        "reflection" => "Uses reflection to invoke methods or read fields — opaque control flow",
        _ => "Complexity / testability concern",
    }
}
