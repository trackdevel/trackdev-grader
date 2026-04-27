//! Architectural layer + action keyword tables (EN / ES / CA).
//! Mirrors `src/repo_analysis/keywords.py`.

use std::collections::HashSet;

const SPRING_LAYERS: &[(&str, &[&str])] = &[
    (
        "spring_controller",
        &[
            "controller",
            "controllers",
            "rest",
            "restcontroller",
            "endpoint",
            "endpoints",
            "route",
            "routes",
            "controlador",
            "controladora",
            "controladores",
            "ruta",
            "rutes",
            "rutas",
        ],
    ),
    (
        "spring_service",
        &[
            "service",
            "services",
            "servicio",
            "servicios",
            "servei",
            "serveis",
        ],
    ),
    (
        "spring_repository",
        &[
            "repository",
            "repositories",
            "repo",
            "dao",
            "repositorio",
            "repositorios",
            "repositori",
            "repositoris",
        ],
    ),
    (
        "spring_entity",
        &[
            "entity",
            "entities",
            "model",
            "models",
            "entidad",
            "entidades",
            "modelo",
            "modelos",
            "entitat",
            "entitats",
            "model",
        ],
    ),
    (
        "spring_dto_mapper",
        &[
            "dto",
            "dtos",
            "mapper",
            "mappers",
            "converter",
            "converters",
            "serializer",
            "serializers",
        ],
    ),
    (
        "spring_config_security",
        &[
            "security",
            "securityconfig",
            "jwt",
            "auth",
            "authentication",
            "seguridad",
            "seguretat",
            "config",
            "configuration",
            "configuracion",
            "configuracio",
            "filter",
            "filters",
            "filtro",
            "filtros",
            "filtre",
            "filtres",
        ],
    ),
];

const ANDROID_LAYERS: &[(&str, &[&str])] = &[
    ("android_fragment", &["fragment", "fragments"]),
    ("android_layout", &["layout", "layouts", "xml"]),
    (
        "android_viewmodel",
        &["viewmodel", "viewmodels", "livedata", "stateflow", "flow"],
    ),
    (
        "android_recyclerview",
        &[
            "recycler",
            "recyclerview",
            "adapter",
            "adapters",
            "viewholder",
            "viewholders",
        ],
    ),
    (
        "android_retrofit",
        &[
            "retrofit",
            "okhttp",
            "apiclient",
            "apiservice",
            "endpoint",
            "endpoints",
        ],
    ),
    ("android_room", &["room", "dao", "daos"]),
    (
        "android_repository",
        &[
            "repository",
            "repositories",
            "repo",
            "repositorio",
            "repositorios",
            "repositori",
            "repositoris",
        ],
    ),
    (
        "android_activity",
        &[
            "activity",
            "activities",
            "actividad",
            "actividades",
            "activitat",
            "activitats",
        ],
    ),
    (
        "android_navigation",
        &[
            "navigation",
            "navgraph",
            "navcontroller",
            "navbar",
            "drawer",
            "navegacion",
            "navegacio",
        ],
    ),
];

const ACTION_CREATE: &[&str] = &[
    "create",
    "add",
    "new",
    "implement",
    "implementation",
    "introduce",
    "build",
    "crear",
    "anadir",
    "anadido",
    "nuevo",
    "nueva",
    "nuevos",
    "nuevas",
    "implementar",
    "implementacion",
    "afegir",
    "nou",
    "nova",
    "nous",
    "noves",
    "implementacio",
];

const ACTION_MODIFY: &[&str] = &[
    "modify",
    "update",
    "updates",
    "edit",
    "change",
    "changes",
    "refactor",
    "refactoring",
    "fix",
    "fixes",
    "bugfix",
    "hotfix",
    "extend",
    "improve",
    "modificar",
    "modificacion",
    "editar",
    "actualizar",
    "actualizacion",
    "cambiar",
    "cambio",
    "arreglar",
    "corregir",
    "correccion",
    "mejora",
    "mejorar",
    "extender",
    "modificacio",
    "actualitzar",
    "actualitzacio",
    "canviar",
    "canvi",
    "correccio",
    "millora",
    "millorar",
    "ampliar",
];

const STOPWORDS: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "or",
    "of",
    "to",
    "for",
    "in",
    "on",
    "at",
    "with",
    "from",
    "by",
    "is",
    "are",
    "be",
    "it",
    "this",
    "that",
    "as",
    "use",
    "using",
    "make",
    "do",
    "el",
    "la",
    "los",
    "las",
    "un",
    "una",
    "unos",
    "unas",
    "y",
    "o",
    "de",
    "del",
    "al",
    "en",
    "con",
    "por",
    "para",
    "es",
    "son",
    "ser",
    "hacer",
    "usar",
    "els",
    "les",
    "uns",
    "unes",
    "i",
    "amb",
    "per",
    "fer",
    "page",
    "pagina",
    "pagines",
    "screen",
    "pantalla",
    "pantalles",
    "pdf",
    "json",
    "feature",
    "task",
    "story",
    "user",
    "usuario",
    "usuari",
];

const FIX_KEYWORDS: &[&str] = &[
    "fix",
    "fixes",
    "bugfix",
    "hotfix",
    "patch",
    "arregla",
    "arreglar",
    "corrige",
    "corregir",
    "correccion",
    "soluciona",
    "solucionar",
    "arregl",
    "correg",
    "correccio",
    "bug",
];

fn is_stopword(s: &str) -> bool {
    STOPWORDS.contains(&s)
}

/// Lowercase, split on non-alphanumeric, drop stopwords and `< 2`-char tokens.
pub fn tokenize(text: Option<&str>) -> Vec<String> {
    let text = match text {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            cur.push(ch.to_ascii_lowercase());
        } else if !cur.is_empty() {
            let tok = std::mem::take(&mut cur);
            if tok.len() >= 2 && !is_stopword(&tok) {
                out.push(tok);
            }
        }
    }
    if !cur.is_empty() {
        let tok = cur;
        if tok.len() >= 2 && !is_stopword(&tok) {
            out.push(tok);
        }
    }
    out
}

pub fn layer_tags(tokens: &[String], stack: Option<&str>) -> HashSet<String> {
    let table: &[(&str, &[&str])] = match stack {
        Some("spring") => SPRING_LAYERS,
        Some("android") => ANDROID_LAYERS,
        _ => return HashSet::new(),
    };
    let token_set: HashSet<&str> = tokens.iter().map(|s| s.as_str()).collect();
    let mut hits = HashSet::new();
    for (tag, words) in table {
        for w in *words {
            if token_set.contains(w) {
                hits.insert((*tag).to_string());
                break;
            }
        }
    }
    hits
}

pub fn action_tag(tokens: &[String]) -> &'static str {
    let token_set: HashSet<&str> = tokens.iter().map(|s| s.as_str()).collect();
    for w in ACTION_MODIFY {
        if token_set.contains(*w) {
            return "modify";
        }
    }
    // ACTION_CREATE is informational: default branch is `create`.
    let _ = ACTION_CREATE;
    "create"
}

pub fn is_fix_title(title: Option<&str>) -> bool {
    let tokens = tokenize(title);
    for tok in tokens {
        if FIX_KEYWORDS.iter().any(|w| *w == tok) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_lowercases_and_drops_stopwords() {
        let t = tokenize(Some("Add a Login Controller"));
        assert!(t.contains(&"add".to_string()));
        assert!(t.contains(&"login".to_string()));
        assert!(t.contains(&"controller".to_string()));
        assert!(!t.contains(&"a".to_string()));
    }

    #[test]
    fn layer_tags_respects_stack() {
        let tokens = vec!["controller".into(), "endpoint".into()];
        let spring = layer_tags(&tokens, Some("spring"));
        assert!(spring.contains("spring_controller"));
        let android = layer_tags(&tokens, Some("android"));
        // "endpoint" is also an android_retrofit keyword
        assert!(android.contains("android_retrofit"));
        // neutral stack yields nothing
        assert!(layer_tags(&tokens, None).is_empty());
    }

    #[test]
    fn action_defaults_to_create() {
        assert_eq!(action_tag(&["login".into(), "controller".into()]), "create");
    }

    #[test]
    fn action_modify_detected() {
        assert_eq!(action_tag(&["update".into(), "login".into()]), "modify");
        assert_eq!(action_tag(&["arreglar".into()]), "modify");
    }

    #[test]
    fn fix_title_detects_catalan_bug() {
        assert!(is_fix_title(Some("Correg error al login")));
        assert!(!is_fix_title(Some("Add login feature")));
    }
}
