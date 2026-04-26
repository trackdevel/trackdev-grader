//! Statement normalization for Java and XML — byte-for-byte parity with
//! `src/survival/normalizer.py`.
//!
//! Uses `fancy-regex` for look-around-based patterns (Rust's stdlib `regex`
//! crate does not support look-around, and we need it to match Python
//! semantics exactly).

use std::collections::BTreeMap;

use fancy_regex::Regex as FancyRegex;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::types::VariableDecl;

// ---- Whitespace ----

static WHITESPACE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").unwrap());

pub fn collapse_whitespace(text: &str) -> String {
    WHITESPACE_RE.replace_all(text, " ").trim().to_string()
}

// ---- Java: string / char literal masking ----

static TEXT_BLOCK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?s)""".*?""""#).unwrap());
static STRING_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#""(?:[^"\\]|\\.)*""#).unwrap());
static CHAR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"'(?:[^'\\]|\\.)*'").unwrap());

fn mask_strings(text: &str) -> String {
    // Text blocks (Java 13+) first — they contain `"""` which would otherwise
    // be chewed up by STRING_RE.
    let t = TEXT_BLOCK_RE.replace_all(text, r#""_STR_""#).into_owned();
    let t = STRING_RE.replace_all(&t, r#""_STR_""#).into_owned();
    CHAR_RE.replace_all(&t, "'_STR_'").into_owned()
}

// ---- Java: numeric literal masking ----

// Hex, binary, decimal integer, and floating-point literals with optional
// type suffixes. `(?<!\w)` / `(?!\w)` ensure digits embedded in identifiers
// (e.g. `_v0`) are not matched — requires look-around, hence fancy-regex.
static NUM_RE: Lazy<FancyRegex> = Lazy::new(|| {
    let pattern = concat!(
        r"(?<!\w)",
        r"(?:",
        r"0[xX][\da-fA-F_]+[lL]?",
        r"|0[bB][01_]+[lL]?",
        r"|\d[\d_]*\.[\d_]*(?:[eE][+-]?\d+)?[fFdDlL]?",
        r"|\.[\d][\d_]*(?:[eE][+-]?\d+)?[fFdD]?",
        r"|\d[\d_]*(?:[eE][+-]?\d+)?[fFdDlL]?",
        r")",
        r"(?!\w)",
    );
    FancyRegex::new(pattern).unwrap()
});

fn is_preserved_num(core: &str) -> bool {
    matches!(core, "0" | "1" | "0.0" | "1.0" | "0." | "1.")
}

fn mask_numerics(text: &str) -> String {
    // fancy-regex's `replace_all` expects a callback; we implement it manually
    // so we can preserve literals like 0 / 1 / 0.0 / 1.0.
    let mut out = String::with_capacity(text.len());
    let mut last_end = 0usize;
    let bytes = text.as_bytes();
    for m in NUM_RE.find_iter(text) {
        let m = match m {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push_str(&text[last_end..m.start()]);
        let slice = &text[m.start()..m.end()];
        let stripped = slice.trim_end_matches(['l', 'L', 'f', 'F', 'd', 'D']);
        let core: String = stripped.chars().filter(|c| *c != '_').collect();
        if is_preserved_num(&core) {
            out.push_str(slice);
        } else {
            out.push_str("_NUM_");
        }
        last_end = m.end();
        let _ = bytes; // silence unused warning in some builds
    }
    out.push_str(&text[last_end..]);
    out
}

// ---- Java: identifier replacement ----

/// Build `{original_name: _vN}` mapping from declarations in source order.
///
/// Input must already be sorted by declaration line (the parser does this).
pub fn build_variable_map(variables: &[VariableDecl]) -> BTreeMap<String, String> {
    let mut var_map: BTreeMap<String, String> = BTreeMap::new();
    let mut idx: usize = 0;
    // Preserve insertion order via a side vector; BTreeMap has deterministic key
    // order, but we need FIRST-SEEN assignment of `_vN`. Use a plain `Vec<(K, V)>`
    // backing with a set of seen names for O(n^2) insertion which is fine at our
    // scale (< 200 vars per method).
    let mut seen: Vec<(String, String)> = Vec::new();
    for v in variables {
        if !seen.iter().any(|(k, _)| k == &v.name) {
            let placeholder = format!("_v{idx}");
            seen.push((v.name.clone(), placeholder.clone()));
            idx += 1;
        }
    }
    for (k, v) in seen {
        var_map.insert(k, v);
    }
    var_map
}

fn escape_for_regex(s: &str) -> String {
    // Mirror Python's `re.escape` for the characters that matter here.
    // `regex`/`fancy-regex` use the same Rust regex syntax which accepts `\X`
    // for any non-alphanumeric character.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

fn replace_identifiers(text: &str, variable_map: &BTreeMap<String, String>) -> String {
    if variable_map.is_empty() {
        return text.to_string();
    }
    // Longer names first so that e.g. "item" is tried before "i".
    let mut names: Vec<&String> = variable_map.keys().collect();
    names.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));

    let alternatives: Vec<String> = names.iter().map(|n| escape_for_regex(n)).collect();
    let pattern = format!(r"(?<![\w.])(?:{})(?![\w(])", alternatives.join("|"));
    let re = match FancyRegex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return text.to_string(),
    };

    let mut out = String::with_capacity(text.len());
    let mut last_end = 0usize;
    for m in re.find_iter(text) {
        let m = match m {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push_str(&text[last_end..m.start()]);
        let original = &text[m.start()..m.end()];
        match variable_map.get(original) {
            Some(repl) => out.push_str(repl),
            None => out.push_str(original),
        }
        last_end = m.end();
    }
    out.push_str(&text[last_end..]);
    out
}

// ---- Java public API ----

pub fn normalize_java_statement(raw_text: &str, variable_map: &BTreeMap<String, String>) -> String {
    let t = collapse_whitespace(raw_text);
    let t = mask_strings(&t);
    let t = mask_numerics(&t);
    replace_identifiers(&t, variable_map)
}

pub fn normalize_imports(import_texts: &[&str]) -> Vec<String> {
    let mut uniq: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for t in import_texts {
        uniq.insert(collapse_whitespace(t));
    }
    uniq.into_iter().collect()
}

// ---- XML normalization ----

static XMLNS_ATTR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\s+xmlns(?::\w+)?\s*=\s*"[^"]*""#).unwrap());
static ATTR_PAIR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"([\w:.-]+)\s*=\s*"([^"]*)""#).unwrap());
static ID_VAL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"@\+id/\w+").unwrap());
static DIM_DP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[\d.]+dp").unwrap());
static DIM_SP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[\d.]+sp").unwrap());
static COLOR_HEX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"#[\da-fA-F]{3,8}").unwrap());
static COLOR_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"@color/\w+").unwrap());
static TAG_OUTER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)^<\s*([\w:.-]+)(.*?)(/?\s*)>$").unwrap());

pub fn normalize_xml_element(raw_text: &str) -> String {
    let t = collapse_whitespace(raw_text);
    let t = XMLNS_ATTR_RE.replace_all(&t, "").into_owned();

    let caps = match TAG_OUTER_RE.captures(&t) {
        Some(c) => c,
        None => return t,
    };
    let tag_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let attrs_str = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    let close = caps.get(3).map(|m| m.as_str()).unwrap_or("");
    let self_close = close.contains('/');

    let mut attrs: Vec<(String, String)> = Vec::new();
    for cap in ATTR_PAIR_RE.captures_iter(attrs_str) {
        let name = cap.get(1).unwrap().as_str();
        let value = cap.get(2).unwrap().as_str();
        if name.starts_with("xmlns") {
            continue;
        }
        let v = ID_VAL_RE.replace_all(value, "@+id/_ID_").into_owned();
        let v = DIM_DP_RE.replace_all(&v, "_DIM_dp").into_owned();
        let v = DIM_SP_RE.replace_all(&v, "_DIM_sp").into_owned();
        let v = COLOR_HEX_RE.replace_all(&v, "_COLOR_").into_owned();
        let v = COLOR_REF_RE.replace_all(&v, "_COLOR_").into_owned();
        attrs.push((name.to_string(), v));
    }
    attrs.sort();

    let attr_str = attrs
        .iter()
        .map(|(n, v)| format!(r#"{n}="{v}""#))
        .collect::<Vec<_>>()
        .join(" ");

    let mut parts = String::new();
    parts.push('<');
    parts.push_str(tag_name);
    if !attr_str.is_empty() {
        parts.push(' ');
        parts.push_str(&attr_str);
    }
    if self_close {
        parts.push_str(" />");
    } else {
        parts.push('>');
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_whitespace_matches_python() {
        assert_eq!(collapse_whitespace("  foo\n\t bar  "), "foo bar");
    }

    #[test]
    fn mask_strings_replaces_literals() {
        assert_eq!(
            mask_strings(r#"System.out.println("hello, world");"#),
            r#"System.out.println("_STR_");"#
        );
        assert_eq!(mask_strings(r"char c = 'a';"), "char c = '_STR_';");
    }

    #[test]
    fn mask_numerics_preserves_zero_one() {
        assert_eq!(mask_numerics("int x = 0;"), "int x = 0;");
        assert_eq!(mask_numerics("int x = 1;"), "int x = 1;");
        assert_eq!(mask_numerics("int x = 42;"), "int x = _NUM_;");
        assert_eq!(mask_numerics("double d = 3.14;"), "double d = _NUM_;");
        assert_eq!(mask_numerics("int x = 0xFF;"), "int x = _NUM_;");
        // Digits embedded in identifiers must not be matched.
        assert_eq!(mask_numerics("var _v0 = 7;"), "var _v0 = _NUM_;");
    }
}
