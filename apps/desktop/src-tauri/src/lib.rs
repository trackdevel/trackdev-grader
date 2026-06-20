use std::path::{Path, PathBuf};

use tauri::Manager;

const LAST_SESSION_FILENAME: &str = "last-session.json";

fn last_session_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(LAST_SESSION_FILENAME))
}

#[tauri::command]
fn get_cwd() -> Result<String, String> {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn resolve_stored_path(config_path: String, stored: String) -> Result<String, String> {
    let stored_path = Path::new(&stored);
    if stored_path.is_absolute() {
        return Ok(stored);
    }
    let base = config_parent(&config_path)?;
    Ok(base.join(stored_path).to_string_lossy().into_owned())
}

#[tauri::command]
fn relativize_path(config_path: String, absolute: String) -> Result<String, String> {
    let abs = Path::new(&absolute);
    let base = config_parent(&config_path)?;
    let abs_canon = abs.canonicalize().unwrap_or_else(|_| abs.to_path_buf());
    let base_canon = base.canonicalize().unwrap_or(base);
    if let Ok(rel) = abs_canon.strip_prefix(&base_canon) {
        return Ok(rel.to_string_lossy().into_owned());
    }
    Ok(abs_canon.to_string_lossy().into_owned())
}

#[tauri::command]
fn join_path(base_dir: String, file_name: String) -> Result<String, String> {
    Ok(Path::new(&base_dir)
        .join(file_name)
        .to_string_lossy()
        .into_owned())
}

#[tauri::command]
fn parent_dir(path: String) -> Result<String, String> {
    config_parent(&path).map(|p| p.to_string_lossy().into_owned())
}

fn config_parent(config_path: &str) -> Result<PathBuf, String> {
    Path::new(config_path)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("config path has no parent: {config_path}"))
}

/// Open an http(s) URL in the user's default browser. The webview would
/// otherwise navigate away from the app on a plain `<a href>` click, so the
/// frontend intercepts task/PR links and routes them here. Restricted to
/// http(s) so it can never shell-execute an arbitrary string.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("refusing to open non-http url: {url}"));
    }
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}

/// Payload for [`export_grade_xlsx`]: the desktop's WASM-computed grades for one
/// project, plus the destination path chosen via the save/folder dialog.
#[derive(serde::Deserialize)]
struct GradeExportPayload {
    out_path: String,
    project_name: String,
    /// `student_id → full_name`; a missing id falls back to the id in the sheet.
    names: std::collections::BTreeMap<String, String>,
    grades: grade_core::ProjectGrades,
    decimals: u32,
}

/// Persist (or clear) the desktop's last-loaded session snapshot in app data.
#[tauri::command]
fn write_last_session(app: tauri::AppHandle, payload: Option<String>) -> Result<(), String> {
    let path = last_session_path(&app)?;
    match payload {
        None => {
            if path.is_file() {
                std::fs::remove_file(path).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Some(text) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, text).map_err(|e| e.to_string())
        }
    }
}

#[tauri::command]
fn read_last_session(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let path = last_session_path(&app)?;
    if !path.is_file() {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Write a student-facing final-grade workbook for one project. The grades are
/// computed in the webview (WASM, live spec) and handed here verbatim, so the
/// file matches exactly what the professor sees on screen. Shares the writer
/// with the CLI's `grade-xlsx`, so both surfaces produce identical layouts.
#[tauri::command]
fn export_grade_xlsx(payload: GradeExportPayload) -> Result<(), String> {
    grade_xlsx::write_grade_workbook(
        Path::new(&payload.out_path),
        &payload.project_name,
        &payload.names,
        &payload.grades,
        payload.decimals,
    )
    .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_sql::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_cwd,
            resolve_stored_path,
            relativize_path,
            join_path,
            parent_dir,
            open_external,
            write_last_session,
            read_last_session,
            export_grade_xlsx,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("grader_desktop_test_{n}"));
        fs::create_dir_all(&dir).expect("tmpdir");
        dir
    }

    #[test]
    fn resolve_stored_path_joins_relative_to_config_parent() {
        let dir = tmp_dir();
        let config = dir.join("grader.desktop.json");
        let resolved = resolve_stored_path(
            config.to_string_lossy().into_owned(),
            "data/grading.db".into(),
        )
        .expect("resolve");
        assert_eq!(resolved, dir.join("data/grading.db").to_string_lossy());
    }

    #[test]
    fn resolve_stored_path_keeps_absolute() {
        let abs: String = if cfg!(windows) {
            r"C:\tmp\grading.db".into()
        } else {
            "/tmp/grading.db".into()
        };
        let resolved =
            resolve_stored_path("/any/grader.desktop.json".into(), abs.clone()).expect("resolve");
        assert_eq!(resolved, abs);
    }

    #[test]
    fn parent_dir_returns_containing_directory() {
        let dir = tmp_dir();
        let file = dir.join("grader.desktop.json");
        let parent = parent_dir(file.to_string_lossy().into_owned()).expect("parent");
        assert_eq!(parent, dir.to_string_lossy());
    }

    #[test]
    fn export_grade_xlsx_writes_workbook_from_payload() {
        let dir = tmp_dir();
        let out_path = dir.join("notes_team-01.xlsx");
        // Mirrors the JSON the webview sends: project name + names + the
        // ProjectGrades slice of a WASM GradeOutput.
        let json = serde_json::json!({
            "out_path": out_path.to_string_lossy(),
            "project_name": "Team 01",
            "names": { "alice": "Alice Liddell" },
            "decimals": 2,
            "grades": {
                "project_id": 1,
                "quality_grade": 7.5,
                "quality_penalized": 7.5,
                "project_penalty": 0.0,
                "ai_factor": 1.0,
                "project_final": 7.5,
                "team_size": 1,
                "axes": [],
                "students": [{
                    "student_id": "alice",
                    "raw_points": 10.0,
                    "effective_points": 8.0,
                    "ai_keep": 0.8,
                    "contribution": 1.0,
                    "base_grade": 6.0,
                    "student_penalty": 0.5,
                    "ai_undeclared_count": 2,
                    "student_final": 5.5
                }]
            }
        });
        let payload: GradeExportPayload = serde_json::from_value(json).expect("payload");
        export_grade_xlsx(payload).expect("export");
        assert!(out_path.is_file());
        assert!(fs::metadata(&out_path).unwrap().len() > 0);
    }
}
