//! Team review network metrics via `petgraph`. Mirrors
//! `src/process/collaboration.py` (which has a networkx path + a pure-Python
//! fallback; we only implement the graph-library path).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sprint_grader_core::stats::round_half_even;
use tracing::info;

struct ReviewGraph {
    nodes: Vec<String>,
    node_idx: HashMap<String, NodeIndex>,
    graph: DiGraph<String, i64>,
}

fn build_review_graph(
    conn: &Connection,
    project_id: i64,
    sprint_id: i64,
) -> rusqlite::Result<ReviewGraph> {
    // Team members (with github_login).
    let mut stmt =
        conn.prepare("SELECT id, github_login FROM students WHERE team_project_id = ?")?;
    let members: Vec<(String, Option<String>)> = stmt
        .query_map([project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut graph: DiGraph<String, i64> = DiGraph::new();
    let mut node_idx: HashMap<String, NodeIndex> = HashMap::new();
    let mut nodes_vec: Vec<String> = Vec::new();
    for (id, _) in &members {
        let idx = graph.add_node(id.clone());
        node_idx.insert(id.clone(), idx);
        nodes_vec.push(id.clone());
    }
    let login_to_id: HashMap<String, String> = members
        .iter()
        .filter_map(|(id, login)| login.as_ref().map(|l| (l.to_lowercase(), id.clone())))
        .collect();

    let mut stmt = conn.prepare(
        "SELECT rv.reviewer_login, pr.author_id
         FROM pr_reviews rv
         JOIN pull_requests pr ON pr.id = rv.pr_id
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         WHERE t.sprint_id = ? AND t.type != 'USER_STORY'
           AND rv.reviewer_login IS NOT NULL",
    )?;
    let reviews: Vec<(Option<String>, Option<String>)> = stmt
        .query_map([sprint_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    // Aggregate edge weights: (reviewer_id, author_id) → count.
    let mut edge_weights: HashMap<(String, String), i64> = HashMap::new();
    for (login, author) in reviews {
        let reviewer_id = match login
            .as_ref()
            .and_then(|l| login_to_id.get(&l.to_lowercase()))
        {
            Some(s) => s.clone(),
            None => continue,
        };
        let author_id = match author {
            Some(a) => a,
            None => continue,
        };
        if reviewer_id == author_id {
            continue;
        }
        if !node_idx.contains_key(&reviewer_id) || !node_idx.contains_key(&author_id) {
            continue;
        }
        *edge_weights.entry((reviewer_id, author_id)).or_insert(0) += 1;
    }
    for ((r, a), w) in edge_weights {
        let ri = node_idx[&r];
        let ai = node_idx[&a];
        graph.add_edge(ri, ai, w);
    }

    Ok(ReviewGraph {
        nodes: nodes_vec,
        node_idx,
        graph,
    })
}

fn density(n: usize, edges: usize) -> f64 {
    if n < 2 {
        return 0.0;
    }
    let possible = n * (n - 1);
    edges as f64 / possible as f64
}

/// `|mutual_edges| / |edges|` — matches `networkx.reciprocity(G)` on a directed
/// graph without self-loops (the default interpretation: the fraction of edges
/// whose reverse also exists in the graph).
fn reciprocity(graph: &DiGraph<String, i64>) -> f64 {
    let m = graph.edge_count();
    if m == 0 {
        return 0.0;
    }
    let mut edges: HashSet<(NodeIndex, NodeIndex)> = HashSet::new();
    for e in graph.edge_references() {
        edges.insert((e.source(), e.target()));
    }
    let mutual = edges
        .iter()
        .filter(|(u, v)| edges.contains(&(*v, *u)))
        .count();
    mutual as f64 / m as f64
}

/// Unweighted betweenness centrality via Brandes' algorithm (same output as
/// `networkx.betweenness_centrality(G, normalized=True, weight=None)`).
fn betweenness_centrality(graph: &DiGraph<String, i64>) -> HashMap<NodeIndex, f64> {
    let mut cb: HashMap<NodeIndex, f64> = HashMap::new();
    let nodes: Vec<NodeIndex> = graph.node_indices().collect();
    for &n in &nodes {
        cb.insert(n, 0.0);
    }
    if graph.edge_count() == 0 {
        return cb;
    }
    for &s in &nodes {
        // BFS single-source.
        let mut stack: Vec<NodeIndex> = Vec::new();
        let mut pred: HashMap<NodeIndex, Vec<NodeIndex>> = HashMap::new();
        let mut sigma: HashMap<NodeIndex, f64> = HashMap::new();
        let mut dist: HashMap<NodeIndex, i64> = HashMap::new();
        for &v in &nodes {
            pred.insert(v, Vec::new());
            sigma.insert(v, 0.0);
            dist.insert(v, -1);
        }
        sigma.insert(s, 1.0);
        dist.insert(s, 0);

        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        queue.push_back(s);
        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for w in graph.neighbors_directed(v, Direction::Outgoing) {
                if dist[&w] < 0 {
                    queue.push_back(w);
                    *dist.get_mut(&w).unwrap() = dist[&v] + 1;
                }
                if dist[&w] == dist[&v] + 1 {
                    *sigma.get_mut(&w).unwrap() += sigma[&v];
                    pred.get_mut(&w).unwrap().push(v);
                }
            }
        }

        let mut delta: HashMap<NodeIndex, f64> = HashMap::new();
        for &v in &nodes {
            delta.insert(v, 0.0);
        }
        while let Some(w) = stack.pop() {
            for &v in pred[&w].iter() {
                let coeff = (sigma[&v] / sigma[&w]) * (1.0 + delta[&w]);
                *delta.get_mut(&v).unwrap() += coeff;
            }
            if w != s {
                *cb.get_mut(&w).unwrap() += delta[&w];
            }
        }
    }
    // Normalise: networkx divides directed betweenness by (n-1)(n-2).
    let n = nodes.len() as f64;
    if n > 2.0 {
        let scale = 1.0 / ((n - 1.0) * (n - 2.0));
        for v in cb.values_mut() {
            *v *= scale;
        }
    }
    cb
}

pub fn compute_collaboration_metrics(
    conn: &Connection,
    project_id: i64,
    sprint_id: i64,
) -> rusqlite::Result<()> {
    let rg = build_review_graph(conn, project_id, sprint_id)?;
    let n = rg.nodes.len();
    if n < 2 {
        return Ok(());
    }

    let total_prs: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT pr.id) FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND pr.merged = 1",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let reviewed_prs: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT pr.id) FROM pull_requests pr
             JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
             JOIN tasks t ON t.id = tpr.task_id
             JOIN pr_reviews rv ON rv.pr_id = pr.id
             WHERE t.sprint_id = ? AND t.type != 'USER_STORY' AND pr.merged = 1",
            [sprint_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let review_coverage = if total_prs > 0 {
        reviewed_prs as f64 / total_prs as f64
    } else {
        0.0
    };

    let dens = density(n, rg.graph.edge_count());
    let recip = reciprocity(&rg.graph);
    let betw = betweenness_centrality(&rg.graph);

    let mut centrality_json = serde_json::Map::new();
    let mut has_isolated = false;
    for (id, idx) in &rg.node_idx {
        let in_d = rg
            .graph
            .neighbors_directed(*idx, Direction::Incoming)
            .count() as i64;
        let out_d = rg
            .graph
            .neighbors_directed(*idx, Direction::Outgoing)
            .count() as i64;
        let b = betw.get(idx).copied().unwrap_or(0.0);
        if in_d == 0 && out_d == 0 {
            has_isolated = true;
        }
        centrality_json.insert(
            id.clone(),
            json!({
                "in_degree": in_d,
                "out_degree": out_d,
                "betweenness": round_half_even(b, 4),
            }),
        );
    }

    conn.execute(
        "INSERT OR REPLACE INTO team_sprint_collaboration
         (project_id, sprint_id, network_density, reciprocity,
          centrality_json, has_isolated_member, review_coverage)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            project_id,
            sprint_id,
            dens,
            recip,
            Value::Object(centrality_json).to_string(),
            has_isolated,
            review_coverage,
        ],
    )?;
    let _ = BTreeMap::<String, ()>::new(); // silence unused-import warning when features shift
    Ok(())
}

pub fn compute_all_collaboration(conn: &Connection, sprint_id: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("SELECT DISTINCT project_id FROM sprints WHERE id = ?")?;
    let project_ids: Vec<i64> = stmt
        .query_map([sprint_id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for pid in &project_ids {
        compute_collaboration_metrics(conn, *pid, sprint_id)?;
    }
    info!(count = project_ids.len(), "collaboration metrics computed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_graph(edges: &[(&str, &str)]) -> DiGraph<String, i64> {
        let mut g: DiGraph<String, i64> = DiGraph::new();
        let mut idx: HashMap<String, NodeIndex> = HashMap::new();
        for &(u, v) in edges {
            for name in [u, v] {
                if !idx.contains_key(name) {
                    idx.insert(name.to_string(), g.add_node(name.to_string()));
                }
            }
            g.add_edge(idx[u], idx[v], 1);
        }
        g
    }

    #[test]
    fn density_of_complete_small_graph() {
        // 3 nodes, all 6 directed edges present → density = 1.0
        let g = mk_graph(&[
            ("a", "b"),
            ("b", "a"),
            ("a", "c"),
            ("c", "a"),
            ("b", "c"),
            ("c", "b"),
        ]);
        assert!((density(g.node_count(), g.edge_count()) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn reciprocity_on_mutual_edges() {
        let g = mk_graph(&[("a", "b"), ("b", "a"), ("a", "c")]); // 3 edges, 2 mutual
        let r = reciprocity(&g);
        assert!((r - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn betweenness_path_graph_middle_node() {
        // a → b → c; only `b` lies on the shortest path from a to c.
        let g = mk_graph(&[("a", "b"), ("b", "c")]);
        let b = betweenness_centrality(&g);
        let idx_b = g.node_indices().find(|i| g[*i] == "b").unwrap();
        // Directed normalised betweenness: σ_st(v)/σ_st for (a,c) = 1, divided by (n-1)(n-2)=2.
        let val = b[&idx_b];
        assert!((val - 0.5).abs() < 1e-9, "got {val}");
    }
}
