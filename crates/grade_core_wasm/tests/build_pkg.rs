//! Emit the desktop app's `apps/desktop/pkg` WASM bundle.
//!
//! ```bash
//! BUILD_WASM_PKG=1 cargo test -p grade_core_wasm build_desktop_pkg -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn wasm_bindgen_bin() -> PathBuf {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".cargo/bin/wasm-bindgen"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("wasm-bindgen"))
}

#[test]
#[ignore = "run with BUILD_WASM_PKG=1 to write apps/desktop/pkg"]
fn build_desktop_pkg() {
    if std::env::var("BUILD_WASM_PKG").ok().as_deref() != Some("1") {
        eprintln!("skip: set BUILD_WASM_PKG=1");
        return;
    }

    let root = workspace_root();
    let wasm_artifact = root.join("target/wasm32-unknown-unknown/release/grade_core_wasm.wasm");
    assert!(
        wasm_artifact.is_file(),
        "missing {}; run: cargo build --release --target wasm32-unknown-unknown -p grade_core_wasm",
        wasm_artifact.display()
    );

    let out_dir = root.join("apps/desktop/pkg");
    std::fs::create_dir_all(&out_dir).expect("mkdir pkg");

    let status = Command::new(wasm_bindgen_bin())
        .arg(&wasm_artifact)
        .arg("--out-dir")
        .arg(&out_dir)
        .arg("--target")
        .arg("web")
        .arg("--out-name")
        .arg("grade_core_wasm")
        .status()
        .expect("spawn wasm-bindgen");
    assert!(status.success(), "wasm-bindgen failed");

    assert!(
        out_dir.join("grade_core_wasm.js").is_file(),
        "expected grade_core_wasm.js in {}",
        out_dir.display()
    );
    eprintln!("WASM bundle written to {}", out_dir.display());
}
