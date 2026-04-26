//! File-level code stylometry feature extraction.
//! Mirrors `src/ai_detect/stylometry.py` (regex-based, no tree-sitter).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use fancy_regex::Regex as FancyRegex;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::Map as JsonMap;
use tracing::{info, warn};
use walkdir::WalkDir;

use sprint_grader_core::stats;

type StyleFileEntry = (Vec<Option<f64>>, Option<bool>);

#[derive(Debug, Clone, Default)]
pub struct StyleFeatureVector {
    // Naming
    pub avg_identifier_length: f64,
    pub identifier_length_stddev: f64,
    pub camelcase_ratio: f64,
    pub screaming_snake_ratio: f64,
    pub single_char_var_ratio: f64,
    pub max_identifier_length: i64,
    // Comments
    pub comment_density: f64,
    pub avg_comment_length_chars: f64,
    pub inline_vs_block_ratio: f64,
    pub javadoc_ratio: f64,
    pub comment_to_code_ratio: f64,
    // Methods
    pub avg_method_length: f64,
    pub method_length_stddev: f64,
    pub avg_parameter_count: f64,
    pub avg_nesting_depth: f64,
    pub max_nesting_depth: i64,
    // Error handling
    pub try_catch_density: f64,
    pub empty_catch_ratio: f64,
    pub avg_catch_body_lines: f64,
    // Imports
    pub import_count: i64,
    pub wildcard_import_ratio: f64,
    pub import_alphabetized: bool,
    // Formatting
    pub blank_line_ratio: f64,
    // AI indicators
    pub has_comprehensive_javadoc: bool,
    pub has_null_checks_everywhere: bool,
    pub uniform_formatting: bool,
}

static IDENTIFIER_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-zA-Z_]\w*)\b").unwrap());
static CAMELCASE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z][a-zA-Z0-9]*$").unwrap());
static SCREAMING_SNAKE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[A-Z][A-Z0-9_]*$").unwrap());

static LINE_COMMENT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)//(.*)$").unwrap());
static BLOCK_COMMENT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)/\*(?-s:[^*])(.*?)\*/").unwrap());
static JAVADOC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)/\*\*(.*?)\*/").unwrap());

static METHOD_RE: Lazy<FancyRegex> = Lazy::new(|| {
    FancyRegex::new(
        r"(?:public|private|protected|static|\s)+[\w<>\[\],\s]+\s+(\w+)\s*\(([^)]*)\)\s*(?:throws\s+[\w,\s]+)?\s*\{",
    )
    .unwrap()
});

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*import\s+([\w.*]+)\s*;").unwrap());

static TRY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)\btry\s*\{").unwrap());
static CATCH_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)\bcatch\s*\([^)]*\)\s*\{").unwrap());

static NULL_CHECK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b\w+\s*[!=]=\s*null\b|\bnull\s*[!=]=\s*\w+\b|Objects\.requireNonNull").unwrap()
});

static JAVADOC_METHOD_RE: Lazy<FancyRegex> = Lazy::new(|| {
    FancyRegex::new(
        r"(?s)/\*\*.*?\*/\s*(?:@\w+\s+)*(?:public|protected)\s+(?:static\s+)?[\w<>\[\],\s]+\s+\w+\s*\(",
    )
    .unwrap()
});

static PUBLIC_METHOD_RE: Lazy<FancyRegex> = Lazy::new(|| {
    FancyRegex::new(r"(?:public|protected)\s+(?:static\s+)?[\w<>\[\],\s]+\s+\w+\s*\(").unwrap()
});

static JAVA_KEYWORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "abstract",
        "assert",
        "boolean",
        "break",
        "byte",
        "case",
        "catch",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "float",
        "for",
        "goto",
        "if",
        "implements",
        "import",
        "instanceof",
        "int",
        "interface",
        "long",
        "native",
        "new",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "static",
        "strictfp",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "void",
        "volatile",
        "while",
        "true",
        "false",
        "null",
        "var",
        "record",
        "sealed",
        "permits",
        "yield",
        "String",
        "Override",
    ]
    .iter()
    .copied()
    .collect()
});

fn strip_comments(content: &str) -> String {
    let r1 = JAVADOC_RE.replace_all(content, "");
    let r2 = BLOCK_COMMENT_RE.replace_all(&r1, "");
    LINE_COMMENT_RE.replace_all(&r2, "").to_string()
}

fn extract_identifiers(code_no_comments: &str) -> Vec<String> {
    IDENTIFIER_RE
        .captures_iter(code_no_comments)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .filter(|ident| {
            !JAVA_KEYWORDS.contains(ident.as_str())
                && !ident.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .collect()
}

fn compute_naming_features(identifiers: &[String], fv: &mut StyleFeatureVector) {
    if identifiers.is_empty() {
        return;
    }
    let lengths: Vec<f64> = identifiers
        .iter()
        .map(|s| s.chars().count() as f64)
        .collect();
    fv.avg_identifier_length = stats::mean(&lengths);
    fv.identifier_length_stddev = if lengths.len() >= 2 {
        sample_stdev(&lengths)
    } else {
        0.0
    };
    fv.max_identifier_length = lengths.iter().fold(0.0_f64, |a, b| a.max(*b)) as i64;
    let n = identifiers.len() as f64;
    fv.camelcase_ratio = identifiers
        .iter()
        .filter(|i| CAMELCASE_RE.is_match(i))
        .count() as f64
        / n;
    fv.screaming_snake_ratio = identifiers
        .iter()
        .filter(|i| SCREAMING_SNAKE_RE.is_match(i))
        .count() as f64
        / n;
    fv.single_char_var_ratio = identifiers
        .iter()
        .filter(|i| i.chars().count() == 1)
        .count() as f64
        / n;
}

/// `statistics.stdev` in Python is the *sample* standard deviation (divide by
/// n-1). stylometry uses it directly, so we match that rather than the
/// population form used elsewhere in the crate.
fn sample_stdev(values: &[f64]) -> f64 {
    let n = values.len();
    if n < 2 {
        return 0.0;
    }
    let mean = stats::mean(values);
    let var: f64 = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    var.sqrt()
}

fn compute_comment_features(content: &str, fv: &mut StyleFeatureVector) {
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = if lines.is_empty() { 1 } else { lines.len() };

    let inline: Vec<String> = LINE_COMMENT_RE
        .captures_iter(content)
        .map(|c| c.get(1).map(|m| m.as_str()).unwrap_or("").to_string())
        .collect();
    let block: Vec<String> = BLOCK_COMMENT_RE
        .captures_iter(content)
        .map(|c| c.get(1).map(|m| m.as_str()).unwrap_or("").to_string())
        .collect();
    let javadoc: Vec<String> = JAVADOC_RE
        .captures_iter(content)
        .map(|c| c.get(1).map(|m| m.as_str()).unwrap_or("").to_string())
        .collect();

    let total_comment_count = inline.len() + block.len() + javadoc.len();
    if total_comment_count == 0 {
        return;
    }

    fv.comment_density = total_comment_count as f64 / total_lines as f64;
    let comment_lengths: Vec<f64> = inline
        .iter()
        .chain(block.iter())
        .chain(javadoc.iter())
        .map(|c| c.trim().chars().count() as f64)
        .collect();
    fv.avg_comment_length_chars = if comment_lengths.is_empty() {
        0.0
    } else {
        stats::mean(&comment_lengths)
    };

    let block_count = block.len() + javadoc.len();
    let inline_count = inline.len();
    fv.inline_vs_block_ratio = if block_count > 0 {
        inline_count as f64 / block_count as f64
    } else {
        inline_count as f64
    };
    fv.javadoc_ratio = javadoc.len() as f64 / total_comment_count as f64;

    let code_only = strip_comments(content);
    let code_lines = code_only
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .count();
    fv.comment_to_code_ratio = if code_lines > 0 {
        total_comment_count as f64 / code_lines as f64
    } else {
        0.0
    };
}

/// Find the matching closing brace starting from `open_pos` (the position of
/// `{`). Honors Java string / char literal escape semantics.
fn find_matching_brace(content: &[char], open_pos: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape_next = false;
    let mut i = open_pos;
    while i < content.len() {
        let ch = content[i];
        if escape_next {
            escape_next = false;
            i += 1;
            continue;
        }
        if ch == '\\' {
            escape_next = true;
            i += 1;
            continue;
        }
        if ch == '"' && !in_char {
            in_string = !in_string;
        } else if ch == '\'' && !in_string {
            in_char = !in_char;
        } else if !in_string && !in_char {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

fn compute_method_features(code_no_comments: &str, fv: &mut StyleFeatureVector) {
    let chars: Vec<char> = code_no_comments.chars().collect();
    let byte_to_char: Vec<usize> = {
        // Map each byte offset to the character index for the chars vector.
        let mut map = Vec::with_capacity(code_no_comments.len() + 1);
        let mut ci = 0;
        for (bi, ch) in code_no_comments.char_indices() {
            while map.len() <= bi {
                map.push(ci);
            }
            let _ = ch;
            ci += 1;
        }
        while map.len() <= code_no_comments.len() {
            map.push(ci);
        }
        map
    };

    let mut method_lengths: Vec<f64> = Vec::new();
    let mut param_counts: Vec<f64> = Vec::new();
    let mut nesting_depths: Vec<i64> = Vec::new();

    for cap in METHOD_RE.captures_iter(code_no_comments).flatten() {
        let params_match = match cap.get(2) {
            Some(p) => p,
            None => continue,
        };
        let params_str = params_match.as_str().trim();
        let param_count = if params_str.is_empty() {
            0
        } else {
            params_str
                .split(',')
                .filter(|p| !p.trim().is_empty())
                .count()
        };
        param_counts.push(param_count as f64);

        let m_end_byte = cap.get(0).unwrap().end();
        // Python: code_no_comments.find('{', m.end() - 1) — m.end() is
        // exclusive end of the full match, which includes the '{'. We need
        // to search for '{' from the start of the brace.
        let search_start_byte = m_end_byte.saturating_sub(1);
        let search_start_char = *byte_to_char.get(search_start_byte).unwrap_or(&chars.len());

        let brace_pos = chars[search_start_char..]
            .iter()
            .position(|c| *c == '{')
            .map(|p| p + search_start_char);
        let Some(brace_pos) = brace_pos else { continue };

        let Some(close_pos) = find_matching_brace(&chars, brace_pos) else {
            continue;
        };
        let body: String = chars[brace_pos + 1..close_pos].iter().collect();
        let body_lines = body.split('\n').filter(|l| !l.trim().is_empty()).count();
        method_lengths.push(body_lines as f64);

        let mut max_depth: i64 = 0;
        let mut depth: i64 = 0;
        for ch in body.chars() {
            if ch == '{' {
                depth += 1;
                if depth > max_depth {
                    max_depth = depth;
                }
            } else if ch == '}' {
                depth -= 1;
            }
        }
        nesting_depths.push(max_depth);
    }

    if !method_lengths.is_empty() {
        fv.avg_method_length = stats::mean(&method_lengths);
        fv.method_length_stddev = if method_lengths.len() >= 2 {
            sample_stdev(&method_lengths)
        } else {
            0.0
        };
    }
    if !param_counts.is_empty() {
        fv.avg_parameter_count = stats::mean(&param_counts);
    }
    if !nesting_depths.is_empty() {
        let nd_f: Vec<f64> = nesting_depths.iter().map(|v| *v as f64).collect();
        fv.avg_nesting_depth = stats::mean(&nd_f);
        fv.max_nesting_depth = *nesting_depths.iter().max().unwrap_or(&0);
    }
}

fn compute_error_handling_features(code_no_comments: &str, fv: &mut StyleFeatureVector) {
    let chars: Vec<char> = code_no_comments.chars().collect();
    let total_lines = code_no_comments.split('\n').count().max(1);

    let try_count = TRY_RE.find_iter(code_no_comments).count();
    fv.try_catch_density = try_count as f64 / total_lines as f64;

    // Collect catch matches as (end_char_pos,) so we can find the subsequent `{`.
    let byte_to_char: Vec<usize> = {
        let mut map = Vec::with_capacity(code_no_comments.len() + 1);
        let mut ci = 0;
        for (bi, _) in code_no_comments.char_indices() {
            while map.len() <= bi {
                map.push(ci);
            }
            ci += 1;
        }
        while map.len() <= code_no_comments.len() {
            map.push(ci);
        }
        map
    };

    let catches: Vec<usize> = CATCH_RE
        .find_iter(code_no_comments)
        .map(|m| m.end().saturating_sub(1))
        .collect();
    if catches.is_empty() {
        return;
    }

    let mut empty_count = 0usize;
    let mut body_lines_vec: Vec<f64> = Vec::new();

    for end_byte in &catches {
        let search_char = *byte_to_char.get(*end_byte).unwrap_or(&chars.len());
        let brace_pos = chars[search_char..]
            .iter()
            .position(|c| *c == '{')
            .map(|p| p + search_char);
        let Some(brace_pos) = brace_pos else { continue };
        let Some(close_pos) = find_matching_brace(&chars, brace_pos) else {
            continue;
        };
        let body: String = chars[brace_pos + 1..close_pos].iter().collect();
        let trimmed_body = body.trim();
        let line_count = trimmed_body
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .count();
        body_lines_vec.push(line_count as f64);
        if line_count == 0 {
            empty_count += 1;
        }
    }

    fv.empty_catch_ratio = empty_count as f64 / catches.len() as f64;
    if !body_lines_vec.is_empty() {
        fv.avg_catch_body_lines = stats::mean(&body_lines_vec);
    }
}

fn compute_import_features(content: &str, fv: &mut StyleFeatureVector) {
    let imports: Vec<String> = IMPORT_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect();
    fv.import_count = imports.len() as i64;
    if imports.is_empty() {
        fv.import_alphabetized = true;
        return;
    }
    fv.wildcard_import_ratio =
        imports.iter().filter(|i| i.ends_with(".*")).count() as f64 / imports.len() as f64;
    let mut sorted = imports.clone();
    sorted.sort();
    fv.import_alphabetized = imports == sorted;
}

fn compute_blank_line_ratio(content: &str, fv: &mut StyleFeatureVector) {
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.is_empty() {
        return;
    }
    let blank = lines.iter().filter(|l| l.trim().is_empty()).count();
    fv.blank_line_ratio = blank as f64 / lines.len() as f64;
}

fn compute_ai_indicator_booleans(
    content: &str,
    code_no_comments: &str,
    fv: &mut StyleFeatureVector,
) {
    let public_methods = PUBLIC_METHOD_RE
        .find_iter(content)
        .filter_map(|r| r.ok())
        .count();
    let javadoc_methods = JAVADOC_METHOD_RE
        .find_iter(content)
        .filter_map(|r| r.ok())
        .count();
    if public_methods > 0 && javadoc_methods >= public_methods {
        fv.has_comprehensive_javadoc = true;
    }

    let method_count = METHOD_RE
        .find_iter(code_no_comments)
        .filter_map(|r| r.ok())
        .count();
    let null_checks = NULL_CHECK_RE.find_iter(code_no_comments).count();
    if method_count > 0 && null_checks >= method_count {
        fv.has_null_checks_everywhere = true;
    }

    let lines: Vec<&str> = content.split('\n').collect();
    let indented_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| !l.is_empty() && l.starts_with(' '))
        .collect();
    if indented_lines.len() >= 5 {
        let indent_sizes: Vec<usize> = indented_lines
            .iter()
            .map(|l| l.chars().take_while(|c| *c == ' ').count())
            .filter(|sz| *sz > 0)
            .collect();
        if !indent_sizes.is_empty() {
            let min_indent = *indent_sizes.iter().min().unwrap();
            if min_indent > 0 {
                let uniform = indent_sizes
                    .iter()
                    .filter(|sz| *sz % min_indent == 0)
                    .count();
                fv.uniform_formatting = uniform as f64 / indent_sizes.len() as f64 > 0.9;
            }
        }
    }
}

pub fn extract_style_features(_file_path: &str, content: &str) -> StyleFeatureVector {
    let mut fv = StyleFeatureVector::default();
    if content.is_empty() || content.trim().is_empty() {
        return fv;
    }
    let code_no_comments = strip_comments(content);
    let identifiers = extract_identifiers(&code_no_comments);

    compute_naming_features(&identifiers, &mut fv);
    compute_comment_features(content, &mut fv);
    compute_method_features(&code_no_comments, &mut fv);
    compute_error_handling_features(&code_no_comments, &mut fv);
    compute_import_features(content, &mut fv);
    compute_blank_line_ratio(content, &mut fv);
    compute_ai_indicator_booleans(content, &code_no_comments, &mut fv);
    fv
}

pub fn compute_ai_style_score(features: &StyleFeatureVector) -> f64 {
    let mut score: f64 = 0.0;
    if features.avg_method_length > 0.0 && features.method_length_stddev < 5.0 {
        score += 0.15;
    }
    if features.import_alphabetized {
        score += 0.10;
    }
    if features.has_comprehensive_javadoc {
        score += 0.15;
    }
    if features.has_null_checks_everywhere {
        score += 0.10;
    }
    if features.comment_density > 0.3 {
        score += 0.10;
    }
    if features.avg_identifier_length > 15.0 {
        score += 0.10;
    }
    if features.avg_catch_body_lines > 3.0 {
        score += 0.10;
    }
    if features.empty_catch_ratio < 0.1 {
        score += 0.10;
    }
    if features.uniform_formatting {
        score += 0.10;
    }
    score.clamp(0.0, 1.0)
}

pub fn compute_deviation_from_baseline(
    features: &StyleFeatureVector,
    baseline_means: &HashMap<String, f64>,
    baseline_stds: &HashMap<String, f64>,
) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    let pairs: [(&str, f64); 18] = [
        ("avg_identifier_length", features.avg_identifier_length),
        (
            "identifier_length_stddev",
            features.identifier_length_stddev,
        ),
        ("camelcase_ratio", features.camelcase_ratio),
        ("screaming_snake_ratio", features.screaming_snake_ratio),
        ("single_char_var_ratio", features.single_char_var_ratio),
        ("comment_density", features.comment_density),
        (
            "avg_comment_length_chars",
            features.avg_comment_length_chars,
        ),
        ("inline_vs_block_ratio", features.inline_vs_block_ratio),
        ("javadoc_ratio", features.javadoc_ratio),
        ("comment_to_code_ratio", features.comment_to_code_ratio),
        ("avg_method_length", features.avg_method_length),
        ("method_length_stddev", features.method_length_stddev),
        ("avg_parameter_count", features.avg_parameter_count),
        ("avg_nesting_depth", features.avg_nesting_depth),
        ("try_catch_density", features.try_catch_density),
        ("empty_catch_ratio", features.empty_catch_ratio),
        ("avg_catch_body_lines", features.avg_catch_body_lines),
        ("blank_line_ratio", features.blank_line_ratio),
    ];
    for (name, value) in pairs {
        let Some(mean) = baseline_means.get(name) else {
            continue;
        };
        let Some(std) = baseline_stds.get(name) else {
            continue;
        };
        let z = if *std > 0.0 {
            (value - mean) / std
        } else {
            0.0
        };
        out.insert(name.to_string(), z);
    }
    out
}

pub fn build_student_baselines(
    conn: &Connection,
    project_id: i64,
    baseline_sprint_id: i64,
) -> rusqlite::Result<()> {
    // Join fingerprints (author attribution) + file_style_features + students.
    // The schema columns here must match Python's stylometry.build_student_baselines.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT f.blame_author_login,
             fsf.file_path, fsf.repo_name,
             fsf.avg_identifier_length, fsf.identifier_length_stddev,
             fsf.camelcase_ratio, fsf.screaming_snake_ratio, fsf.single_char_var_ratio,
             fsf.comment_density, fsf.avg_comment_length_chars,
             fsf.inline_vs_block_ratio, fsf.javadoc_ratio, fsf.comment_to_code_ratio,
             fsf.avg_method_length, fsf.method_length_stddev,
             fsf.avg_parameter_count, fsf.avg_nesting_depth,
             fsf.try_catch_density, fsf.empty_catch_ratio, fsf.avg_catch_body_lines,
             fsf.wildcard_import_ratio, fsf.blank_line_ratio,
             fsf.import_alphabetized
         FROM fingerprints f
         JOIN file_style_features fsf
           ON fsf.file_path = f.file_path
          AND fsf.repo_name = f.repo_full_name
          AND fsf.sprint_id = ?
         JOIN students s ON s.github_login = f.blame_author_login
         WHERE f.sprint_id = ? AND s.team_project_id = ?",
    )?;

    let numeric_field_names: [&str; 18] = [
        "avg_identifier_length",
        "identifier_length_stddev",
        "camelcase_ratio",
        "screaming_snake_ratio",
        "single_char_var_ratio",
        "comment_density",
        "avg_comment_length_chars",
        "inline_vs_block_ratio",
        "javadoc_ratio",
        "comment_to_code_ratio",
        "avg_method_length",
        "method_length_stddev",
        "avg_parameter_count",
        "avg_nesting_depth",
        "try_catch_density",
        "empty_catch_ratio",
        "avg_catch_body_lines",
        "blank_line_ratio",
    ];

    let mut student_files: HashMap<String, Vec<StyleFileEntry>> = HashMap::new();
    let rows = stmt
        .query_map(
            params![baseline_sprint_id, baseline_sprint_id, project_id],
            |r| {
                let login: String = r.get::<_, String>(0)?;
                let mut vals: Vec<Option<f64>> = Vec::with_capacity(18);
                // columns 3..=20 are the 18 numeric features in the same order as numeric_field_names
                // avg_identifier_length=3, identifier_length_stddev=4, camelcase_ratio=5,
                // screaming_snake_ratio=6, single_char_var_ratio=7, comment_density=8,
                // avg_comment_length_chars=9, inline_vs_block_ratio=10, javadoc_ratio=11,
                // comment_to_code_ratio=12, avg_method_length=13, method_length_stddev=14,
                // avg_parameter_count=15, avg_nesting_depth=16, try_catch_density=17,
                // empty_catch_ratio=18, avg_catch_body_lines=19, wildcard_import_ratio=20,
                // blank_line_ratio=21 — but Python's list skips max_*, import_count
                // and only keeps the f64 ones.
                for idx in [
                    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21,
                ] {
                    vals.push(r.get::<_, Option<f64>>(idx)?);
                }
                let alpha: Option<bool> = r.get::<_, Option<bool>>(22)?;
                Ok((login, vals, alpha))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (login, vals, alpha) in rows {
        student_files.entry(login).or_default().push((vals, alpha));
    }

    // Resolve login → student_id (filter to this project).
    let mut login_to_student: HashMap<String, String> = HashMap::new();
    for login in student_files.keys() {
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM students WHERE github_login = ? AND team_project_id = ?",
                params![login, project_id],
                |r| r.get::<_, String>(0),
            )
            .ok();
        if let Some(id) = id {
            login_to_student.insert(login.clone(), id);
        }
    }

    let mut count_written = 0usize;
    for (login, file_rows) in &student_files {
        let Some(student_id) = login_to_student.get(login) else {
            continue;
        };

        // Accumulate values per numeric field.
        let mut accum: Vec<Vec<f64>> = vec![Vec::new(); numeric_field_names.len()];
        let mut alphabetized_values: Vec<bool> = Vec::new();
        for (vals, alpha) in file_rows {
            for (idx, v) in vals.iter().enumerate() {
                if let Some(val) = *v {
                    accum[idx].push(val);
                }
            }
            if let Some(a) = alpha {
                alphabetized_values.push(*a);
            }
        }

        let mut means = JsonMap::new();
        let mut stds = JsonMap::new();
        let mut named_mean: HashMap<&str, f64> = HashMap::new();
        for (idx, name) in numeric_field_names.iter().enumerate() {
            let vals = &accum[idx];
            let (m, s) = if vals.is_empty() {
                (0.0, 0.0)
            } else if vals.len() == 1 {
                (vals[0], 0.0)
            } else {
                (stats::mean(vals), sample_stdev(vals))
            };
            means.insert((*name).into(), serde_json::Value::from(m));
            stds.insert((*name).into(), serde_json::Value::from(s));
            named_mean.insert(*name, m);
        }

        let import_alpha_ratio = if alphabetized_values.is_empty() {
            0.0
        } else {
            alphabetized_values.iter().filter(|b| **b).count() as f64
                / alphabetized_values.len() as f64
        };

        let get_m = |k: &str| -> f64 { *named_mean.get(k).unwrap_or(&0.0) };

        conn.execute(
            "INSERT OR REPLACE INTO student_style_baseline
             (student_id, project_id,
              avg_identifier_length, identifier_length_stddev,
              camelcase_ratio, comment_density,
              avg_method_length, method_length_stddev,
              avg_nesting_depth, try_catch_density,
              import_alphabetized_ratio,
              feature_means, feature_stds, sample_file_count)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                student_id,
                project_id,
                get_m("avg_identifier_length"),
                get_m("identifier_length_stddev"),
                get_m("camelcase_ratio"),
                get_m("comment_density"),
                get_m("avg_method_length"),
                get_m("method_length_stddev"),
                get_m("avg_nesting_depth"),
                get_m("try_catch_density"),
                import_alpha_ratio,
                serde_json::Value::Object(means).to_string(),
                serde_json::Value::Object(stds).to_string(),
                file_rows.len() as i64,
            ],
        )?;
        count_written += 1;
    }

    info!(
        students = count_written,
        project_id, baseline_sprint_id, "style baselines built"
    );
    Ok(())
}

pub fn analyze_repo_stylometry(
    conn: &Connection,
    repo_path: &Path,
    repo_name: &str,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let files: Vec<_> = WalkDir::new(repo_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "java"))
        .collect();

    let mut count = 0;
    for f in files {
        let rel = match f.path().strip_prefix(repo_path) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let rel_str = rel.to_string_lossy().to_string();
        if rel_str.contains("build/") || rel_str.contains(".gradle/") {
            continue;
        }
        let content = match std::fs::read_to_string(f.path()) {
            Ok(c) => c,
            Err(e) => {
                warn!(path = %f.path().display(), error = %e, "cannot read java file");
                continue;
            }
        };
        let fv = extract_style_features(&rel_str, &content);
        conn.execute(
            "INSERT OR REPLACE INTO file_style_features
             (file_path, repo_name, sprint_id,
              avg_identifier_length, identifier_length_stddev,
              camelcase_ratio, screaming_snake_ratio, single_char_var_ratio,
              max_identifier_length,
              comment_density, avg_comment_length_chars,
              inline_vs_block_ratio, javadoc_ratio, comment_to_code_ratio,
              avg_method_length, method_length_stddev,
              avg_parameter_count, avg_nesting_depth, max_nesting_depth,
              try_catch_density, empty_catch_ratio, avg_catch_body_lines,
              import_count, wildcard_import_ratio, import_alphabetized,
              blank_line_ratio,
              has_comprehensive_javadoc, has_null_checks_everywhere,
              uniform_formatting)
             VALUES (?, ?, ?,
                     ?, ?, ?, ?, ?, ?,
                     ?, ?, ?, ?, ?,
                     ?, ?, ?, ?, ?,
                     ?, ?, ?,
                     ?, ?, ?,
                     ?,
                     ?, ?, ?)",
            params![
                rel_str,
                repo_name,
                sprint_id,
                fv.avg_identifier_length,
                fv.identifier_length_stddev,
                fv.camelcase_ratio,
                fv.screaming_snake_ratio,
                fv.single_char_var_ratio,
                fv.max_identifier_length,
                fv.comment_density,
                fv.avg_comment_length_chars,
                fv.inline_vs_block_ratio,
                fv.javadoc_ratio,
                fv.comment_to_code_ratio,
                fv.avg_method_length,
                fv.method_length_stddev,
                fv.avg_parameter_count,
                fv.avg_nesting_depth,
                fv.max_nesting_depth,
                fv.try_catch_density,
                fv.empty_catch_ratio,
                fv.avg_catch_body_lines,
                fv.import_count,
                fv.wildcard_import_ratio,
                fv.import_alphabetized,
                fv.blank_line_ratio,
                fv.has_comprehensive_javadoc,
                fv.has_null_checks_everywhere,
                fv.uniform_formatting,
            ],
        )?;
        count += 1;
    }
    info!(count, repo_name, "stylometry features extracted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comments_removes_all_forms() {
        let src = "// line\nclass A { /* block */ /** javadoc */ int x; }";
        let out = strip_comments(src);
        assert!(!out.contains("line"));
        assert!(!out.contains("block"));
        assert!(!out.contains("javadoc"));
        assert!(out.contains("int x"));
    }

    #[test]
    fn identifier_alphabetized_is_vacuous_when_no_imports() {
        let mut fv = StyleFeatureVector::default();
        compute_import_features("class X {}", &mut fv);
        assert!(fv.import_alphabetized);
        assert_eq!(fv.import_count, 0);
    }

    #[test]
    fn score_rises_with_ai_indicators() {
        let fv = StyleFeatureVector {
            avg_method_length: 10.0,
            method_length_stddev: 1.0,
            import_alphabetized: true,
            has_comprehensive_javadoc: true,
            uniform_formatting: true,
            empty_catch_ratio: 0.0,
            ..Default::default()
        };
        let s = compute_ai_style_score(&fv);
        assert!(s > 0.4);
        assert!(s <= 1.0);
    }

    #[test]
    fn find_matching_brace_balances() {
        let chars: Vec<char> = "{ a { b } c }".chars().collect();
        let close = find_matching_brace(&chars, 0).unwrap();
        assert_eq!(chars[close], '}');
        assert_eq!(close, chars.len() - 1);
    }
}
