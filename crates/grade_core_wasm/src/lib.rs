use grade_core::{CohortGradeOutput, Expr, GradeOutput, GradeSpec, RawProject, StructuralOutput};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn structural_scopes(raw_json: &str, spec_json: &str) -> Result<JsValue, JsError> {
    let raw: RawProject = serde_json::from_str(raw_json)
        .map_err(|e| JsError::new(&format!("invalid raw JSON: {e}")))?;
    let spec: GradeSpec = serde_json::from_str(spec_json)
        .map_err(|e| JsError::new(&format!("invalid spec JSON: {e}")))?;
    let scopes = grade_core::structural_scopes(&raw, &spec);
    serde_wasm_bindgen::to_value(&StructuralOutput { scopes })
        .map_err(|e| JsError::new(&format!("serialize output: {e}")))
}

#[wasm_bindgen]
pub fn grade_cohort(projects_json: &str, spec_json: &str) -> Result<JsValue, JsError> {
    let projects: Vec<RawProject> = serde_json::from_str(projects_json)
        .map_err(|e| JsError::new(&format!("invalid projects JSON: {e}")))?;
    let spec: GradeSpec = serde_json::from_str(spec_json)
        .map_err(|e| JsError::new(&format!("invalid spec JSON: {e}")))?;
    let out: CohortGradeOutput =
        grade_core::grade_cohort(&projects, &spec).map_err(|e| JsError::new(&e.to_string()))?;
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsError::new(&format!("serialize output: {e}")))
}

#[wasm_bindgen]
pub fn grade(raw_json: &str, spec_json: &str) -> Result<JsValue, JsError> {
    let raw: RawProject = serde_json::from_str(raw_json)
        .map_err(|e| JsError::new(&format!("invalid raw JSON: {e}")))?;
    let spec: GradeSpec = serde_json::from_str(spec_json)
        .map_err(|e| JsError::new(&format!("invalid spec JSON: {e}")))?;
    if spec.formulas.task.is_empty() && spec.formulas.project.is_empty() {
        let scopes = grade_core::structural_scopes(&raw, &spec);
        return serde_wasm_bindgen::to_value(&StructuralOutput { scopes })
            .map_err(|e| JsError::new(&format!("serialize output: {e}")));
    }
    let out: GradeOutput =
        grade_core::grade(&raw, &spec).map_err(|e| JsError::new(&e.to_string()))?;
    serde_wasm_bindgen::to_value(&out).map_err(|e| JsError::new(&format!("serialize output: {e}")))
}

#[wasm_bindgen]
pub fn free_vars(expr_json: &str) -> Result<JsValue, JsError> {
    let expr: Expr = serde_json::from_str(expr_json)
        .map_err(|e| JsError::new(&format!("invalid expr JSON: {e}")))?;
    let vars: Vec<String> = grade_core::free_vars(&expr).into_iter().collect();
    serde_wasm_bindgen::to_value(&vars).map_err(|e| JsError::new(&format!("serialize: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grade_core::{structural_scopes, StructuralSpec};

    #[test]
    fn grade_cohort_native() {
        let raw: RawProject = serde_json::from_str(
            r#"{
            "project_id": 1,
            "name": "x",
            "team_size": 2,
            "axis": {
                "documentation_raw": 3, "doc_present": true,
                "code_quality_raw": 90, "cc_pct": 0, "mutation_score": 0, "cq_present": true,
                "survival_raw": 0, "surv_present": false,
                "arch_crit_count": 0, "arch_warn_count": 0, "arch_present": false
            },
            "inventory": [{"repo_full_name":"r","metrics":{"production_loc":1000.0}}],
            "tasks": [],
            "students": [],
            "crit_findings": [],
            "student_flags": []
        }"#,
        )
        .unwrap();
        let spec_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/grading.standard.json");
        let spec: GradeSpec =
            serde_json::from_str(&std::fs::read_to_string(spec_path).unwrap()).unwrap();
        let out = grade_core::grade_cohort(&[raw], &spec).unwrap();
        assert_eq!(out.projects.len(), 1);
        assert!(out.bounds.metrics.contains_key("code_quality_raw"));
    }

    #[test]
    fn grade_roundtrip_native() {
        let raw: RawProject = serde_json::from_str(
            r#"{
            "project_id": 1,
            "name": "x",
            "team_size": 0,
            "axis": {
                "documentation_raw": 0, "doc_present": false,
                "code_quality_raw": 0, "cc_pct": 0, "mutation_score": 0, "cq_present": false,
                "survival_raw": 0, "surv_present": false,
                "arch_crit_count": 0, "arch_warn_count": 0, "arch_present": false
            },
            "tasks": [],
            "students": [],
            "crit_findings": [],
            "student_flags": []
        }"#,
        )
        .unwrap();
        let spec: StructuralSpec = serde_json::from_str(
            r#"{"meta":{"penalty_mode":"subtractive"},"weights":{"ai_strength":1,"floor_keep":0.2},"models":{},"levels":{}}"#,
        )
        .unwrap();
        let scopes = structural_scopes(&raw, &spec);
        assert!((scopes.ai_factor - 1.0).abs() < 1e-9);
    }
}
