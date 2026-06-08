//! Assemble the single-file, offline `grading.html`: vendored JS inlined, the
//! wasm + snapshot base64-embedded, and the default knob vector seeded from
//! config. Substitution is by unique sentinel comments so no asset can be
//! confused with another. The emitted page references NO external URLs — the
//! wasm is passed to sql.js as `wasmBinary`.

use anyhow::Result;
use base64::Engine as _;
use sprint_grader_grading_xlsx::GradingConfig;

const TEMPLATE: &str = include_str!("../assets/template.html");
const APP_CSS: &str = include_str!("../assets/app.css");
const APP_JS: &str = include_str!("../assets/app.js");
const ENGINE_JS: &str = include_str!("../assets/engine.js");
const SQL_WASM_JS: &str = include_str!("../assets/sql-wasm.js");
const MATHJS: &str = include_str!("../assets/mathjs.min.js");
const SQL_WASM: &[u8] = include_bytes!("../assets/sql-wasm.wasm");

/// Build the complete page string from a snapshot and the grading config.
pub fn render_html(snapshot: &[u8], cfg: &GradingConfig) -> Result<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let html = TEMPLATE
        .replace("/*__APP_CSS__*/", APP_CSS)
        .replace("/*__SQL_WASM_JS__*/", SQL_WASM_JS)
        .replace("/*__MATHJS__*/", MATHJS)
        .replace("/*__ENGINE_JS__*/", ENGINE_JS)
        .replace("/*__APP_JS__*/", APP_JS)
        .replace("/*__DEFAULT_KNOBS_JSON__*/", &default_knobs_json(cfg)?)
        .replace("/*__SQL_WASM_B64__*/", &b64.encode(SQL_WASM))
        .replace("/*__SNAPSHOT_B64__*/", &b64.encode(snapshot));
    Ok(html)
}

/// The default knob vector (25 scalars + `penalty_mode` + meta + model/level
/// maps) as JSON, matching `engine.js::knobsFromTables` and the snapshot
/// `weights` rows. Seeds the panel and the "Reset" baseline.
fn default_knobs_json(cfg: &GradingConfig) -> Result<String> {
    let w = &cfg.weights_project;
    let a = &cfg.ai_usage;
    let p = &cfg.penalty;
    let n = &cfg.normalization;
    let scalars: [(&str, f64); 25] = [
        ("w_doc", w.documentation),
        ("w_cq", w.code_quality),
        ("w_surv", w.survival),
        ("w_arch", w.architecture),
        ("ai_strength", a.strength),
        ("floor_keep", a.floor_keep),
        ("undeclared_model_m", a.undeclared_model_m),
        ("undeclared_level_l", a.undeclared_level_l),
        ("max_penalty_points", p.max_penalty_points),
        ("student_penalty_cap", p.student_penalty_cap),
        ("crit_sa_points", p.crit_sa_points),
        ("crit_cx_points", p.crit_cx_points),
        ("crit_flag_points", p.crit_flag_points),
        ("security_extra", p.security_extra),
        ("doc_max", n.doc_max),
        ("mi_floor", n.mi_floor),
        ("mi_ceiling", n.mi_ceiling),
        ("cc_penalty", n.cc_penalty),
        ("test_bonus", n.test_bonus),
        ("test_cap", n.test_cap),
        ("surv_floor", n.surv_floor),
        ("surv_ceiling", n.surv_ceiling),
        ("k_crit", n.k_crit),
        ("k_warn", n.k_warn),
        ("arch_norm", n.arch_norm),
    ];
    let mut o = serde_json::Map::new();
    for (k, v) in scalars {
        o.insert(k.to_string(), serde_json::json!(v));
    }
    o.insert(
        "penalty_mode".into(),
        serde_json::Value::String(p.mode.clone()),
    );
    o.insert("decimals".into(), serde_json::json!(cfg.output.decimals));
    o.insert(
        "quantize_final".into(),
        serde_json::json!(cfg.output.quantize_final),
    );
    o.insert("models".into(), serde_json::to_value(&a.models)?);
    o.insert("levels".into(), serde_json::to_value(&a.levels)?);
    Ok(serde_json::to_string(&serde_json::Value::Object(o))?)
}
