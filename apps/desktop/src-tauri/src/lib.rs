use std::path::{Path, PathBuf};

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
}
