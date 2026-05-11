//! Project-scoped peer-group analysis.
//!
//! Each task is bucketed into a peer group keyed on `(stack, layer,
//! action)`. Within each group we compute a density (statements
//! surviving normalised / estimation points) and flag any task whose
//! density is more than `mad_k_threshold` MADs away from the group
//! median. This is the *only* outlier criterion — the prior
//! multi-criterion (`points` + `LS` + `LS/pt`) detector lit up too many
//! false positives on tasks that just happened to be smaller or larger
//! than typical.
//!
//! Layer is derived from the **file paths** of fingerprints attributed
//! to each linked PR's commits — not from a keyword scan over the task
//! name and parent USER_STORY name. The keyword scan misclassified
//! anything written in mixed Catalan/Spanish/English, and parent
//! USER_STORY names like "CRUD usuaris" exploded into four spurious
//! layer hits per child task.
//!
//! Action is derived from the diff stats of the linked PR(s)
//! (`lat / (lat + ld)`) instead of a keyword scan, which routinely
//! marked a 200-line net-add task as `modify` because the title used
//! the word "fix".
//!
//! The scope is **the entire project** (all sprints with `start_date <=
//! today`). Per-sprint persistence is gone — the report renders one
//! peer-group section at H2 between the sprint loop and the cumulative
//! summary.
//!
//! Filter: only DONE TASK / BUG (not USER_STORY) with at least one
//! linked PR, where every linked PR is `merged = 1`. WIP tasks and
//! tasks linked only to open PRs would distort the densities.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{params, params_from_iter, Connection};
use sprint_grader_core::config::RepoAnalysisConfig;
use sprint_grader_core::formatting::fmt_float;
use sprint_grader_core::stats;
use tracing::info;

use crate::layer_path::layer_for_path;

const ANY: &str = "*";

const LAYER_ORDER: &[&str] = &[
    "spring_controller",
    "spring_service",
    "spring_repository",
    "spring_entity",
    "spring_dto_mapper",
    "spring_config_security",
    "spring_other",
    "android_fragment",
    "android_layout",
    "android_viewmodel",
    "android_recyclerview",
    "android_navigation",
    "android_activity",
    "android_repository",
    "android_retrofit",
    "android_room",
    "android_other",
];

fn layer_display(layer: &str) -> &'static str {
    match layer {
        "spring_controller" => "Controller method",
        "spring_service" => "Service method",
        "spring_repository" => "Repository method",
        "spring_entity" => "Entity",
        "spring_dto_mapper" => "DTO / Mapper",
        "spring_config_security" => "Config / Security",
        "spring_other" => "Other",
        "android_fragment" => "Fragment",
        "android_layout" => "layout",
        "android_viewmodel" => "ViewModel",
        "android_recyclerview" => "RecyclerView / Adapter",
        "android_navigation" => "Navigation",
        "android_activity" => "Activity",
        "android_repository" => "Repository",
        "android_retrofit" => "Retrofit / API client",
        "android_room" => "Room / local DB",
        "android_other" => "Other",
        _ => "",
    }
}

fn stack_display(stack: &str) -> &'static str {
    match stack {
        "spring" => "Spring",
        "android" => "Android",
        _ => "",
    }
}

fn action_display(action: &str) -> &'static str {
    match action {
        "create" => "Create",
        "modify" => "Modify",
        _ => "",
    }
}

fn layer_sort_key(layer: &str) -> usize {
    LAYER_ORDER
        .iter()
        .position(|l| *l == layer)
        .unwrap_or(LAYER_ORDER.len())
}

fn build_label(stack: &str, layer: &str, action: &str) -> String {
    let stack_name = stack_display(stack);
    let layer_name = if layer == ANY {
        None
    } else {
        Some(layer_display(layer))
    };
    let action_name = if action == ANY {
        None
    } else {
        Some(action_display(action))
    };

    match (layer_name, action_name) {
        (None, None) => format!("{} — (any kind)", stack_name),
        (None, Some(a)) => format!("{} — {} (any layer)", stack_name, a),
        (Some(l), None) => format!("{} — {} (any action)", stack_name, l),
        (Some(l), Some(a)) => format!("{} — {} {}", stack_name, a, l),
    }
}

#[derive(Debug, Clone)]
struct TaskInfo {
    task_id: i64,
    estimation_points: Option<f64>,
    stack: Option<String>,
    layers: BTreeSet<String>,
    action: String,
    /// LS = surviving non-comment lines, distributed across linked tasks
    /// by point share. Informational only — kept so the report can still
    /// surface a "median LS" bullet.
    ls: Option<f64>,
    /// Surviving normalised statements distributed across linked tasks
    /// by point share. The numerator of the density unit that drives
    /// outlier detection.
    stmts_surviving_normalized: Option<f64>,
}

fn infer_repo_type(repo: Option<&str>) -> Option<&'static str> {
    let repo = repo?;
    let short = repo.rsplit('/').next()?.to_lowercase();
    if short.starts_with("android") || short.contains("-android") {
        Some("android")
    } else if short.starts_with("spring") || short.contains("-spring") {
        Some("spring")
    } else {
        None
    }
}

fn combine_repo_types(types: &[Option<&'static str>]) -> Option<String> {
    let uniq: BTreeSet<&str> = types.iter().filter_map(|t| *t).collect();
    match uniq.len() {
        0 => None,
        1 => Some(uniq.into_iter().next().unwrap().to_string()),
        _ => Some("mixed".into()),
    }
}

/// Per-PR bag of layer counts (how many fingerprint paths were
/// attributed to each layer) and the diff stats needed for the action
/// inference. Looked up by pr_id while building task layer signatures.
#[derive(Debug, Default, Clone)]
struct PrSignature {
    repo_full_name: Option<String>,
    layer_counts: HashMap<String, u32>,
    lat: Option<f64>,
    ld: Option<f64>,
    /// Surviving non-comment lines (LS), summed across sprint rows in
    /// `pr_line_metrics`. Distributed across linked tasks by point
    /// share and surfaced as the group's "median LS" bullet.
    ls: Option<f64>,
    stmts_surviving_normalized: Option<f64>,
}

/// Build the per-PR signature map for every PR in the project. The
/// fingerprint join is approximate by design — `fingerprints` only
/// holds *surviving* statements, so a PR that introduced code which
/// has since been deleted contributes nothing here. That bias is
/// acceptable for peer-group analysis: tasks whose code didn't survive
/// are exactly the ones the density discordance check is meant to
/// surface anyway, and basing the layer signature on "what made it to
/// HEAD" reflects the work the team kept.
fn build_pr_signatures(
    conn: &Connection,
    sprint_ids: &[i64],
) -> rusqlite::Result<HashMap<String, PrSignature>> {
    if sprint_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sp = std::iter::repeat("?")
        .take(sprint_ids.len())
        .collect::<Vec<_>>()
        .join(",");

    // Seed with PR row + repo_full_name. We pull every PR that's linked
    // to a non-USER_STORY DONE task in the project; the merged-only
    // filter applies later when we build task signatures.
    let mut signatures: HashMap<String, PrSignature> = HashMap::new();
    let pr_sql = format!(
        "SELECT DISTINCT pr.id, pr.repo_full_name
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id IN ({sp})
           AND t.type != 'USER_STORY' AND t.status = 'DONE'"
    );
    let mut stmt = conn.prepare(&pr_sql)?;
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map(params_from_iter(sprint_ids.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (pr_id, repo) in rows {
        signatures.insert(
            pr_id,
            PrSignature {
                repo_full_name: repo,
                ..PrSignature::default()
            },
        );
    }
    if signatures.is_empty() {
        return Ok(signatures);
    }

    // Layer counts: one row per surviving fingerprint, joined to PR via
    // (blame_commit -> pr_commits.sha). The same path can appear on
    // multiple statements; we count statements (cheaper signal than
    // de-duping paths first, and naturally weights "more code in this
    // file" higher).
    let mut stmt = conn.prepare(&format!(
        "SELECT c.pr_id, fp.file_path
         FROM fingerprints fp
         JOIN pr_commits c ON c.sha = fp.blame_commit
         WHERE fp.sprint_id IN ({sp}) AND fp.file_path IS NOT NULL"
    ))?;
    let rows = stmt.query_map(params_from_iter(sprint_ids.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (pr_id, path) = row?;
        let Some(path) = path else { continue };
        let Some(sig) = signatures.get_mut(&pr_id) else {
            continue;
        };
        let stack = infer_repo_type(sig.repo_full_name.as_deref());
        if let Some(layer) = layer_for_path(&path, stack) {
            *sig.layer_counts.entry(layer.to_string()).or_insert(0) += 1;
        }
    }
    drop(stmt);

    // Diff stats for the action inference + LS/LAR informational
    // columns. `pr_line_metrics` is sprint-keyed, so a PR that crossed
    // the sprint boundary has multiple rows; sum them.
    let mut stmt = conn.prepare(&format!(
        "SELECT pr_id,
                COALESCE(SUM(lat), 0) AS lat,
                COALESCE(SUM(ld), 0)  AS ld,
                COALESCE(SUM(ls), 0)  AS ls
         FROM pr_line_metrics
         WHERE sprint_id IN ({sp})
         GROUP BY pr_id"
    ))?;
    let rows = stmt.query_map(params_from_iter(sprint_ids.iter()), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<f64>>(1)?,
            r.get::<_, Option<f64>>(2)?,
            r.get::<_, Option<f64>>(3)?,
        ))
    })?;
    for row in rows {
        let (pr_id, lat, ld, ls) = row?;
        let Some(sig) = signatures.get_mut(&pr_id) else {
            continue;
        };
        sig.lat = lat;
        sig.ld = ld;
        sig.ls = ls;
    }
    drop(stmt);

    // Surviving normalised statements per PR (the numerator of the
    // density unit). Like pr_line_metrics, sprint-keyed; sum across
    // sprints for safety.
    let mut stmt = conn.prepare(&format!(
        "SELECT pr_id,
                COALESCE(SUM(statements_surviving_normalized), 0)
         FROM pr_survival
         WHERE sprint_id IN ({sp})
         GROUP BY pr_id"
    ))?;
    let rows = stmt.query_map(params_from_iter(sprint_ids.iter()), |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
    })?;
    for row in rows {
        let (pr_id, surv) = row?;
        let Some(sig) = signatures.get_mut(&pr_id) else {
            continue;
        };
        sig.stmts_surviving_normalized = surv;
    }
    drop(stmt);

    Ok(signatures)
}

/// Inferred PR action: `create` if the diff is dominated by additions
/// (ratio of LAT to LAT+LD ≥ 0.7); `modify` otherwise. Returns `None`
/// when the PR has no diff stats yet. The threshold is deliberately
/// not a config knob — anything close to 50/50 is genuinely a modify;
/// anything heavily one-sided is a create.
fn pr_action(sig: &PrSignature) -> Option<&'static str> {
    let lat = sig.lat.unwrap_or(0.0).max(0.0);
    let ld = sig.ld.unwrap_or(0.0).max(0.0);
    let total = lat + ld;
    if total <= 0.0 {
        return None;
    }
    let add_ratio = lat / total;
    Some(if add_ratio >= 0.7 { "create" } else { "modify" })
}

/// Pick the dominant layer set for a task: every layer that contributed
/// at least 25 % of the merged-PR statement count, or — when nothing
/// crosses that bar — the single top layer. Returning a small set
/// (typically one or two layers) keeps grouping focused; if every layer
/// touched were emitted, full-stack USER_STORY children would re-explode
/// into multi-bucket noise just like the keyword path used to.
fn dominant_layers(layer_counts: &HashMap<String, u32>) -> Vec<String> {
    if layer_counts.is_empty() {
        return Vec::new();
    }
    let total: u32 = layer_counts.values().sum();
    if total == 0 {
        return Vec::new();
    }
    let threshold = (total as f64 * 0.25).ceil() as u32;
    let mut significant: Vec<(String, u32)> = layer_counts
        .iter()
        .filter(|(_, c)| **c >= threshold)
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    if significant.is_empty() {
        // Nothing crossed the bar; return the single top layer.
        if let Some((k, v)) = layer_counts.iter().max_by_key(|(_, c)| **c) {
            significant.push((k.clone(), *v));
        }
    }
    significant.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    significant.into_iter().map(|(k, _)| k).collect()
}

/// Combine per-PR actions into a task action. `create` wins iff every
/// PR is a create; otherwise `modify`. We deliberately avoid a "mixed"
/// bucket — it always ends up tiny and serves only to muddy the labels.
fn task_action(actions: &[&'static str]) -> &'static str {
    if actions.is_empty() {
        return "create";
    }
    if actions.iter().all(|a| *a == "create") {
        "create"
    } else {
        "modify"
    }
}

/// Load DONE TASK/BUG rows for the project across the given sprints,
/// filtered to those with at least one linked PR where *every* linked
/// PR is merged. Returns the per-task signatures (stack, dominant
/// layers, action, density inputs).
fn load_tasks_for_project(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
) -> rusqlite::Result<Vec<TaskInfo>> {
    if sprint_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sp = std::iter::repeat("?")
        .take(sprint_ids.len())
        .collect::<Vec<_>>()
        .join(",");

    // Candidate tasks: DONE non-USER_STORY in scope.
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, t.estimation_points
         FROM tasks t
         JOIN sprints s ON s.id = t.sprint_id
         WHERE t.sprint_id IN ({sp}) AND s.project_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'"
    ))?;
    let mut params_vec: Vec<rusqlite::types::Value> = sprint_ids
        .iter()
        .copied()
        .map(rusqlite::types::Value::Integer)
        .collect();
    params_vec.push(rusqlite::types::Value::Integer(project_id));
    let candidate_rows: Vec<(i64, Option<f64>)> = stmt
        .query_map(params_from_iter(params_vec.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<f64>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if candidate_rows.is_empty() {
        return Ok(Vec::new());
    }

    // Linked PRs, grouped per task. We pull both pr_id and pr.merged so
    // we can reject tasks with any open / closed-without-merge PRs.
    let mut stmt = conn.prepare(&format!(
        "SELECT tpr.task_id, pr.id, COALESCE(pr.merged, 0)
         FROM task_pull_requests tpr
         JOIN tasks t  ON t.id = tpr.task_id
         JOIN sprints s ON s.id = t.sprint_id
         JOIN pull_requests pr ON pr.id = tpr.pr_id
         WHERE t.sprint_id IN ({sp}) AND s.project_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'"
    ))?;
    let pr_rows: Vec<(i64, String, i64)> = stmt
        .query_map(params_from_iter(params_vec.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut task_prs: HashMap<i64, Vec<(String, bool)>> = HashMap::new();
    for (tid, pr_id, merged) in pr_rows {
        task_prs.entry(tid).or_default().push((pr_id, merged != 0));
    }

    // Per-task PR-share aggregation. Each PR's signature is split
    // across linked tasks weighted by their estimation-point share, so
    // a multi-task PR doesn't double-count.
    let signatures = build_pr_signatures(conn, sprint_ids)?;

    // pr_total_points / pr_task_count drive the weight share. Same
    // rule the per-sprint code used; computed per project here.
    let mut pr_total_points: HashMap<String, f64> = HashMap::new();
    let mut pr_task_count: HashMap<String, i64> = HashMap::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT tpr.pr_id,
                COALESCE(SUM(COALESCE(t.estimation_points, 0)), 0),
                COUNT(*)
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         JOIN sprints s ON s.id = t.sprint_id
         WHERE t.sprint_id IN ({sp}) AND s.project_id = ?
           AND t.type != 'USER_STORY' AND t.status = 'DONE'
         GROUP BY tpr.pr_id"
    ))?;
    for row in stmt
        .query_map(params_from_iter(params_vec.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        pr_total_points.insert(row.0.clone(), row.1);
        pr_task_count.insert(row.0, row.2);
    }
    drop(stmt);

    let mut infos = Vec::with_capacity(candidate_rows.len());
    for (tid, pts) in candidate_rows {
        let Some(prs) = task_prs.get(&tid) else {
            continue; // no linked PR — drop
        };
        if prs.is_empty() || !prs.iter().all(|(_, merged)| *merged) {
            continue; // ≥1 PR + every linked PR merged
        }

        // Combine per-PR signatures into a per-task layer-count bag,
        // weighted the same way LS is split. Stack is the combination
        // of every linked PR's repo type.
        let task_pts = pts.unwrap_or(0.0);
        let mut layer_counts: HashMap<String, u32> = HashMap::new();
        let mut stack_seen: Vec<Option<&'static str>> = Vec::new();
        let mut actions: Vec<&'static str> = Vec::new();
        let mut ls_total = 0.0;
        let mut ls_seen = false;
        let mut stmts_total = 0.0;
        let mut stmts_seen = false;

        for (pr_id, _merged) in prs {
            let Some(sig) = signatures.get(pr_id) else {
                continue;
            };
            stack_seen.push(infer_repo_type(sig.repo_full_name.as_deref()));
            if let Some(action) = pr_action(sig) {
                actions.push(action);
            }
            // PR signature → task: split each PR's signal by the
            // task's point share, falling back to per-task uniform
            // when the PR has zero total points.
            let tot = pr_total_points.get(pr_id).copied().unwrap_or(0.0);
            let count = pr_task_count.get(pr_id).copied().unwrap_or(1).max(1);
            let weight = if tot > 0.0 {
                task_pts / tot
            } else {
                1.0 / count as f64
            };
            for (layer, c) in &sig.layer_counts {
                let w = (*c as f64) * weight;
                // We accumulate on a fractional layer-count so the
                // dominant-layer threshold operates on the task's
                // share. Round on read.
                let entry = layer_counts.entry(layer.clone()).or_insert(0);
                *entry = entry.saturating_add(w.round() as u32);
            }
            if let Some(v) = sig.ls {
                ls_total += v * weight;
                ls_seen = true;
            }
            if let Some(v) = sig.stmts_surviving_normalized {
                stmts_total += v * weight;
                stmts_seen = true;
            }
        }

        let stack = combine_repo_types(&stack_seen);
        let layers: BTreeSet<String> = dominant_layers(&layer_counts).into_iter().collect();
        let ls_opt = if ls_seen { Some(ls_total) } else { None };
        let stmts_opt = if stmts_seen { Some(stmts_total) } else { None };
        if matches!(stack.as_deref(), Some("spring") | Some("android")) && layers.is_empty() {
            // Mark as "<stack>_other" so a task with merged PRs but no
            // recognisable layer touches still appears in the report,
            // grouped with its peers — rather than disappearing from
            // section C entirely, which would obscure delivery state.
            let mut s: BTreeSet<String> = BTreeSet::new();
            s.insert(format!("{}_other", stack.as_deref().unwrap()));
            infos.push(TaskInfo {
                task_id: tid,
                estimation_points: pts,
                stack,
                layers: s,
                action: task_action(&actions).into(),
                ls: ls_opt,
                stmts_surviving_normalized: stmts_opt,
            });
            continue;
        }
        if stack.is_none() || layers.is_empty() {
            continue;
        }

        infos.push(TaskInfo {
            task_id: tid,
            estimation_points: pts,
            stack,
            layers,
            action: task_action(&actions).into(),
            ls: ls_opt,
            stmts_surviving_normalized: stmts_opt,
        });
    }
    Ok(infos)
}

fn task_tuples(task: &TaskInfo, max_per_task: usize) -> Vec<(String, String, String)> {
    let stack = match task.stack.as_deref() {
        Some("spring") | Some("android") => task.stack.as_deref().unwrap().to_string(),
        _ => return Vec::new(),
    };
    if task.layers.is_empty() {
        return Vec::new();
    }
    let mut ordered: Vec<String> = task.layers.iter().cloned().collect();
    ordered.sort_by_key(|l| layer_sort_key(l));
    if max_per_task > 0 {
        ordered.truncate(max_per_task);
    }
    ordered
        .into_iter()
        .map(|layer| (stack.clone(), layer, task.action.clone()))
        .collect()
}

fn candidate_keys(stack: &str, layer: &str, action: &str) -> Vec<(String, String, String)> {
    vec![
        (stack.into(), layer.into(), action.into()),
        (stack.into(), layer.into(), ANY.into()),
        (stack.into(), ANY.into(), action.into()),
        (stack.into(), ANY.into(), ANY.into()),
    ]
}

/// Greedy bucketing with stepwise back-off: try the exact (stack,
/// layer, action) key first, then drop the action, then drop the
/// layer, then fall through to (stack, ANY, ANY) — taking the first
/// candidate set that meets `min_size`. Ensures every grouped task
/// lives in at least one bucket of plausible peer comparison size,
/// instead of the alternative ("you're the only Controller-modify
/// task this project has, no peers, no group").
type StackLayerAction = (String, String, String);

fn build_groups_with_backoff(
    tasks: &[TaskInfo],
    max_per_task: usize,
    min_size: usize,
) -> BTreeMap<StackLayerAction, Vec<i64>> {
    let mut candidate_sets: HashMap<StackLayerAction, BTreeSet<i64>> = HashMap::new();
    let mut task_primaries: Vec<(i64, Vec<StackLayerAction>)> = Vec::new();

    for t in tasks {
        let primaries = task_tuples(t, max_per_task);
        if primaries.is_empty() {
            continue;
        }
        task_primaries.push((t.task_id, primaries.clone()));
        let mut seen_for_this_task: BTreeSet<(String, String, String)> = BTreeSet::new();
        for prim in &primaries {
            for cand in candidate_keys(&prim.0, &prim.1, &prim.2) {
                if !seen_for_this_task.insert(cand.clone()) {
                    continue;
                }
                candidate_sets.entry(cand).or_default().insert(t.task_id);
            }
        }
    }

    let candidate_size: HashMap<(String, String, String), usize> = candidate_sets
        .iter()
        .map(|(k, v)| (k.clone(), v.len()))
        .collect();

    let mut buckets: HashMap<(String, String, String), Vec<i64>> = HashMap::new();
    for (tid, primaries) in &task_primaries {
        for prim in primaries {
            let mut chosen: Option<(String, String, String)> = None;
            for cand in candidate_keys(&prim.0, &prim.1, &prim.2) {
                if candidate_size.get(&cand).copied().unwrap_or(0) >= min_size {
                    chosen = Some(cand);
                    break;
                }
            }
            let Some(key) = chosen else {
                continue;
            };
            buckets.entry(key).or_default().push(*tid);
        }
    }

    let mut deduped: BTreeMap<(String, String, String), Vec<i64>> = BTreeMap::new();
    for (k, mut v) in buckets {
        v.sort_unstable();
        v.dedup();
        if v.len() >= min_size {
            deduped.insert(k, v);
        }
    }
    deduped
}

fn mad_z(value: f64, median: f64, mad: f64) -> Option<f64> {
    if mad <= 0.0 {
        None
    } else {
        Some((value - median) / mad)
    }
}

/// Robust scale estimator: prefer MAD (median absolute deviation) but
/// fall back to the *mean* absolute deviation from the median when MAD
/// is zero. MAD breaks down when the majority of values are clustered
/// tightly with a single outlier — every absolute deviation in the
/// "majority" side is zero, so the median of deviations is zero too,
/// and the outlier never fires. The mean of deviations stays non-zero
/// in that scenario (the outlier contributes 1/N) while still being
/// resistant to multiple outliers (any one outlier moves the mean by
/// only |dev|/N). When *all* values are identical both estimators are
/// zero and `None` is returned, correctly silencing the detector.
fn group_scale(values: &[f64], median: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mad = stats::mad(values);
    if mad > 0.0 {
        return Some(mad);
    }
    let n = values.len() as f64;
    let mean_abs = values.iter().map(|v| (v - median).abs()).sum::<f64>() / n;
    if mean_abs > 0.0 {
        Some(mean_abs)
    } else {
        None
    }
}

#[derive(Debug, Default, Clone)]
struct OutlierInfo {
    is_outlier: bool,
    outlier_reason: Option<String>,
    stmts_per_point_deviation: Option<f64>,
}

/// Density-only outlier detection. Operates on the group's
/// `stmts_normalized / points` distribution; |MAD-z| above the
/// configured `mad_k_threshold` flags. Tasks with zero points or
/// missing stmts are skipped (we report `None` for the deviation so
/// the renderer can elide the cell instead of writing 0).
fn detect_outliers(
    task_map: &HashMap<i64, &TaskInfo>,
    member_ids: &[i64],
    cfg: &RepoAnalysisConfig,
) -> HashMap<i64, OutlierInfo> {
    let members: Vec<&&TaskInfo> = member_ids.iter().filter_map(|i| task_map.get(i)).collect();

    let stmts_per_pt_values: Vec<f64> = members
        .iter()
        .filter_map(|m| {
            let stmts = m.stmts_surviving_normalized?;
            let pt = m.estimation_points?;
            if pt > 0.0 {
                Some(stmts / pt)
            } else {
                None
            }
        })
        .collect();

    let median = if !stmts_per_pt_values.is_empty() {
        Some(stats::median(&stmts_per_pt_values))
    } else {
        None
    };
    // Below 3 samples the scale estimator is unstable, so we silence
    // the detector. Above that, prefer MAD; fall back to mean absolute
    // deviation when MAD = 0 (tight cluster + lone outlier) so the
    // discordant task still fires.
    let scale = if stmts_per_pt_values.len() >= 3 {
        median.and_then(|m| group_scale(&stmts_per_pt_values, m))
    } else {
        None
    };
    let k = cfg.mad_k_threshold;

    let mut out = HashMap::new();
    for m in &members {
        let mut info = OutlierInfo::default();
        if let (Some(stmts), Some(pt), Some(med), Some(s)) = (
            m.stmts_surviving_normalized,
            m.estimation_points,
            median,
            scale,
        ) {
            if pt > 0.0 && s > 0.0 {
                let value = stmts / pt;
                if let Some(z) = mad_z(value, med, s) {
                    info.stmts_per_point_deviation = Some(z);
                    if z.abs() > k {
                        info.is_outlier = true;
                        info.outlier_reason = Some(format!(
                            "stmts/pt={} vs median={} (z={:+.1})",
                            fmt_float(value, 2),
                            fmt_float(med, 2),
                            z
                        ));
                    }
                }
            }
        }
        out.insert(m.task_id, info);
    }
    out
}

fn purge_project(conn: &Connection, project_id: i64) -> rusqlite::Result<()> {
    // Delete members first via the foreign key so groups can be
    // rewritten cleanly. The schema's FK constraint isn't enforced
    // (cascade delete is off in this codebase) but doing it in this
    // order is still correct.
    conn.execute(
        "DELETE FROM task_group_members
         WHERE group_id IN (SELECT group_id FROM task_similarity_groups
                            WHERE project_id = ?)",
        [project_id],
    )?;
    conn.execute(
        "DELETE FROM task_similarity_groups WHERE project_id = ?",
        [project_id],
    )?;
    Ok(())
}

fn pick_representative(task_map: &HashMap<i64, &TaskInfo>, members: &[i64]) -> i64 {
    members
        .iter()
        .copied()
        .max_by(|a, b| {
            let ta = task_map.get(a).unwrap();
            let tb = task_map.get(b).unwrap();
            let a_stmts = ta.stmts_surviving_normalized.unwrap_or(-1.0);
            let b_stmts = tb.stmts_surviving_normalized.unwrap_or(-1.0);
            let a_pt = ta.estimation_points.unwrap_or(-1.0);
            let b_pt = tb.estimation_points.unwrap_or(-1.0);
            (a_stmts, a_pt, -ta.task_id)
                .partial_cmp(&(b_stmts, b_pt, -tb.task_id))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap()
}

pub struct TaskSimilaritySummary {
    pub task_count: usize,
    pub group_count: usize,
    pub outlier_count: usize,
    pub skipped: bool,
}

/// Compute peer-group analysis at the project level. `sprint_ids` is
/// the set of sprints to consider (typically every sprint in the
/// project with `start_date <= today`); we deliberately don't query
/// `sprints` here so callers can constrain the window when needed.
pub fn compute_task_similarity(
    conn: &Connection,
    project_id: i64,
    sprint_ids: &[i64],
    cfg: &RepoAnalysisConfig,
) -> rusqlite::Result<TaskSimilaritySummary> {
    if !cfg.enable_task_similarity {
        return Ok(TaskSimilaritySummary {
            task_count: 0,
            group_count: 0,
            outlier_count: 0,
            skipped: true,
        });
    }
    if sprint_ids.is_empty() {
        purge_project(conn, project_id)?;
        return Ok(TaskSimilaritySummary {
            task_count: 0,
            group_count: 0,
            outlier_count: 0,
            skipped: false,
        });
    }

    let tasks = load_tasks_for_project(conn, project_id, sprint_ids)?;
    if tasks.len() < 2 {
        purge_project(conn, project_id)?;
        return Ok(TaskSimilaritySummary {
            task_count: tasks.len(),
            group_count: 0,
            outlier_count: 0,
            skipped: false,
        });
    }

    let task_map: HashMap<i64, &TaskInfo> = tasks.iter().map(|t| (t.task_id, t)).collect();
    let groups_raw = build_groups_with_backoff(
        &tasks,
        cfg.max_clusters_per_task as usize,
        cfg.group_min_size as usize,
    );
    purge_project(conn, project_id)?;

    let mut outlier_total = 0usize;
    let mut group_count = 0usize;

    let mut keys: Vec<(String, String, String)> = groups_raw.keys().cloned().collect();
    keys.sort_by(|a, b| {
        let la = if a.1 == ANY {
            LAYER_ORDER.len() + 1
        } else {
            layer_sort_key(&a.1)
        };
        let lb = if b.1 == ANY {
            LAYER_ORDER.len() + 1
        } else {
            layer_sort_key(&b.1)
        };
        let aa = if a.2 == "create" {
            0
        } else if a.2 == "modify" {
            1
        } else {
            2
        };
        let ab = if b.2 == "create" {
            0
        } else if b.2 == "modify" {
            1
        } else {
            2
        };
        (la, aa, &a.0).cmp(&(lb, ab, &b.0))
    });

    for key in keys {
        let members = &groups_raw[&key];
        let (stack, layer, action) = (&key.0, &key.1, &key.2);

        let pts: Vec<f64> = members
            .iter()
            .filter_map(|i| task_map.get(i).and_then(|t| t.estimation_points))
            .collect();
        let lss: Vec<f64> = members
            .iter()
            .filter_map(|i| task_map.get(i).and_then(|t| t.ls))
            .collect();
        let ls_per_pts: Vec<f64> = members
            .iter()
            .filter_map(|i| {
                let t = task_map.get(i)?;
                let ls = t.ls?;
                let pt = t.estimation_points?;
                if pt > 0.0 {
                    Some(ls / pt)
                } else {
                    None
                }
            })
            .collect();
        let stmts_per_pts: Vec<f64> = members
            .iter()
            .filter_map(|i| {
                let t = task_map.get(i)?;
                let stmts = t.stmts_surviving_normalized?;
                let pt = t.estimation_points?;
                if pt > 0.0 {
                    Some(stmts / pt)
                } else {
                    None
                }
            })
            .collect();

        let median_pts = if pts.is_empty() {
            None
        } else {
            Some(stats::median(&pts))
        };
        let median_ls = if lss.is_empty() {
            None
        } else {
            Some(stats::median(&lss))
        };
        let median_ls_per_pt = if ls_per_pts.is_empty() {
            None
        } else {
            Some(stats::median(&ls_per_pts))
        };
        let median_stmts_per_pt = if stmts_per_pts.is_empty() {
            None
        } else {
            Some(stats::median(&stmts_per_pts))
        };

        let rep = pick_representative(&task_map, members);
        let label = build_label(stack, layer, action);

        conn.execute(
            "INSERT INTO task_similarity_groups
             (project_id, representative_task_id, group_label,
              stack, layer, action, member_count,
              median_points, median_ls, median_ls_per_point,
              median_stmts_per_point)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                project_id,
                rep,
                label,
                stack,
                layer,
                action,
                members.len() as i64,
                median_pts,
                median_ls,
                median_ls_per_pt,
                median_stmts_per_pt,
            ],
        )?;
        let group_id = conn.last_insert_rowid();
        group_count += 1;

        let outliers = detect_outliers(&task_map, members, cfg);
        for tid in members {
            let info = outliers.get(tid).cloned().unwrap_or_default();
            if info.is_outlier {
                outlier_total += 1;
            }
            conn.execute(
                "INSERT INTO task_group_members
                 (group_id, task_id, is_outlier, outlier_reason,
                  stmts_per_point_deviation)
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    group_id,
                    tid,
                    if info.is_outlier { 1 } else { 0 },
                    info.outlier_reason,
                    info.stmts_per_point_deviation,
                ],
            )?;
        }
    }

    let _ = stack_display;
    let _ = action_display;
    info!(
        project_id,
        sprints = sprint_ids.len(),
        tasks = tasks.len(),
        groups = group_count,
        outliers = outlier_total,
        "peer-group analysis"
    );
    Ok(TaskSimilaritySummary {
        task_count: tasks.len(),
        group_count,
        outlier_count: outlier_total,
        skipped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mk_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        sprint_grader_core::db::apply_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn mad_zero_on_identical_values() {
        assert_eq!(stats::mad(&[3.0, 3.0, 3.0]), 0.0);
    }

    #[test]
    fn mad_nonzero_on_spread() {
        let m = stats::mad(&[1.0, 1.0, 2.0, 2.0, 4.0, 6.0, 9.0]);
        assert!((m - 1.0).abs() < 1e-9);
    }

    #[test]
    fn label_wildcard_forms() {
        assert_eq!(
            build_label("spring", "spring_controller", "create"),
            "Spring — Create Controller method"
        );
        assert_eq!(build_label("android", "*", "*"), "Android — (any kind)");
        assert_eq!(
            build_label("spring", "*", "modify"),
            "Spring — Modify (any layer)"
        );
    }

    #[test]
    fn combine_repo_types_detects_mixed() {
        assert_eq!(
            combine_repo_types(&[Some("spring"), Some("android")]),
            Some("mixed".into())
        );
        assert_eq!(
            combine_repo_types(&[Some("spring"), Some("spring")]),
            Some("spring".into())
        );
        assert_eq!(combine_repo_types(&[None, None]), None);
    }

    #[test]
    fn pr_action_maps_diff_ratio_to_create_or_modify() {
        let mostly_add = PrSignature {
            lat: Some(100.0),
            ld: Some(10.0),
            ..Default::default()
        };
        assert_eq!(pr_action(&mostly_add), Some("create"));

        let balanced = PrSignature {
            lat: Some(60.0),
            ld: Some(40.0),
            ..Default::default()
        };
        assert_eq!(pr_action(&balanced), Some("modify"));

        let no_diff = PrSignature::default();
        assert_eq!(pr_action(&no_diff), None);
    }

    #[test]
    fn dominant_layers_picks_top_when_no_threshold_crossed() {
        // No layer crosses 25 % when split 30/30/30/10 — we still want
        // *some* classification, so the single top layer wins.
        let mut counts = HashMap::new();
        counts.insert("a".to_string(), 30);
        counts.insert("b".to_string(), 30);
        counts.insert("c".to_string(), 30);
        counts.insert("d".to_string(), 10);
        let layers = dominant_layers(&counts);
        assert_eq!(layers.len(), 3, "got {layers:?}");
        // Unrelated weakest layer shouldn't appear.
        assert!(!layers.contains(&"d".to_string()));
    }

    #[test]
    fn dominant_layers_drops_long_tail_below_threshold() {
        let mut counts = HashMap::new();
        counts.insert("controller".to_string(), 40);
        counts.insert("service".to_string(), 30);
        counts.insert("dto".to_string(), 5);
        let layers = dominant_layers(&counts);
        assert!(layers.contains(&"controller".to_string()));
        assert!(layers.contains(&"service".to_string()));
        assert!(
            !layers.contains(&"dto".to_string()),
            "long-tail layer leaked into the dominant set: {layers:?}"
        );
    }

    fn seed_project(conn: &Connection, project_id: i64) {
        conn.execute(
            "INSERT INTO projects (id, slug, name) VALUES (?, ?, ?)",
            params![project_id, "team-test", "Test Project"],
        )
        .unwrap();
    }

    fn seed_sprint(conn: &Connection, sid: i64, project_id: i64, n: u32) {
        conn.execute(
            "INSERT INTO sprints (id, project_id, name, start_date, end_date) VALUES (?, ?, ?, ?, ?)",
            params![
                sid,
                project_id,
                format!("Sprint {n}"),
                "2026-02-01",
                "2026-02-15"
            ],
        )
        .unwrap();
    }

    fn seed_done_task(conn: &Connection, tid: i64, sprint_id: i64, points: i64) {
        conn.execute(
            "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                                assignee_id, sprint_id, parent_task_id)
             VALUES (?, ?, ?, 'TASK', 'DONE', ?, NULL, ?, NULL)",
            params![
                tid,
                format!("T-{tid}"),
                format!("Task {tid}"),
                points,
                sprint_id
            ],
        )
        .unwrap();
    }

    fn seed_pr(conn: &Connection, id: &str, repo: &str, merged: bool) {
        conn.execute(
            "INSERT INTO pull_requests
             (id, pr_number, repo_full_name, url, title, state, merged)
             VALUES (?, 1, ?, '', '', ?, ?)",
            params![
                id,
                repo,
                if merged { "merged" } else { "open" },
                if merged { 1 } else { 0 },
            ],
        )
        .unwrap();
    }

    fn link_task_pr(conn: &Connection, tid: i64, pr_id: &str) {
        conn.execute(
            "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (?, ?)",
            params![tid, pr_id],
        )
        .unwrap();
    }

    fn seed_pr_commit(conn: &Connection, pr_id: &str, sha: &str) {
        conn.execute(
            "INSERT INTO pr_commits (pr_id, sha, author_login, message, timestamp)
             VALUES (?, ?, 'someone', 'msg', '2026-02-05T10:00:00Z')",
            params![pr_id, sha],
        )
        .unwrap();
    }

    fn seed_fingerprint(conn: &Connection, sprint_id: i64, repo: &str, sha: &str, path: &str) {
        conn.execute(
            "INSERT INTO fingerprints (file_path, repo_full_name, statement_index,
                                       method_name, raw_fingerprint, normalized_fingerprint,
                                       method_fingerprint, blame_commit, blame_author_login, sprint_id)
             VALUES (?, ?, 0, 'm', 'r', 'n', 'mf', ?, 'someone', ?)",
            params![path, repo, sha, sprint_id],
        )
        .unwrap();
    }

    fn seed_pr_line_metrics(conn: &Connection, pr_id: &str, sprint_id: i64, lat: i64, ld: i64) {
        conn.execute(
            "INSERT INTO pr_line_metrics (pr_id, sprint_id, lat, lar, ls, ld)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![pr_id, sprint_id, lat, lat, lat, ld],
        )
        .unwrap();
    }

    fn seed_pr_survival(conn: &Connection, pr_id: &str, sprint_id: i64, surv: i64) {
        conn.execute(
            "INSERT INTO pr_survival
             (pr_id, sprint_id, statements_added_raw, statements_surviving_raw,
              statements_added_normalized, statements_surviving_normalized,
              methods_added, methods_surviving)
             VALUES (?, ?, ?, ?, ?, ?, 1, 1)",
            params![pr_id, sprint_id, surv, surv, surv, surv],
        )
        .unwrap();
    }

    /// End-to-end happy path: a project with three Spring controller
    /// tasks and three service tasks should produce two groups, each
    /// with three members and zero outliers (densities are identical
    /// inside each group). Verifies the path → layer pipeline,
    /// project-scope load, and density-only outlier detection.
    #[test]
    fn end_to_end_project_clustering_groups_by_path_layer() {
        let conn = mk_db();
        seed_project(&conn, 1);
        seed_sprint(&conn, 10, 1, 1);
        seed_sprint(&conn, 11, 1, 2);

        let cases: Vec<(i64, i64, &str, &str, &str)> = vec![
            (
                100,
                10,
                "pr-100",
                "abc100",
                "src/main/java/foo/controller/AController.java",
            ),
            (
                101,
                10,
                "pr-101",
                "abc101",
                "src/main/java/foo/controller/BController.java",
            ),
            (
                102,
                11,
                "pr-102",
                "abc102",
                "src/main/java/foo/controller/CController.java",
            ),
            (
                103,
                10,
                "pr-103",
                "abc103",
                "src/main/java/foo/service/AService.java",
            ),
            (
                104,
                11,
                "pr-104",
                "abc104",
                "src/main/java/foo/service/BService.java",
            ),
            (
                105,
                11,
                "pr-105",
                "abc105",
                "src/main/java/foo/service/CService.java",
            ),
        ];

        for (tid, sid, pr, sha, path) in &cases {
            seed_done_task(&conn, *tid, *sid, 5);
            seed_pr(&conn, pr, "udg-pds/spring-test", true);
            link_task_pr(&conn, *tid, pr);
            seed_pr_commit(&conn, pr, sha);
            seed_fingerprint(&conn, *sid, "udg-pds/spring-test", sha, path);
            seed_pr_line_metrics(&conn, pr, *sid, 100, 10);
            seed_pr_survival(&conn, pr, *sid, 20);
        }

        let cfg = RepoAnalysisConfig {
            group_min_size: 3,
            ..Default::default()
        };
        let summary = compute_task_similarity(&conn, 1, &[10, 11], &cfg).unwrap();

        assert_eq!(summary.task_count, 6);
        assert_eq!(
            summary.group_count, 2,
            "expected one controller + one service group"
        );
        assert_eq!(
            summary.outlier_count, 0,
            "uniform densities must produce no outliers"
        );

        let labels: Vec<String> = conn
            .prepare("SELECT group_label FROM task_similarity_groups WHERE project_id = 1 ORDER BY group_label")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            labels.iter().any(|l| l.contains("Controller")),
            "controller group missing; got: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Service")),
            "service group missing; got: {labels:?}"
        );
    }

    /// Density discordance fires for the one task that delivered far
    /// more surviving statements per point than its peers. The other
    /// metrics (raw points, raw LS) are uniform — confirms this is a
    /// density-only check.
    #[test]
    fn density_outlier_fires_when_stmts_per_point_diverges() {
        let conn = mk_db();
        seed_project(&conn, 1);
        seed_sprint(&conn, 10, 1, 1);

        // Five controller tasks, all 5 points each.
        for tid in 100..105 {
            let pr = format!("pr-{tid}");
            let sha = format!("sha-{tid}");
            seed_done_task(&conn, tid, 10, 5);
            seed_pr(&conn, &pr, "udg-pds/spring-test", true);
            link_task_pr(&conn, tid, &pr);
            seed_pr_commit(&conn, &pr, &sha);
            seed_fingerprint(
                &conn,
                10,
                "udg-pds/spring-test",
                &sha,
                "src/main/java/foo/controller/X.java",
            );
            seed_pr_line_metrics(&conn, &pr, 10, 100, 10);
            // Four tasks at 10 stmts; one (tid 104) at 100 stmts → 20 stmts/pt vs 2.
            let surv = if tid == 104 { 100 } else { 10 };
            seed_pr_survival(&conn, &pr, 10, surv);
        }

        let cfg = RepoAnalysisConfig {
            group_min_size: 3,
            mad_k_threshold: 2.0,
            ..Default::default()
        };
        let summary = compute_task_similarity(&conn, 1, &[10], &cfg).unwrap();

        assert_eq!(
            summary.outlier_count, 1,
            "exactly one density outlier expected"
        );
        let outlier_tid: i64 = conn
            .query_row(
                "SELECT task_id FROM task_group_members WHERE is_outlier = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(outlier_tid, 104);

        // The reason string mentions stmts/pt, never raw points or LS.
        let reason: String = conn
            .query_row(
                "SELECT outlier_reason FROM task_group_members WHERE is_outlier = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(reason.contains("stmts/pt"), "reason: {reason:?}");
        assert!(
            !reason.contains("LS=") && !reason.contains("points="),
            "non-density signal leaked into reason: {reason:?}"
        );
    }

    /// A task with zero linked PRs, or with any open PR, must be
    /// excluded from grouping. Both states distort densities.
    #[test]
    fn open_pr_or_no_pr_tasks_are_excluded() {
        let conn = mk_db();
        seed_project(&conn, 1);
        seed_sprint(&conn, 10, 1, 1);

        // Three valid tasks (DONE, every PR merged).
        for tid in 100..103 {
            let pr = format!("pr-{tid}");
            let sha = format!("sha-{tid}");
            seed_done_task(&conn, tid, 10, 5);
            seed_pr(&conn, &pr, "udg-pds/spring-test", true);
            link_task_pr(&conn, tid, &pr);
            seed_pr_commit(&conn, &pr, &sha);
            seed_fingerprint(
                &conn,
                10,
                "udg-pds/spring-test",
                &sha,
                "src/main/java/foo/controller/X.java",
            );
            seed_pr_line_metrics(&conn, &pr, 10, 100, 10);
            seed_pr_survival(&conn, &pr, 10, 10);
        }
        // Excluded: DONE but no PR linked.
        seed_done_task(&conn, 200, 10, 5);
        // Excluded: DONE with one merged + one open PR.
        seed_done_task(&conn, 201, 10, 5);
        seed_pr(&conn, "pr-201a", "udg-pds/spring-test", true);
        seed_pr(&conn, "pr-201b", "udg-pds/spring-test", false);
        link_task_pr(&conn, 201, "pr-201a");
        link_task_pr(&conn, 201, "pr-201b");
        seed_pr_commit(&conn, "pr-201a", "sha-201a");
        seed_fingerprint(
            &conn,
            10,
            "udg-pds/spring-test",
            "sha-201a",
            "src/main/java/foo/controller/X.java",
        );
        seed_pr_line_metrics(&conn, "pr-201a", 10, 100, 10);
        seed_pr_survival(&conn, "pr-201a", 10, 10);

        let cfg = RepoAnalysisConfig {
            group_min_size: 3,
            ..Default::default()
        };
        let summary = compute_task_similarity(&conn, 1, &[10], &cfg).unwrap();
        assert_eq!(summary.task_count, 3, "open / no-PR tasks must be excluded");
    }

    /// Re-running on the same DB must replace the project's prior rows
    /// rather than accumulating duplicates.
    #[test]
    fn rerun_replaces_prior_groups_idempotently() {
        let conn = mk_db();
        seed_project(&conn, 1);
        seed_sprint(&conn, 10, 1, 1);
        for tid in 100..103 {
            let pr = format!("pr-{tid}");
            let sha = format!("sha-{tid}");
            seed_done_task(&conn, tid, 10, 5);
            seed_pr(&conn, &pr, "udg-pds/spring-test", true);
            link_task_pr(&conn, tid, &pr);
            seed_pr_commit(&conn, &pr, &sha);
            seed_fingerprint(
                &conn,
                10,
                "udg-pds/spring-test",
                &sha,
                "src/main/java/foo/controller/X.java",
            );
            seed_pr_line_metrics(&conn, &pr, 10, 100, 10);
            seed_pr_survival(&conn, &pr, 10, 10);
        }
        let cfg = RepoAnalysisConfig {
            group_min_size: 3,
            ..Default::default()
        };
        compute_task_similarity(&conn, 1, &[10], &cfg).unwrap();
        let first: i64 = conn
            .query_row(
                "SELECT count(*) FROM task_similarity_groups WHERE project_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        compute_task_similarity(&conn, 1, &[10], &cfg).unwrap();
        let second: i64 = conn
            .query_row(
                "SELECT count(*) FROM task_similarity_groups WHERE project_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(first, second, "second run must not duplicate");
    }
}
