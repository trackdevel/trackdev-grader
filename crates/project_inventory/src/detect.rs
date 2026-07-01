//! EXTRA_TECH depth detectors (Layer B).
//!
//! Static AST detection of curated "advanced" features, calibrated against
//! the real cohort under `data/entregues`. Produces numeric metric keys (for
//! `repo_structural_metrics` + the `extra_tech` aggregate) and itemized
//! [`FeatureFinding`]s (for `repo_extra_technologies` / the report + desktop).
//!
//! All detection is pure static (no LLM). Where a signal needs cross-file
//! reasoning (the FCM-Spring endpoint call graph; an FCM-Android receiver that
//! stores into Room) we use a bounded, name-based best-effort traversal — the
//! limits are documented at each site.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use sprint_grader_architecture::scanner::ScannedFile;
use tree_sitter::Node;

use crate::catalog::Stack;
use crate::inventory::is_production_main_source;
use crate::metrics;

/// One detected advanced feature, for itemization in `repo_extra_technologies`.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureFinding {
    pub technology: String,
    pub category: String,
    pub evidence: String,
    pub depth: f64,
}

/// Output of the depth pass: every EXTRA_TECH numeric key (zero-filled) plus the
/// itemized findings.
#[derive(Debug, Clone, Default)]
pub struct DepthScan {
    pub metrics: BTreeMap<String, f64>,
    pub features: Vec<FeatureFinding>,
}

const FCM_SEND_NAMES: &[&str] = &[
    "send",
    "sendAsync",
    "sendEach",
    "sendEachAsync",
    "sendEachForMulticast",
    "sendEachForMulticastAsync",
    "sendMulticast",
    "sendMulticastAsync",
    "sendAll",
    "sendAllAsync",
];

const MAPPING_ANNOTATIONS: &[&str] = &[
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PatchMapping",
    "RequestMapping",
];

/// Callee names on a Dao/Repository that are token plumbing, not message storage.
const TOKEN_CALL_HINTS: &[&str] = &[
    "register",
    "registerpushdevice",
    "updatefcmtoken",
    "savetoken",
    "updatetoken",
    "subscribe",
    "unsubscribe",
    "sendtoken",
];

const AV_AUDIO_TYPES: &[&str] = &["MediaPlayer", "SoundPool", "MediaRecorder", "AudioManager"];
const AV_VIDEO_TYPES: &[&str] = &["VideoView", "ExoPlayer", "SimpleExoPlayer", "PlayerView"];

/// Run every depth detector over a repo's parsed files. `stack` gates the
/// Android-only vs Spring-only detectors.
pub fn detect_depth(files: &[ScannedFile], stack: Stack) -> DepthScan {
    let prod: Vec<&ScannedFile> = files
        .iter()
        .filter(|f| is_production_main_source(&f.rel_path))
        .collect();

    let mut metrics: BTreeMap<String, f64> = metrics::EXTRA_TECH_KEYS
        .iter()
        // extra_dependency_count is a Layer-A (gradle) key; the depth pass does
        // not own it, but zero-fill so the map is total.
        .map(|k| ((*k).to_string(), 0.0))
        .collect();
    let mut features = Vec::new();

    if stack == Stack::Spring {
        let fcm = detect_fcm_spring(&prod);
        metrics.insert(metrics::FCM_SEND_CALL_COUNT.into(), fcm.send_calls as f64);
        metrics.insert(
            metrics::FCM_SENDING_ENDPOINT_COUNT.into(),
            fcm.sending_endpoints as f64,
        );
        if fcm.send_calls > 0 {
            features.push(FeatureFinding {
                technology: "Firebase Cloud Messaging (Spring send)".into(),
                category: "fcm".into(),
                evidence: fcm.evidence.unwrap_or_default(),
                depth: fcm.send_calls as f64,
            });
        }

        let spec = detect_specifications(&prod);
        metrics.insert(
            metrics::SPEC_EXECUTOR_REPO_COUNT.into(),
            spec.exec_repos as f64,
        );
        metrics.insert(
            metrics::SPECIFICATION_DEF_COUNT.into(),
            spec.spec_defs as f64,
        );
        if spec.exec_repos > 0 || spec.spec_defs > 0 {
            features.push(FeatureFinding {
                technology: "Spring Data Specifications".into(),
                category: "specifications".into(),
                evidence: spec.evidence.unwrap_or_default(),
                depth: spec.spec_defs.max(spec.exec_repos) as f64,
            });
        }

        let email = detect_email(&prod);
        metrics.insert(
            metrics::EMAIL_SEND_SITE_COUNT.into(),
            email.send_sites as f64,
        );
        if email.send_sites > 0 {
            features.push(FeatureFinding {
                technology: "Email (JavaMailSender)".into(),
                category: "email".into(),
                evidence: email.evidence.unwrap_or_default(),
                depth: email.send_sites as f64,
            });
        }

        let okhttp = detect_okhttp_external_apis(&prod);
        metrics.insert(
            metrics::OKHTTP_EXTERNAL_API_COUNT.into(),
            okhttp.api_count as f64,
        );
        if okhttp.api_count > 0 {
            features.push(FeatureFinding {
                technology: "External API (OkHttp)".into(),
                category: "external_api".into(),
                evidence: okhttp.evidence.unwrap_or_default(),
                depth: okhttp.api_count as f64,
            });
        }

        let minio = detect_minio_object_io(&prod);
        metrics.insert(
            metrics::MINIO_OBJECT_IO_SITE_COUNT.into(),
            minio.io_sites as f64,
        );
        if minio.io_sites > 0 {
            features.push(FeatureFinding {
                technology: "MinIO object storage".into(),
                category: "storage".into(),
                evidence: minio.evidence.unwrap_or_default(),
                depth: minio.io_sites as f64,
            });
        }
    }

    if stack == Stack::Android {
        let fcm = detect_fcm_android(&prod);
        metrics.insert(metrics::FCM_ANDROID_ROOM_STORE.into(), fcm.score as f64);
        if fcm.score > 0 {
            features.push(FeatureFinding {
                technology: "FCM stored in Room (Android)".into(),
                category: "fcm".into(),
                evidence: fcm.evidence.unwrap_or_default(),
                depth: fcm.score as f64,
            });
        }

        let gfx = detect_graphics(&prod);
        metrics.insert(metrics::GRAPHICS_CUSTOM_DRAW_COUNT.into(), gfx.count as f64);
        if gfx.count > 0 {
            features.push(FeatureFinding {
                technology: "Custom graphics drawing".into(),
                category: "graphics".into(),
                evidence: gfx.evidence.unwrap_or_default(),
                depth: gfx.count as f64,
            });
        }

        let av = detect_av(&prod);
        metrics.insert(metrics::AV_USAGE_COUNT.into(), (av.audio + av.video) as f64);
        if av.video > 0 {
            features.push(FeatureFinding {
                technology: "Video (Android)".into(),
                category: "av".into(),
                evidence: av.video_evidence.unwrap_or_default(),
                depth: av.video as f64,
            });
        }
        if av.audio > 0 {
            features.push(FeatureFinding {
                technology: "Audio (Android)".into(),
                category: "av".into(),
                evidence: av.audio_evidence.unwrap_or_default(),
                depth: av.audio as f64,
            });
        }
    }

    DepthScan { metrics, features }
}

// ---------------------------------------------------------------------------
// FCM — Spring
// ---------------------------------------------------------------------------

struct FcmSpring {
    send_calls: u32,
    sending_endpoints: u32,
    evidence: Option<String>,
}

struct MethodInfo {
    name: String,
    is_endpoint: bool,
    sends_fcm: bool,
    callees: Vec<String>,
}

fn detect_fcm_spring(files: &[&ScannedFile]) -> FcmSpring {
    let mut send_calls = 0u32;
    let mut evidence = None;
    let mut methods: Vec<MethodInfo> = Vec::new();

    for f in files {
        let uses_fcm = f
            .imports
            .iter()
            .any(|i| i.text.contains("firebase.messaging.FirebaseMessaging"));
        let src = f.source();
        // Count send sites file-wide (no double-count from nested methods).
        for_each_descendant(f.root(), &mut |n| {
            if n.kind() == "method_invocation" && is_fcm_send(n, src, uses_fcm) {
                send_calls += 1;
                if evidence.is_none() {
                    evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                }
            }
        });
        // Per-method facts for the call graph.
        for_each_descendant(f.root(), &mut |n| {
            if n.kind() == "method_declaration" {
                methods.push(method_info(n, src, uses_fcm));
            }
        });
    }

    // Transitive "reaches an FCM send" over the method-name graph (best-effort;
    // name-based, so overloads/collisions are conflated — acceptable).
    let mut reaching: HashSet<String> = methods
        .iter()
        .filter(|m| m.sends_fcm)
        .map(|m| m.name.clone())
        .collect();
    for _ in 0..6 {
        let mut grew = false;
        for m in &methods {
            if reaching.contains(&m.name) {
                continue;
            }
            if m.callees.iter().any(|c| reaching.contains(c)) {
                reaching.insert(m.name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    let sending_endpoints = methods
        .iter()
        .filter(|m| m.is_endpoint && (m.sends_fcm || reaching.contains(&m.name)))
        .count() as u32;

    FcmSpring {
        send_calls,
        sending_endpoints,
        evidence,
    }
}

fn method_info(method: Node, src: &[u8], uses_fcm: bool) -> MethodInfo {
    let name = field_text(method, "name", src).unwrap_or_default();
    let is_endpoint = method_annotations(method, src)
        .iter()
        .any(|a| MAPPING_ANNOTATIONS.contains(&a.as_str()));
    let mut sends_fcm = false;
    let mut callees = Vec::new();
    for_each_descendant(method, &mut |n| {
        if n.kind() == "method_invocation" {
            if let Some(nm) = field_text(n, "name", src) {
                callees.push(nm);
            }
            if is_fcm_send(n, src, uses_fcm) {
                sends_fcm = true;
            }
        }
    });
    MethodInfo {
        name,
        is_endpoint,
        sends_fcm,
        callees,
    }
}

fn is_fcm_send(inv: Node, src: &[u8], uses_fcm: bool) -> bool {
    let Some(name) = field_text(inv, "name", src) else {
        return false;
    };
    if !FCM_SEND_NAMES.contains(&name.as_str()) {
        return false;
    }
    let obj = inv
        .child_by_field_name("object")
        .map(|o| node_text(o, src))
        .unwrap_or_default();
    let obj_l = obj.to_ascii_lowercase();
    // Dominant pattern `FirebaseMessaging.getInstance().send(...)`, plus a field
    // named like firebaseMessaging in a file that imports the type.
    obj.contains("FirebaseMessaging") || (uses_fcm && obj_l.contains("messaging"))
}

// ---------------------------------------------------------------------------
// Spring Data Specifications
// ---------------------------------------------------------------------------

struct SpecScan {
    exec_repos: u32,
    spec_defs: u32,
    evidence: Option<String>,
}

fn detect_specifications(files: &[&ScannedFile]) -> SpecScan {
    let mut exec_repos = 0u32;
    let mut spec_defs = 0u32;
    let mut evidence = None;
    for f in files {
        let src = f.source();
        for_each_descendant(f.root(), &mut |n| match n.kind() {
            "interface_declaration" | "class_declaration" => {
                if supertypes(n, src)
                    .iter()
                    .any(|t| t == "JpaSpecificationExecutor")
                {
                    exec_repos += 1;
                    if evidence.is_none() {
                        evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                    }
                }
            }
            "method_declaration" => {
                if field_node(n, "type")
                    .and_then(|t| simple_type_name(t, src))
                    .as_deref()
                    == Some("Specification")
                {
                    spec_defs += 1;
                    if evidence.is_none() {
                        evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                    }
                }
            }
            "field_declaration"
                if field_node(n, "type")
                    .and_then(|t| simple_type_name(t, src))
                    .as_deref()
                    == Some("Specification") =>
            {
                spec_defs += 1;
            }
            _ => {}
        });
    }
    SpecScan {
        exec_repos,
        spec_defs,
        evidence,
    }
}

// ---------------------------------------------------------------------------
// Email (JavaMailSender)
// ---------------------------------------------------------------------------

struct EmailScan {
    send_sites: u32,
    evidence: Option<String>,
}

fn detect_email(files: &[&ScannedFile]) -> EmailScan {
    let mut send_sites = 0u32;
    let mut evidence = None;
    for f in files {
        let src = f.source();
        // Fields/params typed JavaMailSender → receiver names we trust.
        let mut sender_names: BTreeSet<String> = BTreeSet::new();
        for_each_descendant(f.root(), &mut |n| {
            if matches!(n.kind(), "field_declaration" | "formal_parameter")
                && field_node(n, "type")
                    .and_then(|t| simple_type_name(t, src))
                    .as_deref()
                    == Some("JavaMailSender")
            {
                for nm in declarator_names(n, src) {
                    sender_names.insert(nm);
                }
            }
        });
        if sender_names.is_empty() {
            continue;
        }
        for_each_descendant(f.root(), &mut |n| {
            if n.kind() == "method_invocation"
                && field_text(n, "name", src).as_deref() == Some("send")
            {
                let obj = n
                    .child_by_field_name("object")
                    .map(|o| node_text(o, src))
                    .unwrap_or_default();
                let recv = obj.trim_start_matches("this.").to_string();
                if sender_names.contains(&recv) {
                    send_sites += 1;
                    if evidence.is_none() {
                        evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                    }
                }
            }
        });
    }
    EmailScan {
        send_sites,
        evidence,
    }
}

// ---------------------------------------------------------------------------
// OkHttp external APIs (Spring)
// ---------------------------------------------------------------------------

struct OkHttpApiScan {
    api_count: u32,
    evidence: Option<String>,
}

/// Count distinct external API hosts reached via OkHttp in production Spring
/// sources. Scans `http(s)://` string literals in files that import `okhttp3`
/// and that issue outbound calls (`Request.Builder` / `newCall`).
fn detect_okhttp_external_apis(files: &[&ScannedFile]) -> OkHttpApiScan {
    let mut hosts = BTreeSet::new();
    let mut evidence = None;
    for f in files {
        if !uses_okhttp(f) || !file_issues_okhttp_requests(f) {
            continue;
        }
        let src = f.source();
        for_each_descendant(f.root(), &mut |n| {
            if !matches!(n.kind(), "string_literal" | "binary_expression") {
                return;
            }
            for url in extract_http_url_literals(n, src) {
                if let Some(host) = external_api_host(&url) {
                    if hosts.insert(host.clone()) && evidence.is_none() {
                        evidence = Some(format!(
                            "{}:{} ({})",
                            f.rel_path,
                            n.start_position().row + 1,
                            host
                        ));
                    }
                }
            }
        });
    }
    let api_count = hosts.len() as u32;
    let evidence = if api_count == 0 {
        None
    } else {
        Some(format!(
            "{}; hosts={}",
            evidence.unwrap_or_else(|| "okhttp".into()),
            hosts.iter().cloned().collect::<Vec<_>>().join(", ")
        ))
    };
    OkHttpApiScan {
        api_count,
        evidence,
    }
}

fn uses_okhttp(f: &ScannedFile) -> bool {
    f.imports.iter().any(|i| i.text.contains("okhttp3."))
}

// ---------------------------------------------------------------------------
// MinIO object storage (Spring upload / download)
// ---------------------------------------------------------------------------

struct MinioIoScan {
    io_sites: u32,
    evidence: Option<String>,
}

/// Count `putObject` / `getObject` call sites on `MinioClient` receivers in
/// production Spring sources (upload and download).
fn detect_minio_object_io(files: &[&ScannedFile]) -> MinioIoScan {
    let mut io_sites = 0u32;
    let mut evidence = None;
    for f in files {
        if !f.imports.iter().any(|i| i.text.contains("io.minio")) {
            continue;
        }
        let src = f.source();
        let mut client_names: BTreeSet<String> = BTreeSet::new();
        for_each_descendant(f.root(), &mut |n| {
            if !matches!(
                n.kind(),
                "field_declaration" | "formal_parameter" | "local_variable_declaration"
            ) {
                return;
            }
            let Some(t) = field_node(n, "type") else {
                return;
            };
            if simple_type_name(t, src).as_deref() != Some("MinioClient") {
                return;
            }
            for nm in declarator_names(n, src) {
                client_names.insert(nm);
            }
        });
        if client_names.is_empty() {
            continue;
        }
        for_each_descendant(f.root(), &mut |n| {
            if n.kind() != "method_invocation" {
                return;
            }
            let name = field_text(n, "name", src).unwrap_or_default();
            if name != "putObject" && name != "getObject" {
                return;
            }
            let obj = n
                .child_by_field_name("object")
                .map(|o| node_text(o, src))
                .unwrap_or_default();
            let recv = obj.trim_start_matches("this.").to_string();
            let base = recv.split('.').next().unwrap_or(&recv).to_string();
            if client_names.contains(&recv) || client_names.contains(&base) {
                io_sites += 1;
                if evidence.is_none() {
                    evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                }
            }
        });
    }
    MinioIoScan {
        io_sites,
        evidence,
    }
}

fn file_issues_okhttp_requests(f: &ScannedFile) -> bool {
    let src = f.source();
    let mut issues = false;
    for_each_descendant(f.root(), &mut |n| {
        if issues {
            return;
        }
        if n.kind() != "method_invocation" {
            return;
        }
        let name = field_text(n, "name", src).unwrap_or_default();
        let obj = n
            .child_by_field_name("object")
            .map(|o| node_text(o, src))
            .unwrap_or_default();
        if name == "newCall" || (name == "url" && obj.contains("Builder")) {
            issues = true;
        }
    });
    issues
}

fn extract_http_url_literals(node: Node, src: &[u8]) -> Vec<String> {
    match node.kind() {
        "string_literal" => {
            let raw = unquote_java_string(&node_text(node, src));
            if raw.starts_with("http://") || raw.starts_with("https://") {
                vec![raw]
            } else {
                Vec::new()
            }
        }
        "binary_expression" => {
            let left = node.child_by_field_name("left");
            let right = node.child_by_field_name("right");
            let mut out = left
                .map(|l| extract_http_url_literals(l, src))
                .unwrap_or_default();
            out.extend(
                right
                    .map(|r| extract_http_url_literals(r, src))
                    .unwrap_or_default(),
            );
            out
        }
        _ => Vec::new(),
    }
}

fn unquote_java_string(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        t[1..t.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        t.to_string()
    }
}

fn external_api_host(url_like: &str) -> Option<String> {
    let s = url_like.trim();
    if !s.starts_with("http://") && !s.starts_with("https://") {
        return None;
    }
    let rest = s.split("://").nth(1)?;
    let authority = rest.split(&['/', '?', '#'][..]).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if let Some((h, port)) = host_port.rsplit_once(':') {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host_port
        }
    } else {
        host_port
    };
    let host = host.to_ascii_lowercase();
    if is_internal_host(&host) {
        return None;
    }
    Some(host)
}

fn is_internal_host(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".local")
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host == "0.0.0.0"
}

// ---------------------------------------------------------------------------
// FCM — Android (stored in Room to be observed)
// ---------------------------------------------------------------------------

struct FcmAndroid {
    score: u32,
    evidence: Option<String>,
}

fn detect_fcm_android(files: &[&ScannedFile]) -> FcmAndroid {
    // Repo-wide: does any @Dao expose an observable (LiveData/Flow) return?
    let observable = files.iter().any(|f| dao_exposes_observable(f));

    for f in files {
        let src = f.source();
        let mut result: Option<FcmAndroid> = None;
        for_each_descendant(f.root(), &mut |class| {
            if class.kind() != "class_declaration" {
                return;
            }
            if !supertypes(class, src)
                .iter()
                .any(|t| t == "FirebaseMessagingService")
            {
                return;
            }
            // Field name -> type; data-layer = type ends with Dao/Repository.
            let data_fields = data_layer_fields(class, src);
            // Map of same-class method name -> node, for 1+ hop traversal.
            let class_methods = class_methods(class, src);
            let Some(omr) = class_methods.iter().find(|(n, _)| n == "onMessageReceived") else {
                return;
            };
            let reachable = reachable_method_bodies(omr.1, &class_methods, src);
            let mut wrote = false;
            let mut ev = None;
            for body in &reachable {
                for_each_descendant(*body, &mut |n| {
                    if n.kind() != "method_invocation" {
                        return;
                    }
                    let obj = n
                        .child_by_field_name("object")
                        .map(|o| node_text(o, src))
                        .unwrap_or_default();
                    let recv = obj.trim_start_matches("this.").to_string();
                    if !data_fields.contains(&recv) {
                        return;
                    }
                    let callee = field_text(n, "name", src).unwrap_or_default();
                    if is_token_call(&callee) {
                        return;
                    }
                    wrote = true;
                    if ev.is_none() {
                        ev = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                    }
                });
            }
            if wrote {
                let score = 1 + u32::from(observable);
                result = Some(FcmAndroid {
                    score,
                    evidence: ev.or_else(|| {
                        Some(format!("{}:{}", f.rel_path, omr.1.start_position().row + 1))
                    }),
                });
            }
        });
        if let Some(r) = result {
            return r;
        }
    }
    FcmAndroid {
        score: 0,
        evidence: None,
    }
}

fn is_token_call(callee: &str) -> bool {
    let c = callee.to_ascii_lowercase();
    TOKEN_CALL_HINTS.iter().any(|h| c.contains(h))
}

fn dao_exposes_observable(f: &ScannedFile) -> bool {
    let src = f.source();
    let mut found = false;
    for_each_descendant(f.root(), &mut |n| {
        if !matches!(n.kind(), "interface_declaration" | "class_declaration") {
            return;
        }
        if !class_annotations(n, src).iter().any(|a| a == "Dao") {
            return;
        }
        for m in descendants_of_kind(n, "method_declaration") {
            if let Some(t) = field_node(m, "type").and_then(|t| simple_type_name(t, src)) {
                if t == "LiveData" || t == "Flow" || t == "StateFlow" {
                    found = true;
                }
            }
        }
    });
    found
}

/// Field names whose declared type ends with `Dao` or `Repository`.
fn data_layer_fields(class: Node, src: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in descendants_of_kind(class, "field_declaration") {
        if let Some(t) = field_node(f, "type").and_then(|t| simple_type_name(t, src)) {
            if t.ends_with("Dao") || t.ends_with("Repository") {
                for nm in declarator_names(f, src) {
                    out.insert(nm);
                }
            }
        }
    }
    out
}

/// (method name, body node) for each method declared directly-ish in the class.
fn class_methods<'a>(class: Node<'a>, src: &[u8]) -> Vec<(String, Node<'a>)> {
    let mut out = Vec::new();
    for m in descendants_of_kind(class, "method_declaration") {
        let name = field_text(m, "name", src).unwrap_or_default();
        if let Some(body) = m.child_by_field_name("body") {
            out.push((name, body));
        }
    }
    out
}

/// Bodies reachable from `start` via same-class calls (bounded depth 4).
fn reachable_method_bodies<'a>(
    start: Node<'a>,
    class_methods: &[(String, Node<'a>)],
    src: &[u8],
) -> Vec<Node<'a>> {
    let mut visited: HashSet<usize> = HashSet::new();
    let mut bodies = vec![start];
    let mut frontier = vec![start];
    visited.insert(start.id());
    for _ in 0..4 {
        let mut next = Vec::new();
        for body in &frontier {
            let callee_names: Vec<String> = descendants_of_kind(*body, "method_invocation")
                .iter()
                .filter_map(|inv| field_text(*inv, "name", src))
                .collect();
            for (name, mbody) in class_methods {
                if visited.contains(&mbody.id()) {
                    continue;
                }
                if callee_names.iter().any(|c| c == name) {
                    visited.insert(mbody.id());
                    bodies.push(*mbody);
                    next.push(*mbody);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    bodies
}

// ---------------------------------------------------------------------------
// Android graphics (custom drawing)
// ---------------------------------------------------------------------------

struct GfxScan {
    count: u32,
    evidence: Option<String>,
}

fn detect_graphics(files: &[&ScannedFile]) -> GfxScan {
    let mut count = 0u32;
    let mut evidence = None;
    for f in files {
        let src = f.source();
        for_each_descendant(f.root(), &mut |n| {
            let hit = match n.kind() {
                "method_declaration" => {
                    field_text(n, "name", src).as_deref() == Some("onDraw")
                        && param_types(n, src).iter().any(|t| t == "Canvas")
                }
                "class_declaration" => supertypes(n, src).iter().any(|t| t == "GLSurfaceView"),
                "object_creation_expression" => field_node(n, "type")
                    .and_then(|t| simple_type_name(t, src))
                    .map(|t| t == "Paint" || t == "Canvas")
                    .unwrap_or(false),
                _ => false,
            };
            if hit {
                count += 1;
                if evidence.is_none() {
                    evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                }
            }
        });
    }
    GfxScan { count, evidence }
}

// ---------------------------------------------------------------------------
// Android audio / video
// ---------------------------------------------------------------------------

struct AvScan {
    audio: u32,
    video: u32,
    audio_evidence: Option<String>,
    video_evidence: Option<String>,
}

fn detect_av(files: &[&ScannedFile]) -> AvScan {
    let mut audio = 0u32;
    let mut video = 0u32;
    let mut audio_evidence = None;
    let mut video_evidence = None;
    for f in files {
        let src = f.source();
        let media3 = f.imports.iter().any(|i| i.text.contains("androidx.media3"));
        if media3 {
            video += 1;
            if video_evidence.is_none() {
                video_evidence = Some(format!("{}:import androidx.media3", f.rel_path));
            }
        }
        for_each_descendant(f.root(), &mut |n| {
            if n.kind() != "type_identifier" {
                return;
            }
            let t = node_text(n, src);
            if AV_VIDEO_TYPES.contains(&t.as_str()) {
                video += 1;
                if video_evidence.is_none() {
                    video_evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                }
            } else if AV_AUDIO_TYPES.contains(&t.as_str()) {
                audio += 1;
                if audio_evidence.is_none() {
                    audio_evidence = Some(format!("{}:{}", f.rel_path, n.start_position().row + 1));
                }
            }
        });
    }
    AvScan {
        audio,
        video,
        audio_evidence,
        video_evidence,
    }
}

// ---------------------------------------------------------------------------
// Shared AST helpers
// ---------------------------------------------------------------------------

fn for_each_descendant<F: FnMut(Node)>(node: Node, f: &mut F) {
    f(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        for_each_descendant(child, f);
    }
}

fn descendants_of_kind<'a>(node: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        let mut cursor = n.walk();
        for c in n.children(&mut cursor) {
            if c.kind() == kind {
                out.push(c);
            }
            stack.push(c);
        }
    }
    out
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text(node: Node, src: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(src.len());
    String::from_utf8_lossy(&src[start..end]).into_owned()
}

fn field_node<'a>(node: Node<'a>, field: &str) -> Option<Node<'a>> {
    node.child_by_field_name(field)
}

fn field_text(node: Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field).map(|n| node_text(n, src))
}

fn last_segment(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s).trim()
}

fn simple_type_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "scoped_identifier" | "scoped_type_identifier" => {
            Some(last_segment(&node_text(node, src)).to_string())
        }
        "generic_type" => children(node)
            .iter()
            .find_map(|c| simple_type_name(*c, src)),
        _ => children(node)
            .iter()
            .find_map(|c| simple_type_name(*c, src)),
    }
}

fn declarator_names(field_or_param: Node, src: &[u8]) -> Vec<String> {
    if field_or_param.kind() == "formal_parameter" {
        return field_text(field_or_param, "name", src)
            .into_iter()
            .collect();
    }
    let mut out = Vec::new();
    for d in descendants_of_kind(field_or_param, "variable_declarator") {
        if let Some(n) = field_text(d, "name", src) {
            out.push(n);
        }
    }
    out
}

fn param_types(method: Node, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for p in descendants_of_kind(method, "formal_parameter") {
        if let Some(t) = field_node(p, "type").and_then(|t| simple_type_name(t, src)) {
            out.push(t);
        }
    }
    out
}

fn annotation_name(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "marker_annotation" | "annotation" => children(node).iter().find_map(|c| {
            let k = c.kind();
            if k == "identifier" || k == "scoped_identifier" || k == "type_identifier" {
                Some(last_segment(&node_text(*c, src)).to_string())
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn class_annotations(node: Node, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in children(node) {
        if c.kind() != "modifiers" {
            continue;
        }
        for m in children(c) {
            if let Some(a) = annotation_name(m, src) {
                out.push(a);
            }
        }
    }
    out
}

fn method_annotations(method: Node, src: &[u8]) -> Vec<String> {
    class_annotations(method, src)
}

fn supertypes(node: Node, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in children(node) {
        match c.kind() {
            "superclass" => {
                for t in children(c) {
                    if let Some(n) = simple_type_name(t, src) {
                        out.push(n);
                    }
                }
            }
            "super_interfaces" | "extends_interfaces" => {
                for tl in children(c) {
                    if tl.kind() == "type_list" {
                        for t in children(tl) {
                            if let Some(n) = simple_type_name(t, src) {
                                out.push(n);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(rel: &str, src: &str) -> ScannedFile {
        ScannedFile::from_inline(rel, src.as_bytes()).expect("parse")
    }

    #[test]
    fn fcm_spring_counts_send_and_endpoint_via_call_graph() {
        let svc = sf(
            "src/main/java/com/x/PushNotificationService.java",
            r#"package com.x;
import com.google.firebase.messaging.FirebaseMessaging;
import org.springframework.stereotype.Service;
@Service
public class PushNotificationService {
    public String sendPush(String t) {
        return FirebaseMessaging.getInstance().send(null);
    }
}"#,
        );
        let ctrl = sf(
            "src/main/java/com/x/NotifController.java",
            r#"package com.x;
import org.springframework.web.bind.annotation.*;
@RestController
public class NotifController {
    private PushNotificationService svc;
    @PostMapping("/n")
    public void create() { svc.sendPush("hi"); }
    @GetMapping("/q")
    public void quiet() { }
}"#,
        );
        let got = detect_depth(&[svc, ctrl], Stack::Spring).metrics;
        assert_eq!(got[metrics::FCM_SEND_CALL_COUNT], 1.0);
        assert_eq!(got[metrics::FCM_SENDING_ENDPOINT_COUNT], 1.0);
    }

    #[test]
    fn specifications_detects_executor_and_defs() {
        let repo = sf(
            "src/main/java/com/x/ProductRepository.java",
            r#"package com.x;
import org.springframework.data.jpa.repository.JpaRepository;
import org.springframework.data.jpa.repository.JpaSpecificationExecutor;
public interface ProductRepository extends JpaRepository<Product, Long>, JpaSpecificationExecutor<Product> {}"#,
        );
        let specs = sf(
            "src/main/java/com/x/ProductSpecifications.java",
            r#"package com.x;
import org.springframework.data.jpa.domain.Specification;
public final class ProductSpecifications {
    public static Specification<Product> hasStatus(String s) { return null; }
    public static Specification<Product> nameLike(String s) { return null; }
}"#,
        );
        let got = detect_depth(&[repo, specs], Stack::Spring).metrics;
        assert_eq!(got[metrics::SPEC_EXECUTOR_REPO_COUNT], 1.0);
        assert_eq!(got[metrics::SPECIFICATION_DEF_COUNT], 2.0);
    }

    #[test]
    fn email_counts_javamailsender_sends() {
        let f = sf(
            "src/main/java/com/x/EmailService.java",
            r#"package com.x;
import org.springframework.mail.SimpleMailMessage;
import org.springframework.mail.javamail.JavaMailSender;
public class EmailService {
    private JavaMailSender mailSender;
    public void send(String to) {
        SimpleMailMessage msg = new SimpleMailMessage();
        mailSender.send(msg);
    }
}"#,
        );
        let got = detect_depth(&[f], Stack::Spring).metrics;
        assert_eq!(got[metrics::EMAIL_SEND_SITE_COUNT], 1.0);
    }

    #[test]
    fn fcm_android_room_store_positive_and_token_only_negative() {
        let dao = sf(
            "app/src/main/java/com/x/MessageDao.java",
            r#"package com.x;
import androidx.room.Dao;
import androidx.lifecycle.LiveData;
@Dao
public interface MessageDao {
    void insert(Object e);
    LiveData<Object> observeAll();
}"#,
        );
        let positive = sf(
            "app/src/main/java/com/x/MyFirebaseMessagingService.java",
            r#"package com.x;
import com.google.firebase.messaging.FirebaseMessagingService;
import com.google.firebase.messaging.RemoteMessage;
public class MyFirebaseMessagingService extends FirebaseMessagingService {
    MessageDao messageDao;
    UserRepository userRepository;
    public void onNewToken(String t) { userRepository.updateFcmToken(t); }
    public void onMessageReceived(RemoteMessage m) { store(m); }
    private void store(RemoteMessage m) { messageDao.insert(m); }
}"#,
        );
        let got = detect_depth(&[dao, positive], Stack::Android).metrics;
        assert_eq!(got[metrics::FCM_ANDROID_ROOM_STORE], 2.0);

        // Token-only: repository call lives in onNewToken; onMessageReceived only
        // shows a notification → must score 0. (Fresh DAO; ScannedFile isn't Clone.)
        let dao = sf(
            "app/src/main/java/com/x/MessageDao.java",
            r#"package com.x;
import androidx.room.Dao;
import androidx.lifecycle.LiveData;
@Dao
public interface MessageDao {
    void insert(Object e);
    LiveData<Object> observeAll();
}"#,
        );
        let token_only = sf(
            "app/src/main/java/com/x/AppFms.java",
            r#"package com.x;
import com.google.firebase.messaging.FirebaseMessagingService;
import com.google.firebase.messaging.RemoteMessage;
public class AppFms extends FirebaseMessagingService {
    PushDeviceRepository pushDeviceRepository;
    public void onNewToken(String t) { pushDeviceRepository.registerPushDevice(t); }
    public void onMessageReceived(RemoteMessage m) { showNotification(m); }
    private void showNotification(RemoteMessage m) { }
}"#,
        );
        let got2 = detect_depth(&[dao, token_only], Stack::Android).metrics;
        assert_eq!(got2[metrics::FCM_ANDROID_ROOM_STORE], 0.0);
    }

    #[test]
    fn graphics_and_av() {
        let gfx = sf(
            "app/src/main/java/com/x/Chart.java",
            r#"package com.x;
import android.graphics.Canvas;
import android.view.View;
public class Chart extends View {
    protected void onDraw(Canvas canvas) { }
}"#,
        );
        let got = detect_depth(&[gfx], Stack::Android).metrics;
        assert_eq!(got[metrics::GRAPHICS_CUSTOM_DRAW_COUNT], 1.0);

        let av = sf(
            "app/src/main/java/com/x/Player.java",
            r#"package com.x;
import androidx.media3.exoplayer.ExoPlayer;
public class Player {
    ExoPlayer player;
    void play() { }
}"#,
        );
        let got2 = detect_depth(&[av], Stack::Android).metrics;
        assert!(got2[metrics::AV_USAGE_COUNT] >= 1.0);
    }

    #[test]
    fn okhttp_external_api_counts_distinct_hosts_and_skips_internal() {
        let outbound = sf(
            "src/main/java/com/x/OutboundMessageService.java",
            r#"package com.x;
import okhttp3.MediaType;
import okhttp3.OkHttpClient;
import okhttp3.Request;
import okhttp3.RequestBody;
import okhttp3.Response;
public class OutboundMessageService {
    private final OkHttpClient httpClient = new OkHttpClient();
    private boolean sendToMeta(String token) {
        String url = "https://graph.facebook.com/v18.0/me/messages?access_token=" + token;
        Request request = new Request.Builder().url(url).post(RequestBody.create("{}", MediaType.get("application/json"))).build();
        return httpClient.newCall(request).execute().isSuccessful();
    }
    private boolean sendToTelegram(String token, String chatId) {
        String url = "https://api.telegram.org/bot" + token + "/sendMessage";
        Request request = new Request.Builder().url(url).post(RequestBody.create("{}", MediaType.get("application/json"))).build();
        return httpClient.newCall(request).execute().isSuccessful();
    }
    private boolean pingLocal() {
        Request request = new Request.Builder().url("http://localhost:8080/health").get().build();
        return httpClient.newCall(request).execute().isSuccessful();
    }
}"#,
        );
        let scan = detect_depth(&[outbound], Stack::Spring);
        assert_eq!(scan.metrics[metrics::OKHTTP_EXTERNAL_API_COUNT], 2.0);
        assert!(scan.features.iter().any(|f| f.category == "external_api"));
    }

    #[test]
    fn minio_object_io_counts_put_and_get_on_minio_client() {
        let image = sf(
            "src/main/java/com/x/ImageService.java",
            r#"package com.x;
import io.minio.GetObjectArgs;
import io.minio.MinioClient;
import io.minio.PutObjectArgs;
import java.io.InputStream;
import java.util.Optional;
public class ImageService {
    private final Optional<MinioClient> minioClient;
    private final String minioBucket;
    public ImageService(Optional<MinioClient> minioClient, String minioBucket) {
        this.minioClient = minioClient;
        this.minioBucket = minioBucket;
    }
    public void upload(InputStream istream, String objectName) throws Exception {
        MinioClient client = getClientOrThrow();
        client.putObject(
            PutObjectArgs.builder()
                .bucket(minioBucket)
                .object(objectName)
                .stream(istream, -1, 10485760)
                .build());
    }
    public InputStream download(String filename) throws Exception {
        MinioClient client = getClientOrThrow();
        return client.getObject(
            GetObjectArgs.builder()
                .bucket(minioBucket)
                .object(filename)
                .build());
    }
    private MinioClient getClientOrThrow() {
        return minioClient.orElseThrow();
    }
}"#,
        );
        let scan = detect_depth(&[image], Stack::Spring);
        assert_eq!(scan.metrics[metrics::MINIO_OBJECT_IO_SITE_COUNT], 2.0);
        assert!(scan.features.iter().any(|f| f.category == "storage"));

        let config = sf(
            "src/main/java/com/x/MinioConfig.java",
            r#"package com.x;
import io.minio.MinioClient;
import org.springframework.context.annotation.Bean;
public class MinioConfig {
    @Bean
    public MinioClient minioClient() {
        return MinioClient.builder().endpoint("https://minio.example.com").build();
    }
}"#,
        );
        let cfg = detect_depth(&[config], Stack::Spring);
        assert_eq!(cfg.metrics[metrics::MINIO_OBJECT_IO_SITE_COUNT], 0.0);
    }

    #[test]
    fn okhttp_dynamic_base_url_counts_literal_host() {
        let paypal = sf(
            "src/main/java/com/x/PaypalService.java",
            r#"package com.x;
import okhttp3.OkHttpClient;
import okhttp3.Request;
import okhttp3.RequestBody;
import okhttp3.MediaType;
public class PaypalService {
    private final OkHttpClient client = new OkHttpClient();
    private String baseUrl = "https://api-m.sandbox.paypal.com";
    public void create() {
        Request httpRequest = new Request.Builder()
            .url(baseUrl + "/v2/checkout/orders")
            .post(RequestBody.create("{}", MediaType.get("application/json")))
            .build();
        client.newCall(httpRequest).execute();
    }
}"#,
        );
        let got = detect_depth(&[paypal], Stack::Spring).metrics;
        assert_eq!(got[metrics::OKHTTP_EXTERNAL_API_COUNT], 1.0);
    }

    #[test]
    fn spring_keys_zero_on_android_stack_and_vice_versa() {
        let f = sf(
            "src/main/java/com/x/A.java",
            "package com.x; public class A {}",
        );
        let got = detect_depth(&[f], Stack::Spring).metrics;
        // All extra-tech keys present (zero-filled) regardless of matches.
        for k in metrics::EXTRA_TECH_KEYS {
            assert!(got.contains_key(*k), "missing key {k}");
        }
    }
}
