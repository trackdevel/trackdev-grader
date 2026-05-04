//! Single source of truth for the Spanish-language strings the report
//! shows for the static-analysis section. Keeping them here makes a
//! future per-rule i18n pass mechanical: this file is the only place
//! to translate, callers reference constants only. Phase-1: Spanish
//! framing only; rule messages stay in their tool-native language
//! (English, mostly).

pub const SECTION_HEADER: &str = "Análisis estático del código (informativo — no afecta a la nota)";

pub const PER_STUDENT_SUBHEADER: &str = "Por estudiante (atribuido por `git blame`)";

pub const DISCLAIMER: &str = concat!(
    "> Estos avisos son informativos y no afectan a la calificación de la entrega.\n",
    "> Atribución por `git blame -w --ignore-revs-file`: una corrección de typo\n",
    "> de 1 línea sobre un método de 30 líneas pesa ~3 %, no 50 %."
);

pub const NO_FINDINGS: &str = "_Sin hallazgos para este sprint._";

pub const SEVERITY_CRITICAL_PLURAL: &str = "críticos";
pub const SEVERITY_WARNING_PLURAL: &str = "advertencias";
pub const SEVERITY_INFO_PLURAL: &str = "sugerencias";

pub const WEIGHT_LABEL: &str = "peso";
pub const MORE_LABEL: &str = "más";
