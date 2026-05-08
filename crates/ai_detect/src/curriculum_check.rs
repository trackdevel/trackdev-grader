//! Curriculum-aware static analysis.
//! Mirrors `src/ai_detect/curriculum_check.py`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use tracing::{info, warn};
use walkdir::WalkDir;

use sprint_grader_curriculum::get_allowed_concepts_with_snapshot;

// ── High-severity patterns ──────────────────────────────────────────────────

fn high_patterns() -> &'static [(&'static str, Vec<Regex>)] {
    static PAT: Lazy<Vec<(&'static str, Vec<Regex>)>> = Lazy::new(|| {
        vec![
            (
                "test_framework",
                vec![
                    Regex::new(r"import\s+org\.junit\b").unwrap(),
                    Regex::new(r"import\s+org\.mockito\b").unwrap(),
                    Regex::new(r"import\s+org\.assertj\b").unwrap(),
                    Regex::new(r"import\s+org\.hamcrest\b").unwrap(),
                    Regex::new(r"import\s+org\.testng\b").unwrap(),
                    Regex::new(r"@Test\b").unwrap(),
                    Regex::new(r"@Mock\b").unwrap(),
                    Regex::new(r"@InjectMocks\b").unwrap(),
                    Regex::new(r"@RunWith\b").unwrap(),
                    Regex::new(r"@ExtendWith\b").unwrap(),
                ],
            ),
            (
                "advanced_java",
                vec![
                    Regex::new(r"\.stream\s*\(").unwrap(),
                    Regex::new(r"\.parallelStream\s*\(").unwrap(),
                    Regex::new(r"\.collect\s*\(\s*Collectors\.").unwrap(),
                    Regex::new(r"\.flatMap\s*\(").unwrap(),
                    Regex::new(r"Optional\s*<").unwrap(),
                    Regex::new(r"Optional\.of\b").unwrap(),
                    Regex::new(r"CompletableFuture\b").unwrap(),
                    Regex::new(r"import\s+java\.util\.stream\b").unwrap(),
                    Regex::new(r"import\s+java\.util\.Optional\b").unwrap(),
                ],
            ),
            (
                "dependency_injection",
                vec![
                    Regex::new(r"import\s+javax\.inject\b").unwrap(),
                    Regex::new(r"import\s+com\.google\.inject\b").unwrap(),
                    Regex::new(r"import\s+dagger\b").unwrap(),
                    Regex::new(r"@Inject\b").unwrap(),
                    Regex::new(r"@Provides\b").unwrap(),
                    Regex::new(r"@Module\b").unwrap(),
                    Regex::new(r"@Singleton\b").unwrap(),
                ],
            ),
            (
                "reactive",
                vec![
                    Regex::new(r"import\s+io\.reactivex\b").unwrap(),
                    Regex::new(r"import\s+reactor\b").unwrap(),
                    Regex::new(r"import\s+kotlinx\.coroutines\b").unwrap(),
                    Regex::new(r"Observable\s*<").unwrap(),
                    Regex::new(r"Flowable\s*<").unwrap(),
                    Regex::new(r"Mono\s*<").unwrap(),
                    Regex::new(r"Flux\s*<").unwrap(),
                ],
            ),
        ]
    });
    &PAT
}

fn medium_patterns() -> &'static [(&'static str, Vec<Regex>)] {
    static PAT: Lazy<Vec<(&'static str, Vec<Regex>)>> = Lazy::new(|| {
        vec![
            (
                "design_patterns",
                vec![
                    Regex::new(r"class\s+\w+Factory\b").unwrap(),
                    Regex::new(r"class\s+\w+Builder\b").unwrap(),
                    Regex::new(r"class\s+\w+Singleton\b").unwrap(),
                    Regex::new(r"\.getInstance\s*\(").unwrap(),
                    Regex::new(r"\.newBuilder\s*\(").unwrap(),
                    Regex::new(r"class\s+\w+Adapter\s+extends\b").unwrap(),
                    Regex::new(r"class\s+\w+Decorator\b").unwrap(),
                    Regex::new(r"class\s+\w+Observer\b").unwrap(),
                    Regex::new(r"class\s+\w+Strategy\b").unwrap(),
                ],
            ),
            (
                "error_handling_sophistication",
                vec![
                    Regex::new(
                        r"class\s+\w+Exception\s+extends\s+\w*(?:Exception|RuntimeException|Error)\b",
                    )
                    .unwrap(),
                    Regex::new(
                        r"class\s+\w+Error\s+extends\s+\w*(?:Exception|RuntimeException|Error)\b",
                    )
                    .unwrap(),
                    Regex::new(r"@ExceptionHandler\b").unwrap(),
                    Regex::new(r"@ControllerAdvice\b").unwrap(),
                    Regex::new(r"@ResponseStatus\b").unwrap(),
                ],
            ),
            (
                "external_libraries",
                vec![
                    Regex::new(r"import\s+lombok\b").unwrap(),
                    Regex::new(r"import\s+org\.mapstruct\b").unwrap(),
                    Regex::new(r"import\s+com\.fasterxml\.jackson\b").unwrap(),
                    Regex::new(r"import\s+org\.apache\.commons\b").unwrap(),
                    Regex::new(r"import\s+com\.google\.common\b").unwrap(),
                    Regex::new(r"import\s+io\.swagger\b").unwrap(),
                    Regex::new(r"import\s+org\.modelmapper\b").unwrap(),
                ],
            ),
        ]
    });
    &PAT
}

// ── Regex patterns for concept extraction ───────────────────────────────────

static IMPORT_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*import\s+([\w.*]+)\s*;").unwrap());
static ANNOTATION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"@([A-Z]\w+)\b").unwrap());
static EXTENDS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"class\s+\w+\s+extends\s+(\w+)").unwrap());
static IMPLEMENTS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"class\s+\w+[^{]*implements\s+([\w\s,]+)\s*\{").unwrap());
static METHOD_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\.(\w+)\s*\(").unwrap());

#[derive(Debug, Clone)]
pub struct ConceptEntry {
    pub value: String,
    pub line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct FileConcepts {
    pub imports: Vec<ConceptEntry>,
    pub annotations: Vec<ConceptEntry>,
    pub patterns: Vec<ConceptEntry>,
    pub api_calls: Vec<ConceptEntry>,
}

pub fn extract_file_concepts(content: &str) -> FileConcepts {
    let mut out = FileConcepts::default();
    for (line_idx, line) in content.split('\n').enumerate() {
        let line_no = line_idx + 1;
        if let Some(m) = IMPORT_LINE_RE.captures(line) {
            out.imports.push(ConceptEntry {
                value: m[1].into(),
                line: line_no,
            });
        }
        for m in ANNOTATION_RE.captures_iter(line) {
            out.annotations.push(ConceptEntry {
                value: m[1].into(),
                line: line_no,
            });
        }
        if let Some(m) = EXTENDS_RE.captures(line) {
            out.patterns.push(ConceptEntry {
                value: format!("extends {}", &m[1]),
                line: line_no,
            });
        }
        if let Some(m) = IMPLEMENTS_RE.captures(line) {
            for ifc in m[1].split(',') {
                let ifc = ifc.trim();
                if !ifc.is_empty() {
                    out.patterns.push(ConceptEntry {
                        value: format!("implements {}", ifc),
                        line: line_no,
                    });
                }
            }
        }
        for m in METHOD_CALL_RE.captures_iter(line) {
            let method = &m[1];
            if method.len() > 2 && method.chars().next().is_some_and(|c| c.is_lowercase()) {
                out.api_calls.push(ConceptEntry {
                    value: method.into(),
                    line: line_no,
                });
            }
        }
    }
    out
}

// ── Curriculum comparison ───────────────────────────────────────────────────

fn import_matches_allowed(import_value: &str, allowed: &HashSet<String>) -> bool {
    if allowed.contains(import_value) {
        return true;
    }
    for a in allowed {
        if a.ends_with(".*") {
            let prefix = &a[..a.len() - 1]; // drop '*' → "android.widget."
            if import_value.starts_with(prefix) {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub violation_type: String,
    pub value: String,
    pub line: Option<usize>,
    pub severity: String,
}

fn test_strings_for(entry: &ConceptEntry, ctype: &str) -> Vec<String> {
    let v = entry.value.clone();
    let mut out = vec![v.clone()];
    match ctype {
        "imports" => {
            out.push(format!("import {}", v));
            out.push(format!("import {};", v));
        }
        "annotations" => {
            out.push(format!("@{}", v));
        }
        "api_calls" => {
            out.push(format!(".{}(", v));
            out.push(format!(".{} (", v));
            out.push(format!("{}<", v));
        }
        "patterns" => {
            out.push(format!("class SomeClass {}", v));
            let last_word = v.split_whitespace().last().unwrap_or("");
            out.push(format!("class Foo{} {}", last_word, v));
        }
        _ => {}
    }
    out
}

pub fn check_against_curriculum(
    concepts: &FileConcepts,
    allowed_concepts: &HashMap<String, HashSet<String>>,
) -> Vec<Violation> {
    let mut violations: Vec<Violation> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    let typed: Vec<(&ConceptEntry, &'static str)> = concepts
        .imports
        .iter()
        .map(|e| (e, "imports"))
        .chain(concepts.annotations.iter().map(|e| (e, "annotations")))
        .chain(concepts.patterns.iter().map(|e| (e, "patterns")))
        .chain(concepts.api_calls.iter().map(|e| (e, "api_calls")))
        .collect();

    // HIGH patterns
    for (category, regex_list) in high_patterns() {
        for (entry, ctype) in &typed {
            let test_strs = test_strings_for(entry, ctype);
            for pat in regex_list {
                let mut matched = false;
                for ts in &test_strs {
                    if pat.is_match(ts) {
                        let key = (category.to_string(), entry.value.clone());
                        if !seen.contains(&key) {
                            seen.insert(key);
                            violations.push(Violation {
                                violation_type: category.to_string(),
                                value: entry.value.clone(),
                                line: Some(entry.line),
                                severity: "HIGH".into(),
                            });
                        }
                        matched = true;
                        break;
                    }
                }
                if matched {
                    break;
                }
            }
        }
    }

    for (category, regex_list) in medium_patterns() {
        for (entry, ctype) in &typed {
            let test_strs = test_strings_for(entry, ctype);
            for pat in regex_list {
                let mut matched = false;
                for ts in &test_strs {
                    if pat.is_match(ts) {
                        let key = (category.to_string(), entry.value.clone());
                        if !seen.contains(&key) {
                            seen.insert(key);
                            violations.push(Violation {
                                violation_type: category.to_string(),
                                value: entry.value.clone(),
                                line: Some(entry.line),
                                severity: "MEDIUM".into(),
                            });
                        }
                        matched = true;
                        break;
                    }
                }
                if matched {
                    break;
                }
            }
        }
    }

    // Imports vs allowed
    let empty_set = HashSet::new();
    let allowed_imports = allowed_concepts.get("import").unwrap_or(&empty_set);
    for imp in &concepts.imports {
        let v = &imp.value;
        if v.starts_with("java.lang.") || !v.contains('.') {
            continue;
        }
        if !import_matches_allowed(v, allowed_imports) {
            let key = ("unlisted_import".into(), v.clone());
            if !seen.contains(&key) {
                seen.insert(key);
                violations.push(Violation {
                    violation_type: "unlisted_import".into(),
                    value: v.clone(),
                    line: Some(imp.line),
                    severity: "LOW".into(),
                });
            }
        }
    }

    let allowed_annotations = allowed_concepts.get("annotation").unwrap_or(&empty_set);
    let allowed_features = allowed_concepts
        .get("framework_feature")
        .unwrap_or(&empty_set);
    let common: HashSet<&str> = [
        "Override",
        "Deprecated",
        "SuppressWarnings",
        "FunctionalInterface",
    ]
    .iter()
    .copied()
    .collect();
    for ann in &concepts.annotations {
        let v = &ann.value;
        if common.contains(v.as_str()) {
            continue;
        }
        if !allowed_annotations.contains(v) && !allowed_features.contains(v) {
            let key = ("unlisted_annotation".into(), v.clone());
            if !seen.contains(&key) {
                seen.insert(key);
                violations.push(Violation {
                    violation_type: "unlisted_annotation".into(),
                    value: v.clone(),
                    line: Some(ann.line),
                    severity: "LOW".into(),
                });
            }
        }
    }
    violations
}

// ── Blame-based author attribution ──────────────────────────────────────────

fn git_blame_line(
    repo_path: &Path,
    file_path: &str,
    line_number: usize,
) -> (Option<String>, Option<String>) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("blame")
        .arg("--porcelain")
        .arg("-L")
        .arg(format!("{},{}", line_number, line_number))
        .arg("--")
        .arg(file_path)
        .output();
    let Ok(out) = output else { return (None, None) };
    if !out.status.success() {
        return (None, None);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut email: Option<String> = None;
    let mut sha: Option<String> = None;
    for line in stdout.split('\n') {
        if let Some(rest) = line.strip_prefix("author-mail ") {
            email = Some(
                rest.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .into(),
            );
        } else if !line.starts_with('\t') && line.len() >= 40 {
            let first = line.split_whitespace().next().unwrap_or("");
            if first.len() == 40 {
                sha = Some(first.into());
            }
        }
    }
    (email, sha)
}

fn resolve_author_id(conn: &Connection, author_email: Option<&str>) -> Option<String> {
    let email = author_email?;
    // Sole source of truth: student_github_identity (resolver-derived
    // from task-PR evidence). TrackDev's `students.email` is the school
    // address and almost never matches a git commit email; `github_users`
    // pre-resolved mappings are no longer trusted here either.
    let needle = email.to_lowercase();
    conn.query_row(
        "SELECT student_id FROM student_github_identity
         WHERE identity_kind = 'email' AND identity_value = ?
         ORDER BY weight DESC, confidence DESC, student_id
         LIMIT 1",
        [&needle],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

const SKIP_DIRS: &[&str] = &[
    "build",
    ".gradle",
    ".idea",
    ".git",
    "node_modules",
    "bin",
    "out",
];

fn should_skip(rel: &Path) -> bool {
    rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        SKIP_DIRS.iter().any(|d| s == *d)
    })
}

pub fn scan_repo_curriculum(
    conn: &Connection,
    repo_path: &Path,
    repo_name: &str,
    project_id: i64,
    sprint_id: i64,
    sprint_number: i64,
) -> rusqlite::Result<usize> {
    // T-P2.5: snapshot wins when present (past sprints stay frozen),
    // otherwise fall through to the live curriculum table (active sprint).
    let allowed = get_allowed_concepts_with_snapshot(conn, sprint_id, sprint_number)?;
    let total_allowed: usize = allowed.values().map(|v| v.len()).sum();
    info!(
        repo = repo_name,
        sprint = sprint_number,
        allowed = total_allowed,
        "curriculum scan"
    );

    let mut java_files: Vec<PathBuf> = WalkDir::new(repo_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.extension().is_some_and(|ext| ext == "java"))
        .collect();
    java_files.sort();

    let mut total_violations = 0usize;
    let mut scanned = 0usize;
    for java_file in java_files {
        let rel = match java_file.strip_prefix(repo_path) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        if should_skip(&rel) {
            continue;
        }
        let content = match std::fs::read_to_string(&java_file) {
            Ok(c) => c,
            Err(e) => {
                warn!(path = %java_file.display(), error = %e, "cannot read java file");
                continue;
            }
        };
        scanned += 1;
        let rel_str = rel.to_string_lossy().to_string();
        let concepts = extract_file_concepts(&content);
        let violations = check_against_curriculum(&concepts, &allowed);
        for v in violations {
            let (email, sha) = if let Some(line) = v.line {
                git_blame_line(repo_path, &rel_str, line)
            } else {
                (None, None)
            };
            let author_id = resolve_author_id(conn, email.as_deref());
            conn.execute(
                "INSERT OR REPLACE INTO curriculum_violations
                 (file_path, repo_name, project_id, sprint_id,
                  violation_type, value, line_number, severity,
                  author_id, commit_sha)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    rel_str,
                    repo_name,
                    project_id,
                    sprint_id,
                    v.violation_type,
                    v.value,
                    v.line.map(|l| l as i64),
                    v.severity,
                    author_id,
                    sha,
                ],
            )?;
            total_violations += 1;
        }
    }
    info!(
        repo = repo_name,
        scanned,
        violations = total_violations,
        "curriculum scan done"
    );
    Ok(total_violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_file_concepts_picks_up_imports_and_annotations() {
        let src = "import java.util.List;\n@Override\npublic class Foo extends Bar implements Baz, Qux {\n  list.stream();\n}";
        let c = extract_file_concepts(src);
        assert!(c.imports.iter().any(|e| e.value == "java.util.List"));
        assert!(c.annotations.iter().any(|e| e.value == "Override"));
        assert!(c.patterns.iter().any(|e| e.value == "extends Bar"));
        assert!(c.patterns.iter().any(|e| e.value == "implements Baz"));
        assert!(c.patterns.iter().any(|e| e.value == "implements Qux"));
        assert!(c.api_calls.iter().any(|e| e.value == "stream"));
    }

    #[test]
    fn wildcard_import_matches_prefix() {
        let mut allowed = HashSet::new();
        allowed.insert("android.widget.*".into());
        assert!(import_matches_allowed("android.widget.Button", &allowed));
        assert!(!import_matches_allowed("android.view.View", &allowed));
    }

    #[test]
    fn stream_call_flagged_as_advanced_java() {
        let src = "import java.util.List;\nclass X { void m() { list.stream().collect(Collectors.toList()); } }";
        let c = extract_file_concepts(src);
        let allowed: HashMap<String, HashSet<String>> =
            HashMap::from([("import".into(), HashSet::from(["java.util.List".into()]))]);
        let v = check_against_curriculum(&c, &allowed);
        assert!(v
            .iter()
            .any(|x| x.violation_type == "advanced_java" && x.severity == "HIGH"));
    }
}
