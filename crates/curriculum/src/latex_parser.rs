//! LaTeX beamer slide parsing → curriculum concept extraction.
//! Mirrors `src/curriculum/latex_parser.py`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection};
use tracing::{info, warn};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct CurriculumConcept {
    pub category: String,
    pub value: String,
    pub source_file: String,
    pub sprint_taught: Option<i64>,
}

static IMPORT_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"import\s+([\w.*]+)\s*;").unwrap());
static ANNOTATION_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"@([A-Z]\w+)\b").unwrap());
static TEXTTT_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\\(?:texttt|verb\|?|lstinline\|?)\{?([A-Z]\w+(?:\.\w+)*)\}?").unwrap()
});
static CODE_BLOCK_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?s)\\begin\{(?:lstlisting|verbatim|minted)(?:\[.*?\])?\}(.*?)\\end\{(?:lstlisting|verbatim|minted)\}",
    )
    .unwrap()
});
static API_METHOD_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\.(\w+)\s*\(").unwrap());

const DESIGN_PATTERNS: &[&str] = &[
    "Factory",
    "Singleton",
    "Observer",
    "Strategy",
    "Builder",
    "Adapter",
    "Decorator",
    "Facade",
    "Repository",
    "DAO",
    "DTO",
    "MVC",
    "MVVM",
    "MVP",
];

const FRAMEWORK_KEYWORDS: &[&str] = &[
    "LiveData",
    "ViewModel",
    "RecyclerView",
    "Fragment",
    "Activity",
    "Intent",
    "Bundle",
    "SharedPreferences",
    "Room",
    "Retrofit",
    "OkHttp",
    "Glide",
    "Picasso",
    "Navigation",
    "ViewBinding",
    "DataBinding",
    "Coroutine",
    "Flow",
    "Compose",
    "JpaRepository",
    "CrudRepository",
    "RestController",
    "Service",
    "Entity",
    "Repository",
    "Component",
    "Autowired",
    "RequestMapping",
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PathVariable",
    "RequestBody",
    "ResponseEntity",
    "ResponseStatus",
    "Transactional",
    "SpringBootApplication",
    "Configuration",
    "Bean",
    "Profile",
    "Value",
    "CrossOrigin",
];

static PATTERN_RE: Lazy<Regex> = Lazy::new(|| {
    let joined = DESIGN_PATTERNS.join("|");
    Regex::new(&format!(r"(?i)\b({})\s*(?:pattern|patr[oó]n)?\b", joined)).unwrap()
});

static FRAMEWORK_RE: Lazy<Regex> = Lazy::new(|| {
    let joined: Vec<String> = FRAMEWORK_KEYWORDS
        .iter()
        .map(|k| regex::escape(k))
        .collect();
    Regex::new(&format!(r"\b({})\b", joined.join("|"))).unwrap()
});

fn extract_from_code_blocks(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for cap in CODE_BLOCK_PATTERN.captures_iter(content) {
        let block = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        for m in IMPORT_PATTERN.captures_iter(block) {
            out.push(("import".into(), m[1].to_string()));
        }
        for m in ANNOTATION_PATTERN.captures_iter(block) {
            out.push(("annotation".into(), m[1].to_string()));
        }
        for m in API_METHOD_PATTERN.captures_iter(block) {
            let method = &m[1];
            if method.len() > 2 && method.chars().next().is_some_and(|c| c.is_lowercase()) {
                out.push(("api_method".into(), method.to_string()));
            }
        }
    }
    out
}

fn extract_from_inline(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for m in TEXTTT_PATTERN.captures_iter(content) {
        let value = m[1].to_string();
        if value.contains('.') {
            out.push(("import".into(), value));
        } else {
            out.push(("framework_feature".into(), value));
        }
    }
    out
}

fn extract_from_text(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for m in PATTERN_RE.captures_iter(content) {
        out.push(("pattern".into(), m[1].to_string()));
    }
    for m in FRAMEWORK_RE.captures_iter(content) {
        out.push(("framework_feature".into(), m[1].to_string()));
    }
    for m in ANNOTATION_PATTERN.captures_iter(content) {
        let val = m[1].to_string();
        if val != "Override" && val != "Deprecated" && !val.starts_with("begin") {
            out.push(("annotation".into(), val));
        }
    }
    out
}

fn infer_sprint(
    file_path: &Path,
    file_index: usize,
    total_files: usize,
    num_sprints: u32,
) -> Option<i64> {
    let stem = file_path.file_stem()?.to_string_lossy();
    let num_re = Regex::new(r"^(\d+)").unwrap();
    if let Some(m) = num_re.captures(&stem) {
        if let Ok(n) = m[1].parse::<u32>() {
            if n >= 1 && n <= num_sprints {
                return Some(n as i64);
            }
        }
    }
    if total_files <= num_sprints as usize {
        return Some((file_index + 1) as i64);
    }
    let value = ((file_index as u32) * num_sprints) / (total_files as u32) + 1;
    Some(value.min(num_sprints) as i64)
}

pub fn parse_tex_file(file_path: &Path, sprint_taught: Option<i64>) -> Vec<CurriculumConcept> {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            warn!(path = %file_path.display(), error = %e, "cannot read .tex");
            return Vec::new();
        }
    };
    let source = file_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut concepts: Vec<CurriculumConcept> = Vec::new();

    let mut all: Vec<(String, String)> = Vec::new();
    all.extend(extract_from_code_blocks(&content));
    all.extend(extract_from_inline(&content));
    all.extend(extract_from_text(&content));

    for (category, value) in all {
        let key = (category.clone(), value.clone());
        if !seen.contains(&key) {
            seen.insert(key);
            concepts.push(CurriculumConcept {
                category,
                value,
                source_file: source.clone(),
                sprint_taught,
            });
        }
    }
    concepts
}

pub fn parse_all_slides(slides_dir: &Path, num_sprints: u32) -> Vec<CurriculumConcept> {
    let mut tex_files: Vec<PathBuf> = WalkDir::new(slides_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.extension().is_some_and(|ext| ext == "tex"))
        .collect();
    tex_files.sort();

    if tex_files.is_empty() {
        warn!(dir = %slides_dir.display(), "no .tex files found");
        return Vec::new();
    }

    let mut all_concepts: Vec<CurriculumConcept> = Vec::new();
    let mut seen_global: HashSet<(String, String)> = HashSet::new();
    let total = tex_files.len();

    for (i, f) in tex_files.iter().enumerate() {
        let sprint = infer_sprint(f, i, total, num_sprints);
        for c in parse_tex_file(f, sprint) {
            let key = (c.category.clone(), c.value.clone());
            if !seen_global.contains(&key) {
                seen_global.insert(key);
                all_concepts.push(c);
            }
        }
    }

    info!(
        concepts = all_concepts.len(),
        files = total,
        "extracted concepts from .tex files"
    );
    all_concepts
}

pub fn build_curriculum_db(
    conn: &Connection,
    slides_dir: &Path,
    extra_imports: &[String],
    num_sprints: u32,
) -> rusqlite::Result<()> {
    let mut concepts = parse_all_slides(slides_dir, num_sprints);
    for imp in extra_imports {
        concepts.push(CurriculumConcept {
            category: "import".into(),
            value: imp.clone(),
            source_file: "config".into(),
            sprint_taught: None,
        });
    }

    conn.execute("DELETE FROM curriculum_concepts", [])?;
    for c in &concepts {
        conn.execute(
            "INSERT OR IGNORE INTO curriculum_concepts
             (category, value, source_file, sprint_taught)
             VALUES (?, ?, ?, ?)",
            params![c.category, c.value, c.source_file, c.sprint_taught],
        )?;
    }

    let total: i64 =
        conn.query_row("SELECT COUNT(*) FROM curriculum_concepts", [], |r| r.get(0))?;
    info!(concepts = total, "curriculum DB built");
    Ok(())
}

pub fn get_allowed_concepts(
    conn: &Connection,
    sprint_number: i64,
) -> rusqlite::Result<HashMap<String, HashSet<String>>> {
    let mut stmt = conn.prepare(
        "SELECT category, value FROM curriculum_concepts
         WHERE sprint_taught IS NULL OR sprint_taught <= ?",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([sprint_number], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for (cat, val) in rows {
        map.entry(cat).or_default().insert(val);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_imports_from_lstlisting_block() {
        let tex = "\\begin{lstlisting}\nimport java.util.List;\n@Override\n\\end{lstlisting}\n";
        let results = extract_from_code_blocks(tex);
        assert!(results
            .iter()
            .any(|(cat, val)| cat == "import" && val == "java.util.List"));
        assert!(results
            .iter()
            .any(|(cat, val)| cat == "annotation" && val == "Override"));
    }

    #[test]
    fn extracts_framework_keywords_from_prose() {
        let tex = "Use a RestController to handle JpaRepository calls.";
        let results = extract_from_text(tex);
        assert!(results
            .iter()
            .any(|(c, v)| c == "framework_feature" && v == "RestController"));
        assert!(results
            .iter()
            .any(|(c, v)| c == "framework_feature" && v == "JpaRepository"));
    }

    #[test]
    fn texttt_with_dot_is_import() {
        let tex = "\\texttt{Foo.Bar}";
        let r = extract_from_inline(tex);
        assert!(r.iter().any(|(c, v)| c == "import" && v == "Foo.Bar"));
    }
}
