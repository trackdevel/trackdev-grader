//! Byte-exact fingerprint parity against the Python reference.
//!
//! `tests/fixtures_reference.json` is regenerated from `tests/gen_reference.py`
//! whenever the fixtures or the Python reference change. Every hash in this
//! test must match byte-for-byte.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use sprint_grader_survival::{fingerprint_file, parse_file};

fn fixtures_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p
}

#[test]
fn fingerprints_match_python_reference() {
    let dir = fixtures_dir();
    let reference_text = fs::read_to_string(dir.join("fixtures_reference.json"))
        .expect("fixtures_reference.json must exist (run tests/gen_reference.py)");
    let reference: Vec<Value> = serde_json::from_str(&reference_text).expect("reference JSON");

    let mut mismatches: Vec<String> = Vec::new();

    for entry in &reference {
        let file_name = entry["file"].as_str().expect("file");
        let path = dir.join(file_name);
        let source = fs::read(&path).unwrap_or_else(|_| panic!("fixture missing: {file_name}"));

        let parse_result = match parse_file(&source, file_name) {
            Some(r) => r,
            None => {
                if !entry["skipped"].as_bool().unwrap_or(false) {
                    mismatches.push(format!("{file_name}: rust parse returned None"));
                }
                continue;
            }
        };
        let ff = fingerprint_file(&parse_result);

        // Language.
        let ref_lang = entry["language"].as_str().unwrap_or("");
        if ff.language != ref_lang {
            mismatches.push(format!(
                "{file_name}: language mismatch (rust={}, py={ref_lang})",
                ff.language
            ));
        }

        // Statement count.
        let ref_stmt_count = entry["statement_count"].as_u64().unwrap_or(0) as usize;
        if ff.statements.len() != ref_stmt_count {
            mismatches.push(format!(
                "{file_name}: statement count mismatch (rust={}, py={ref_stmt_count})",
                ff.statements.len()
            ));
        }

        // Method count.
        let ref_method_count = entry["method_count"].as_u64().unwrap_or(0) as usize;
        if ff.methods.len() != ref_method_count {
            mismatches.push(format!(
                "{file_name}: method count mismatch (rust={}, py={ref_method_count})",
                ff.methods.len()
            ));
        }

        // Per-statement fingerprints.
        let ref_stmts = entry["statements"].as_array().cloned().unwrap_or_default();
        let limit = ff.statements.len().min(ref_stmts.len());
        for (i, r) in ff.statements.iter().enumerate().take(limit) {
            let p = &ref_stmts[i];
            let py_raw = p["raw_fp"].as_str().unwrap_or("");
            let py_norm = p["normalized_fp"].as_str().unwrap_or("");
            let py_method = p["method"].as_str();
            let rust_method = r.method_name.as_deref();
            if r.raw_fp != py_raw {
                mismatches.push(format!(
                    "{file_name} stmt[{i}]: raw_fp mismatch\n  rust={}\n  py  ={py_raw}",
                    r.raw_fp
                ));
            }
            if r.normalized_fp != py_norm {
                mismatches.push(format!(
                    "{file_name} stmt[{i}]: normalized_fp mismatch\n  rust={}\n  py  ={py_norm}\n  rust_norm_text={:?}\n  py_norm_text  ={:?}",
                    r.normalized_fp,
                    r.normalized_text,
                    p["normalized_text"].as_str().unwrap_or("")
                ));
            }
            if rust_method != py_method {
                mismatches.push(format!(
                    "{file_name} stmt[{i}]: method_name mismatch (rust={:?}, py={:?})",
                    rust_method, py_method
                ));
            }
        }

        // Per-method fingerprints.
        let ref_methods = entry["methods"].as_array().cloned().unwrap_or_default();
        let mlimit = ff.methods.len().min(ref_methods.len());
        for (i, r) in ff.methods.iter().enumerate().take(mlimit) {
            let p = &ref_methods[i];
            let py_name = p["name"].as_str().unwrap_or("");
            let py_fp = p["method_fp"].as_str().unwrap_or("");
            if r.method_name != py_name {
                mismatches.push(format!(
                    "{file_name} method[{i}]: name mismatch (rust={:?}, py={py_name:?})",
                    r.method_name
                ));
            }
            if r.method_fp != py_fp {
                mismatches.push(format!(
                    "{file_name} method[{i}] ({py_name}): method_fp mismatch\n  rust={}\n  py  ={py_fp}",
                    r.method_fp
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "{} fingerprint mismatches:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}
