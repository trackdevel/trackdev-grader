//! Task similarity grouping by (stack, layer, action) with MAD-based outlier
//! detection. Mirrors `src/repo_analysis/task_similarity.py`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{params, Connection};
use sprint_grader_core::config::RepoAnalysisConfig;
use sprint_grader_core::formatting::fmt_float;
use sprint_grader_core::stats;
use tracing::info;

use crate::keywords::{action_tag, layer_tags, tokenize};

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
    name: String,
    project_id: Option<i64>,
    estimation_points: Option<f64>,
    #[allow(dead_code)]
    tokens: Vec<String>,
    stack: Option<String>,
    layers: BTreeSet<String>,
    action: String,
    lar: Option<f64>,
    #[allow(dead_code)]
    lat: Option<f64>,
    ls: Option<f64>,
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

fn load_tasks_for_sprint(conn: &Connection, sprint_id: i64) -> rusqlite::Result<Vec<TaskInfo>> {
    let project_id: Option<i64> = conn
        .query_row(
            "SELECT project_id FROM sprints WHERE id = ?",
            [sprint_id],
            |r| r.get(0),
        )
        .ok();

    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.estimation_points
         FROM tasks t
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND t.status = 'DONE'",
    )?;
    let task_rows: Vec<(i64, Option<String>, Option<f64>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<f64>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if task_rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut parent_map: HashMap<i64, String> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT t.id, p.name
         FROM tasks t
         LEFT JOIN tasks p ON p.id = t.parent_task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND t.status = 'DONE' AND p.name IS NOT NULL",
    )?;
    for row in stmt
        .query_map([sprint_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        parent_map.insert(row.0, row.1);
    }
    drop(stmt);

    let mut pr_links: HashMap<i64, Vec<(String, Option<String>)>> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT tpr.task_id, tpr.pr_id, pr.repo_full_name
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         LEFT JOIN pull_requests pr ON pr.id = tpr.pr_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND t.status = 'DONE'",
    )?;
    for row in stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        pr_links.entry(row.0).or_default().push((row.1, row.2));
    }
    drop(stmt);

    let mut pr_metrics: HashMap<String, (Option<f64>, Option<f64>, Option<f64>)> = HashMap::new();
    let mut stmt =
        conn.prepare("SELECT pr_id, lar, lat, ls FROM pr_line_metrics WHERE sprint_id = ?")?;
    for row in stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, Option<f64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        pr_metrics.insert(row.0, (row.1, row.2, row.3));
    }
    drop(stmt);

    let mut pr_total_points: HashMap<String, f64> = HashMap::new();
    let mut pr_task_count: HashMap<String, i64> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT tpr.pr_id,
                COALESCE(SUM(COALESCE(t.estimation_points, 0)), 0),
                COUNT(*)
         FROM task_pull_requests tpr
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND t.status = 'DONE'
         GROUP BY tpr.pr_id",
    )?;
    for row in stmt
        .query_map([sprint_id], |r| {
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

    let mut infos = Vec::with_capacity(task_rows.len());
    for (tid, name, pts) in task_rows {
        let name_str = name.unwrap_or_default();
        let parent = parent_map.get(&tid).cloned().unwrap_or_default();
        let text = format!("{} {}", name_str, parent);
        let tokens = tokenize(Some(&text));

        let links = pr_links.remove(&tid).unwrap_or_default();
        let types: Vec<Option<&'static str>> = links
            .iter()
            .map(|(_, repo)| infer_repo_type(repo.as_deref()))
            .collect();
        let stack = combine_repo_types(&types);

        let mut layers = layer_tags(&tokens, stack.as_deref());
        if matches!(stack.as_deref(), Some("spring") | Some("android")) && layers.is_empty() {
            layers.insert(format!("{}_other", stack.as_deref().unwrap()));
        }
        let layers_sorted: BTreeSet<String> = layers.into_iter().collect();
        let action = action_tag(&tokens).to_string();

        let task_pts = pts.unwrap_or(0.0);
        let mut lar_total = 0.0;
        let mut lar_seen = false;
        let mut lat_total = 0.0;
        let mut lat_seen = false;
        let mut ls_total = 0.0;
        let mut ls_seen = false;
        for (pr_id, _) in &links {
            let Some(m) = pr_metrics.get(pr_id) else {
                continue;
            };
            let tot = pr_total_points.get(pr_id).copied().unwrap_or(0.0);
            let count = pr_task_count.get(pr_id).copied().unwrap_or(1).max(1);
            let weight = if tot > 0.0 {
                task_pts / tot
            } else {
                1.0 / count as f64
            };
            if let Some(v) = m.0 {
                lar_total += v * weight;
                lar_seen = true;
            }
            if let Some(v) = m.1 {
                lat_total += v * weight;
                lat_seen = true;
            }
            if let Some(v) = m.2 {
                ls_total += v * weight;
                ls_seen = true;
            }
        }

        infos.push(TaskInfo {
            task_id: tid,
            name: name_str,
            project_id,
            estimation_points: pts,
            tokens,
            stack,
            layers: layers_sorted,
            action,
            lar: if lar_seen { Some(lar_total) } else { None },
            lat: if lat_seen { Some(lat_total) } else { None },
            ls: if ls_seen { Some(ls_total) } else { None },
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

fn build_groups_with_backoff(
    tasks: &[TaskInfo],
    max_per_task: usize,
    min_size: usize,
) -> BTreeMap<(String, String, String), Vec<i64>> {
    let mut candidate_sets: HashMap<(String, String, String), BTreeSet<i64>> = HashMap::new();
    let mut task_primaries: Vec<(i64, Vec<(String, String, String)>)> = Vec::new();

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

#[derive(Debug, Default, Clone)]
struct OutlierInfo {
    is_outlier: bool,
    outlier_reason: Option<String>,
    points_deviation: Option<f64>,
    lar_deviation: Option<f64>,
    ls_deviation: Option<f64>,
    ls_per_point_deviation: Option<f64>,
}

fn detect_outliers(
    task_map: &HashMap<i64, &TaskInfo>,
    member_ids: &[i64],
    cfg: &RepoAnalysisConfig,
) -> HashMap<i64, OutlierInfo> {
    let members: Vec<&&TaskInfo> = member_ids.iter().filter_map(|i| task_map.get(i)).collect();

    let pts_values: Vec<f64> = members
        .iter()
        .filter_map(|m| m.estimation_points.filter(|v| *v > 0.0))
        .collect();
    let ls_values: Vec<f64> = members.iter().filter_map(|m| m.ls).collect();
    let ls_per_pt_values: Vec<f64> = members
        .iter()
        .filter_map(|m| {
            let ls = m.ls?;
            let pt = m.estimation_points?;
            if pt > 0.0 {
                Some(ls / pt)
            } else {
                None
            }
        })
        .collect();

    let pts_median = if !pts_values.is_empty() {
        Some(stats::median(&pts_values))
    } else {
        None
    };
    let ls_median = if !ls_values.is_empty() {
        Some(stats::median(&ls_values))
    } else {
        None
    };
    let ls_per_pt_median = if !ls_per_pt_values.is_empty() {
        Some(stats::median(&ls_per_pt_values))
    } else {
        None
    };

    let pts_mad = if pts_values.len() >= 3 {
        stats::mad(&pts_values)
    } else {
        0.0
    };
    let ls_mad = if ls_values.len() >= 3 {
        stats::mad(&ls_values)
    } else {
        0.0
    };
    let ls_per_pt_mad = if ls_per_pt_values.len() >= 3 {
        stats::mad(&ls_per_pt_values)
    } else {
        0.0
    };

    let k = cfg.mad_k_threshold;
    let mut out = HashMap::new();

    for m in &members {
        let mut info = OutlierInfo::default();
        let mut reasons: Vec<String> = Vec::new();

        if let (Some(v), Some(med)) = (m.estimation_points, pts_median) {
            if pts_mad > 0.0 {
                if let Some(z) = mad_z(v, med, pts_mad) {
                    info.points_deviation = Some(z);
                    if z.abs() > k {
                        reasons.push(format!(
                            "points={} vs median={} (z={:+.1})",
                            fmt_float(v, 1),
                            fmt_float(med, 1),
                            z
                        ));
                    }
                }
            }
        }

        if let (Some(v), Some(med)) = (m.ls, ls_median) {
            if ls_mad > 0.0 {
                if let Some(z) = mad_z(v, med, ls_mad) {
                    info.ls_deviation = Some(z);
                    if z.abs() > k {
                        reasons.push(format!(
                            "LS={} vs median={} (z={:+.1})",
                            fmt_float(v, 2),
                            fmt_float(med, 2),
                            z
                        ));
                    }
                }
            }
        }

        if let (Some(ls), Some(pt), Some(med)) = (m.ls, m.estimation_points, ls_per_pt_median) {
            if pt > 0.0 && ls_per_pt_mad > 0.0 {
                let value = ls / pt;
                if let Some(z) = mad_z(value, med, ls_per_pt_mad) {
                    info.ls_per_point_deviation = Some(z);
                    if z.abs() > k {
                        reasons.push(format!(
                            "LS/pt={} vs median={} (z={:+.1})",
                            fmt_float(value, 2),
                            fmt_float(med, 2),
                            z
                        ));
                    }
                }
            }
        }

        if !reasons.is_empty() {
            info.is_outlier = true;
            info.outlier_reason = Some(reasons.join("; "));
        }
        out.insert(m.task_id, info);
    }
    out
}

fn purge_sprint(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM task_group_members WHERE sprint_id = ?",
        [sprint_id],
    )?;
    conn.execute(
        "DELETE FROM task_similarity_groups WHERE sprint_id = ?",
        [sprint_id],
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
            let a_ls = ta.ls.unwrap_or(-1.0);
            let b_ls = tb.ls.unwrap_or(-1.0);
            let a_pt = ta.estimation_points.unwrap_or(-1.0);
            let b_pt = tb.estimation_points.unwrap_or(-1.0);
            (a_ls, a_pt, -ta.task_id)
                .partial_cmp(&(b_ls, b_pt, -tb.task_id))
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

pub fn compute_task_similarity(
    conn: &Connection,
    sprint_id: i64,
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

    let tasks = load_tasks_for_sprint(conn, sprint_id)?;
    if tasks.len() < 2 {
        purge_sprint(conn, sprint_id)?;
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

    purge_sprint(conn, sprint_id)?;

    let mut outlier_total = 0;
    let mut group_count = 0;

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
        let lars: Vec<f64> = members
            .iter()
            .filter_map(|i| task_map.get(i).and_then(|t| t.lar))
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

        let median_pts = if pts.is_empty() {
            None
        } else {
            Some(stats::median(&pts))
        };
        let median_lar = if lars.is_empty() {
            None
        } else {
            Some(stats::median(&lars))
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

        let rep = pick_representative(&task_map, members);
        let label = build_label(stack, layer, action);

        let rep_project: Option<i64> = task_map.get(&rep).and_then(|t| t.project_id);

        conn.execute(
            "INSERT INTO task_similarity_groups
             (sprint_id, project_id, representative_task_id, group_label,
              stack, layer, action,
              member_count, median_points, median_lar,
              median_ls, median_ls_per_point)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                sprint_id,
                rep_project,
                rep,
                label,
                stack,
                layer,
                action,
                members.len() as i64,
                median_pts,
                median_lar,
                median_ls,
                median_ls_per_pt,
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
                 (group_id, task_id, sprint_id,
                  is_outlier, outlier_reason,
                  points_deviation, lar_deviation,
                  ls_deviation, ls_per_point_deviation)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    group_id,
                    tid,
                    sprint_id,
                    if info.is_outlier { 1 } else { 0 },
                    info.outlier_reason,
                    info.points_deviation,
                    info.lar_deviation,
                    info.ls_deviation,
                    info.ls_per_point_deviation,
                ],
            )?;
        }
    }

    let _ = stack_display;
    let _ = action_display;
    info!(
        sprint_id,
        tasks = tasks.len(),
        groups = group_count,
        outliers = outlier_total,
        "task similarity"
    );
    let task_count = tasks.len();
    // silence unused
    let _ = tasks.iter().map(|t| &t.name).count();
    Ok(TaskSimilaritySummary {
        task_count,
        group_count,
        outlier_count: outlier_total,
        skipped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mad_zero_on_identical_values() {
        assert_eq!(stats::mad(&[3.0, 3.0, 3.0]), 0.0);
    }

    #[test]
    fn mad_nonzero_on_spread() {
        // [1,1,2,2,4,6,9] → median 2, deviations [1,1,0,0,2,4,7] → median 1
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
}
