//! SHA-256 fingerprints at three levels (raw / normalized / method).
//! Mirrors `src/survival/fingerprint.py`.

use sha2::{Digest, Sha256};

use crate::normalizer::{
    build_variable_map, collapse_whitespace, normalize_java_statement, normalize_xml_element,
};
use crate::types::{Method, ParseResult};

// ---- Core hashing ----

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn fingerprint_raw(text: &str) -> String {
    sha256_hex(&collapse_whitespace(text))
}

pub fn fingerprint_normalized(text: &str) -> String {
    sha256_hex(text)
}

pub fn fingerprint_method(statement_fingerprints: &[String]) -> String {
    sha256_hex(&statement_fingerprints.join("\n"))
}

// ---- Per-statement results ----

#[derive(Debug, Clone)]
pub struct StatementFingerprint {
    pub file_path: String,
    pub statement_index: usize,
    pub method_name: Option<String>,
    pub raw_text: String,
    pub raw_fp: String,
    pub normalized_text: String,
    pub normalized_fp: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone)]
pub struct MethodFingerprint {
    pub method_name: String,
    pub method_fp: String,
    pub statement_fps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FileFingerprints {
    pub file_path: String,
    pub language: String,
    pub statements: Vec<StatementFingerprint>,
    pub methods: Vec<MethodFingerprint>,
}

// ---- Per-method helpers ----

fn fingerprint_java_method(
    method: &Method,
    file_path: &str,
    start_index: usize,
) -> (Vec<StatementFingerprint>, MethodFingerprint) {
    let var_map = build_variable_map(&method.variables);
    let mut stmt_fps: Vec<StatementFingerprint> = Vec::new();
    let mut normalized_digests: Vec<String> = Vec::new();

    for (i, stmt) in method.statements.iter().enumerate() {
        let raw_fp = fingerprint_raw(&stmt.raw_text);
        let normalized_text = normalize_java_statement(&stmt.raw_text, &var_map);
        let norm_fp = fingerprint_normalized(&normalized_text);
        stmt_fps.push(StatementFingerprint {
            file_path: file_path.to_string(),
            statement_index: start_index + i,
            method_name: Some(method.name.clone()),
            raw_text: stmt.raw_text.clone(),
            raw_fp,
            normalized_text,
            normalized_fp: norm_fp.clone(),
            start_line: stmt.start_line,
            end_line: stmt.end_line,
        });
        normalized_digests.push(norm_fp);
    }

    let method_fp = if normalized_digests.is_empty() {
        String::new()
    } else {
        fingerprint_method(&normalized_digests)
    };
    (
        stmt_fps,
        MethodFingerprint {
            method_name: method.name.clone(),
            method_fp,
            statement_fps: normalized_digests,
        },
    )
}

fn fingerprint_xml_method(
    method: &Method,
    file_path: &str,
    start_index: usize,
) -> (Vec<StatementFingerprint>, MethodFingerprint) {
    let mut stmt_fps: Vec<StatementFingerprint> = Vec::new();
    let mut normalized_digests: Vec<String> = Vec::new();

    for (i, stmt) in method.statements.iter().enumerate() {
        let raw_fp = fingerprint_raw(&stmt.raw_text);
        let normalized_text = normalize_xml_element(&stmt.raw_text);
        let norm_fp = fingerprint_normalized(&normalized_text);
        stmt_fps.push(StatementFingerprint {
            file_path: file_path.to_string(),
            statement_index: start_index + i,
            method_name: Some(method.name.clone()),
            raw_text: stmt.raw_text.clone(),
            raw_fp,
            normalized_text,
            normalized_fp: norm_fp.clone(),
            start_line: stmt.start_line,
            end_line: stmt.end_line,
        });
        normalized_digests.push(norm_fp);
    }

    let method_fp = if normalized_digests.is_empty() {
        String::new()
    } else {
        fingerprint_method(&normalized_digests)
    };
    (
        stmt_fps,
        MethodFingerprint {
            method_name: method.name.clone(),
            method_fp,
            statement_fps: normalized_digests,
        },
    )
}

// ---- Public API ----

pub fn fingerprint_file(parse_result: &ParseResult) -> FileFingerprints {
    let mut result = FileFingerprints {
        file_path: parse_result.file_path.clone(),
        language: parse_result.language.clone(),
        statements: Vec::new(),
        methods: Vec::new(),
    };

    let is_java = parse_result.language == "java";
    let mut index = 0usize;

    // Class-level statements — no method context.
    for stmt in &parse_result.class_level_statements {
        let raw_fp = fingerprint_raw(&stmt.raw_text);
        let normalized_text = if is_java {
            normalize_java_statement(&stmt.raw_text, &std::collections::BTreeMap::new())
        } else {
            normalize_xml_element(&stmt.raw_text)
        };
        let norm_fp = fingerprint_normalized(&normalized_text);
        result.statements.push(StatementFingerprint {
            file_path: parse_result.file_path.clone(),
            statement_index: index,
            method_name: None,
            raw_text: stmt.raw_text.clone(),
            raw_fp,
            normalized_text,
            normalized_fp: norm_fp,
            start_line: stmt.start_line,
            end_line: stmt.end_line,
        });
        index += 1;
    }

    // Methods — each gets its own variable map.
    for method in &parse_result.methods {
        let (stmt_fps, method_fp) = if is_java {
            fingerprint_java_method(method, &parse_result.file_path, index)
        } else {
            fingerprint_xml_method(method, &parse_result.file_path, index)
        };
        let added = stmt_fps.len();
        result.statements.extend(stmt_fps);
        if !method_fp.method_fp.is_empty() {
            result.methods.push(method_fp);
        }
        index += added;
    }

    result
}
