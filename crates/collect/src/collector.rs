//! Orchestrates data collection from TrackDev and GitHub.
//! Mirrors `src/collect/collector.py`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use chrono::Utc;
use rusqlite::params;
use serde_json::Value;
use tracing::{info, warn};

use sprint_grader_core::attribution::{
    merge_attribution_errors, ATTR_ERR_HTTP_FAILURE, ATTR_ERR_NULL_AUTHOR,
};
use sprint_grader_core::{Config, Database};

use crate::github_client::{ConditionalResult, GitHubClient};
use crate::pm_client::{TrackDevClient, TrackDevError};
use crate::repo_manager::RepoManager;

/// Caller-controlled knobs matching the Python CLI flags.
#[derive(Debug, Clone, Default)]
pub struct CollectOpts {
    /// ISO `YYYY-MM-DD` — the reference date. Sprints with `start_date <= today`
    /// are collected; the one containing today is the current sprint.
    pub today: String,
    pub project_filter: Option<Vec<String>>,
    pub skip_github: bool,
    pub skip_repos: bool,
    pub force_pr_refresh: bool,
    /// Where cloned repos live (typically `data/entregues/`).
    pub repos_dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    #[error("TrackDev error: {0}")]
    TrackDev(#[from] TrackDevError),

    #[error("database error: {0}")]
    Core(#[from] sprint_grader_core::Error),

    #[error("rusqlite error: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Entry point called from the CLI.
pub fn run_collection(
    config: &Config,
    db: &Database,
    opts: &CollectOpts,
) -> Result<(), CollectError> {
    let td = TrackDevClient::new(&config.pm_base_url, &config.trackdev_token)?;

    // Step 1 — course discovery populates projects + students universally.
    let course = collect_course(&td, config, db)?;

    // Step 2 — per-project: resolve sprint list, collect via exports.
    let mut project_ids: Vec<i64> = Vec::new();
    let mut project_repos: Vec<(String, String)> = Vec::new(); // (repo_full_name, project_name)

    let projects = course
        .get("projects")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for project in projects {
        let project_name = project
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if let Some(ref filter) = opts.project_filter {
            if !filter.iter().any(|n| n == &project_name) {
                continue;
            }
        }

        let project_id = match project.get("id").and_then(Value::as_i64) {
            Some(id) => id,
            None => {
                warn!(project_name, "project missing id — skipping");
                continue;
            }
        };

        info!(project_name, project_id, "Processing project");

        let sprint_ids = resolve_sprint_ids_for_project(&td, db, project_id, &opts.today)?;
        if sprint_ids.is_empty() {
            warn!(
                project_name,
                today = %opts.today,
                "No sprints with start_date <= today — skipping project"
            );
            continue;
        }

        collect_project_via_exports(&td, db, project_id, &sprint_ids)?;
        project_ids.push(project_id);

        // Stash repo list for optional cloning step below — union across all
        // collected sprints for this project.
        if !opts.skip_repos {
            for repo_fullname in collect_repo_names_for_project(db, project_id)? {
                project_repos.push((repo_fullname, project_name.clone()));
            }
        }
    }

    info!(
        tasks = db.count_table("tasks")?,
        pull_requests = db.count_table("pull_requests")?,
        "TrackDev collection done"
    );

    // Step 3 — GitHub details (body, commits, reviews). One pass per project
    // across the union of PRs — Layer-1 watermark + Layer-2 ETag skips keep
    // this cheap even with multiple sprints of history.
    if !opts.skip_github {
        match GitHubClient::new(&config.github_token) {
            Ok(gh) => {
                for project_id in &project_ids {
                    collect_github_details_for_project(
                        &gh,
                        db,
                        *project_id,
                        opts.force_pr_refresh,
                    )?;
                }
            }
            Err(e) => warn!(error = %e, "Cannot collect GitHub data"),
        }
    } else {
        info!("Skipping GitHub collection (--skip-github)");
    }

    // Step 4 — PR author attribution. Scoped to the projects we just
    // collected, so `--projects pds26-3c` doesn't re-resolve every PR in
    // the whole DB.
    let pid_filter: Option<&[i64]> = if opts.project_filter.is_some() {
        Some(project_ids.as_slice())
    } else {
        None
    };
    resolve_pr_authors(db, pid_filter)?;

    // Step 5 — optional: clone/update repos.
    if !opts.skip_repos {
        if let Some(repos_dir) = opts.repos_dir.clone() {
            // Deduplicate (repo, project) pairs — each team usually has an
            // Android + Spring repo and the pair list can contain dupes from
            // multiple PRs in the same repo.
            let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
            let mut unique: Vec<(String, String)> = Vec::new();
            for (repo, proj) in project_repos {
                if seen.insert((repo.clone(), proj.clone())) {
                    unique.push((repo, proj));
                }
            }
            if !unique.is_empty() {
                let manager = RepoManager::new(repos_dir, config.github_token.clone());
                manager.clone_all(&unique, 4);
            }
        } else {
            info!("No repos_dir configured — skipping clone step");
        }
    }

    Ok(())
}

/// Fetch all of `project_id`'s sprints from TrackDev, upsert them into the DB,
/// then return the ids of those whose `start_date <= today` ordered ASC.
/// The sprint containing today (if any) is the last element.
fn resolve_sprint_ids_for_project(
    client: &TrackDevClient,
    db: &Database,
    project_id: i64,
    today: &str,
) -> Result<Vec<i64>, CollectError> {
    let sprints = client.get_project_sprints(project_id)?;

    for s in &sprints {
        let sid = s.get("id").and_then(Value::as_i64).unwrap_or(0);
        let name = s
            .get("value")
            .or_else(|| s.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let start = s.get("startDate").and_then(Value::as_str).unwrap_or("");
        let end = s.get("endDate").and_then(Value::as_str).unwrap_or("");
        db.upsert_sprint(sid, project_id, name, start, end)?;
    }

    Ok(db.sprint_ids_up_to_current(project_id, today)?)
}

fn collect_course(
    client: &TrackDevClient,
    config: &Config,
    db: &Database,
) -> Result<Value, CollectError> {
    info!(course_id = config.course_id, "Fetching course details");
    let course = client.get_course_details(config.course_id)?;

    if let Some(projects) = course.get("projects").and_then(Value::as_array) {
        for project in projects {
            let id = project.get("id").and_then(Value::as_i64).unwrap_or(0);
            let slug = project
                .get("slug")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = project
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            db.upsert_project(id, slug, name)?;

            if let Some(members) = project.get("members").and_then(Value::as_array) {
                for member in members {
                    upsert_student_from_value(db, member, Some(id))?;
                }
            }
        }
    }

    // Also insert students from the top-level students list (may include
    // students not yet assigned to a project). team_project_id = NULL so
    // COALESCE in upsert_student preserves any existing team assignment.
    if let Some(students) = course.get("students").and_then(Value::as_array) {
        for student in students {
            upsert_student_from_value(db, student, None)?;
        }
    }

    info!(
        projects = db.count_table("projects")?,
        students = db.count_table("students")?,
        "Collected course metadata"
    );

    Ok(course)
}

fn upsert_student_from_value(
    db: &Database,
    member: &Value,
    team_project_id: Option<i64>,
) -> Result<(), CollectError> {
    let id = match member.get("id").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return Ok(()),
    };
    let username = member
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let full_name = member
        .get("fullName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let email = member
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let gh_login = member
        .get("githubInfo")
        .and_then(|g| g.get("login"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if gh_login.is_none() {
        warn!(
            username = %username,
            full_name = %full_name,
            "Student has no GitHub login",
        );
    }
    db.upsert_student(
        &id,
        &username,
        gh_login.as_deref(),
        &full_name,
        email.as_deref(),
        team_project_id,
    )?;
    Ok(())
}

/// Collect project data via the 3 TrackDev export endpoints, upserting
/// per-sprint rows for every sprint id in `sprint_ids`. These exports are
/// project-wide; the three HTTP calls are made once and the payload is
/// filtered in-memory per sprint.
///   - `GET /projects/{id}/export/team`
///   - `GET /projects/{id}/export/tasks`
///   - `GET /projects/{id}/export/pull-requests`
fn collect_project_via_exports(
    client: &TrackDevClient,
    db: &Database,
    project_id: i64,
    sprint_ids: &[i64],
) -> Result<(), CollectError> {
    if sprint_ids.is_empty() {
        return Ok(());
    }
    let sprint_set: HashSet<i64> = sprint_ids.iter().copied().collect();

    // (1) Team export — upsert students with project association.
    let team = client.get_project_export_team(project_id)?;
    if let Some(members) = team.get("members").and_then(Value::as_array) {
        for member in members {
            if let Some(user) = member.get("user") {
                upsert_student_from_value(db, user, Some(project_id))?;
            }
        }
    }

    // (2) Tasks export — iterate ALL project tasks, assign each task to the
    // sprint_id from `sprint_ids` its `activeSprints` intersects. TrackDev
    // keeps one active sprint per task at a time; matching by set membership
    // tolerates tasks that live on multiple historical sprints.
    let tasks_export = client.get_project_export_tasks(project_id)?;
    let mut task_ids_by_sprint: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut links_by_sprint: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
    let mut pr_author_ids: HashMap<String, String> = HashMap::new();

    if let Some(entries) = tasks_export.get("tasks").and_then(Value::as_array) {
        for entry in entries {
            let task = match entry.get("task") {
                Some(t) => t,
                None => continue,
            };
            let matched_sprint = task
                .get("activeSprints")
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter()
                        .filter_map(|s| s.get("id").and_then(Value::as_i64))
                        .find(|sid| sprint_set.contains(sid))
                });
            let sprint_id = match matched_sprint {
                Some(sid) => sid,
                None => continue,
            };

            let task_id = task.get("id").and_then(Value::as_i64).unwrap_or(0);
            task_ids_by_sprint
                .entry(sprint_id)
                .or_default()
                .push(task_id);
            let assignee_id = task
                .get("assignee")
                .and_then(|a| a.get("id"))
                .and_then(Value::as_str);
            db.upsert_task(
                task_id,
                task.get("taskKey").and_then(Value::as_str).unwrap_or(""),
                task.get("name").and_then(Value::as_str).unwrap_or(""),
                task.get("type").and_then(Value::as_str).unwrap_or(""),
                task.get("status").and_then(Value::as_str).unwrap_or(""),
                task.get("estimationPoints").and_then(Value::as_i64),
                assignee_id,
                sprint_id,
                task.get("parentTaskId").and_then(Value::as_i64),
            )?;

            if let Some(prs) = task.get("pullRequests").and_then(Value::as_array) {
                for pr in prs {
                    let pr_id = pr.get("id").and_then(Value::as_str).unwrap_or("");
                    if pr_id.is_empty() {
                        continue;
                    }
                    upsert_pr_from_export(db, pr)?;
                    if let Some(aid) = pr
                        .get("author")
                        .and_then(|a| a.get("id"))
                        .and_then(Value::as_str)
                    {
                        pr_author_ids.insert(pr_id.to_string(), aid.to_string());
                    }
                    links_by_sprint
                        .entry(sprint_id)
                        .or_default()
                        .push((task_id, pr_id.to_string()));
                }
            }
        }
    }

    // (3) Pull-requests export — authoritative author attribution from
    // TrackDev. Enriches any PR we already upserted (and any missed ones).
    let prs_export = client.get_project_export_pull_requests(project_id)?;
    if let Some(entries) = prs_export.get("pullRequests").and_then(Value::as_array) {
        for entry in entries {
            let pr = match entry.get("pullRequest") {
                Some(p) => p,
                None => continue,
            };
            let pr_id = pr.get("id").and_then(Value::as_str).unwrap_or("");
            if pr_id.is_empty() {
                continue;
            }
            upsert_pr_from_export(db, pr)?;
            if let Some(aid) = pr
                .get("author")
                .and_then(|a| a.get("id"))
                .and_then(Value::as_str)
            {
                pr_author_ids.insert(pr_id.to_string(), aid.to_string());
            }
        }
    }

    // Persist TrackDev-side author_id as a pre-seed; resolve_pr_authors()
    // will still run afterwards to cover PRs TrackDev couldn't attribute.
    for (pr_id, author_id) in &pr_author_ids {
        db.conn.execute(
            "UPDATE pull_requests SET author_id = COALESCE(author_id, ?) WHERE id = ?",
            params![author_id, pr_id],
        )?;
    }

    // Reconcile per sprint so every sprint in the list ends up consistent,
    // even ones that had zero tasks in this run.
    for sid in sprint_ids {
        let links = links_by_sprint.remove(sid).unwrap_or_default();
        let task_ids = task_ids_by_sprint.remove(sid).unwrap_or_default();
        db.replace_task_pr_links_for_sprint(*sid, &links)?;
        db.remove_missing_tasks_for_sprint(*sid, &task_ids)?;
    }

    Ok(())
}

/// Upsert a PR from an export payload (`PullRequestDTO`). Leaves GitHub-only
/// fields (body, merged_at, github_author_*, merged_by_*) NULL — they are
/// filled later by `collect_github_details_for_project`.
fn upsert_pr_from_export(db: &Database, pr: &Value) -> Result<(), CollectError> {
    let pr_id = pr.get("id").and_then(Value::as_str).unwrap_or("");
    let author_id = pr
        .get("author")
        .and_then(|a| a.get("id"))
        .and_then(Value::as_str);
    db.upsert_pull_request(
        pr_id,
        pr.get("prNumber").and_then(Value::as_i64).unwrap_or(0),
        pr.get("repoFullName").and_then(Value::as_str).unwrap_or(""),
        pr.get("url").and_then(Value::as_str).unwrap_or(""),
        pr.get("title").and_then(Value::as_str).unwrap_or(""),
        None,
        pr.get("state").and_then(Value::as_str).unwrap_or(""),
        pr.get("merged").and_then(Value::as_bool).unwrap_or(false),
        author_id,
        pr.get("additions").and_then(Value::as_i64),
        pr.get("deletions").and_then(Value::as_i64),
        pr.get("changedFiles").and_then(Value::as_i64),
        pr.get("createdAt").and_then(Value::as_str),
        pr.get("updatedAt").and_then(Value::as_str),
        None,
        None,
        None,
        None,
        None,
        None,
    )?;
    Ok(())
}

/// Collect unique `repo_full_name` values for every PR linked to any
/// non-USER_STORY task in the given project (across all its sprints).
fn collect_repo_names_for_project(
    db: &Database,
    project_id: i64,
) -> Result<Vec<String>, CollectError> {
    let mut stmt = db.conn.prepare(
        "SELECT DISTINCT pr.repo_full_name
         FROM pull_requests pr
         JOIN task_pull_requests tpr ON tpr.pr_id = pr.id
         JOIN tasks t ON t.id = tpr.task_id
         JOIN sprints s ON s.id = t.sprint_id
         WHERE s.project_id = ? AND t.type != 'USER_STORY'
           AND pr.repo_full_name IS NOT NULL AND pr.repo_full_name != ''",
    )?;
    let rows = stmt.query_map([project_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// True iff a PR is in a terminal state AND we already have commits for it.
/// Append a single `{kind, detail, observed_at}` entry to
/// `pull_requests.attribution_errors` for `pr_id` (T-P1.5). The column
/// existed in the schema but was barely used; this helper centralises
/// merging so the array grows monotonically rather than being overwritten.
fn record_attribution_error(
    db: &Database,
    pr_id: &str,
    kind: &str,
    detail: &str,
) -> Result<(), CollectError> {
    let existing: Option<String> = db
        .conn
        .query_row(
            "SELECT attribution_errors FROM pull_requests WHERE id = ?",
            [pr_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let merged = merge_attribution_errors(existing.as_deref(), kind, detail);
    db.conn.execute(
        "UPDATE pull_requests SET attribution_errors = ? WHERE id = ?",
        params![merged, pr_id],
    )?;
    Ok(())
}

/// Open PRs always re-fetch; merged PRs also require `merged_at` to be set
/// (sanity check that the GitHub-side UPDATE actually ran before).
fn pr_fully_collected(
    db: &Database,
    pr: &sprint_grader_core::db::PullRequestRow,
) -> Result<bool, CollectError> {
    let state = pr.state.as_deref().unwrap_or("").to_lowercase();
    let is_terminal = pr.merged || state == "closed" || state == "merged";
    if !is_terminal {
        return Ok(false);
    }
    if pr.merged && pr.merged_at.as_deref().unwrap_or("").is_empty() {
        return Ok(false);
    }
    Ok(db.count_pr_commits(&pr.id)? > 0)
}

/// True iff TrackDev's `updated_at` matches the timestamp we recorded the
/// last time GitHub details were successfully fetched for this PR. When it
/// matches, nothing on the PR can have changed since last run, so the 3
/// GitHub calls (PR, commits, reviews) can be skipped entirely.
fn pr_unchanged_since_last_fetch(pr: &sprint_grader_core::db::PullRequestRow) -> bool {
    match (
        pr.updated_at.as_deref(),
        pr.last_github_fetch_updated_at.as_deref(),
    ) {
        (Some(now), Some(last)) if !now.is_empty() && !last.is_empty() => now == last,
        _ => false,
    }
}

fn collect_github_details_for_project(
    gh: &GitHubClient,
    db: &Database,
    project_id: i64,
    force_refresh: bool,
) -> Result<(), CollectError> {
    let prs = db.get_pull_requests_for_project(project_id)?;
    let total = prs.len();
    let mut fetched = 0usize;
    let mut skipped = 0usize;
    info!(total, force_refresh, "Fetching GitHub details for PRs");

    for (i, pr) in prs.iter().enumerate() {
        let repo = pr.repo_full_name.as_deref().unwrap_or("");
        let number = pr.pr_number;
        if repo.is_empty() || number == 0 {
            warn!(pr_id = %pr.id, "Skipping PR — missing repo or number");
            continue;
        }

        // Skip only when *both* conditions hold: TrackDev says nothing
        // changed since our last fetch AND we already have the GitHub
        // data we expect for this PR's state. The watermark alone is
        // unsafe because `last_github_fetch_updated_at` is recorded
        // unconditionally at the end of each iteration — including
        // iterations where the PR fetch errored or returned a partial
        // payload — which used to permanently lock those rows out of
        // re-fetch despite obvious incompleteness (e.g. merged=1 with
        // merged_at empty).
        if !force_refresh && pr_unchanged_since_last_fetch(pr) && pr_fully_collected(db, pr)? {
            skipped += 1;
            continue;
        }

        fetched += 1;
        info!(idx = i + 1, total, repo, number, "  fetching");

        let now_iso = Utc::now().to_rfc3339();
        let pr_etag = if force_refresh {
            None
        } else {
            db.get_pr_github_etag(&pr.id, "pr")?
        };
        let commits_etag = if force_refresh {
            None
        } else {
            db.get_pr_github_etag(&pr.id, "commits")?
        };
        let reviews_etag = if force_refresh {
            None
        } else {
            db.get_pr_github_etag(&pr.id, "reviews")?
        };

        // PR body + merged_at + GitHub author/merger identity.
        match gh.get_pr_conditional(repo, number, pr_etag.as_deref()) {
            Ok(ConditionalResult::NotModified) => {}
            Ok(ConditionalResult::Fresh {
                value: gh_pr,
                etag: new_etag,
            }) => {
                let user = gh_pr.get("user");
                let merged_by = gh_pr.get("merged_by");
                db.conn.execute(
                    "UPDATE pull_requests
                     SET body = ?, merged_at = ?,
                         github_author_login = ?, github_author_email = ?,
                         merged_by_login = ?, merged_by_email = ?,
                         additions = COALESCE(?, additions),
                         deletions = COALESCE(?, deletions),
                         changed_files = COALESCE(?, changed_files)
                     WHERE id = ?",
                    params![
                        gh_pr.get("body").and_then(Value::as_str),
                        gh_pr.get("merged_at").and_then(Value::as_str),
                        user.and_then(|u| u.get("login")).and_then(Value::as_str),
                        user.and_then(|u| u.get("email")).and_then(Value::as_str),
                        merged_by
                            .and_then(|m| m.get("login"))
                            .and_then(Value::as_str),
                        merged_by
                            .and_then(|m| m.get("email"))
                            .and_then(Value::as_str),
                        gh_pr.get("additions").and_then(Value::as_i64),
                        gh_pr.get("deletions").and_then(Value::as_i64),
                        gh_pr.get("changed_files").and_then(Value::as_i64),
                        pr.id,
                    ],
                )?;
                if let Some(tag) = new_etag.as_deref() {
                    if !tag.is_empty() {
                        db.upsert_pr_github_etag(&pr.id, "pr", tag, Some(&now_iso))?;
                    }
                }
            }
            Err(e) => {
                warn!(repo, number, error = %e, "Failed to fetch PR");
                let _ = record_attribution_error(
                    db,
                    &pr.id,
                    ATTR_ERR_HTTP_FAILURE,
                    &format!("GET /pulls/{number}: {e}"),
                );
            }
        }

        // Commits.
        let mut null_author_count: i64 = 0;
        match gh.get_pr_commits_conditional(repo, number, commits_etag.as_deref()) {
            Ok(ConditionalResult::NotModified) => {}
            Ok(ConditionalResult::Fresh {
                value: commits,
                etag: new_etag,
            }) => {
                for c in commits {
                    let author_login = c
                        .get("author")
                        .and_then(|a| a.get("login"))
                        .and_then(Value::as_str);
                    let commit = c.get("commit");
                    let commit_author = commit.and_then(|cm| cm.get("author"));
                    let author_email = commit_author
                        .and_then(|a| a.get("email"))
                        .and_then(Value::as_str);
                    let message = commit
                        .and_then(|cm| cm.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let timestamp = commit_author
                        .and_then(|a| a.get("date"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if author_login.is_none() {
                        null_author_count += 1;
                    }
                    let sha = c.get("sha").and_then(Value::as_str).unwrap_or("");
                    db.upsert_pr_commit(
                        &pr.id,
                        sha,
                        author_login,
                        author_email,
                        message,
                        timestamp,
                        None,
                        None,
                    )?;
                    // T-P1.4: shadow per-commit author into pr_pre_squash_authors
                    // for merged PRs so AUTHOR_MISMATCH survives a later
                    // force-push that would otherwise erase the original
                    // history. Detection of "is this PR squash-merged" via
                    // local-clone reachability is an optimisation deferred to
                    // a follow-up; the always-shadow strategy is purely
                    // additive (PK on (pr_id, sha) keeps writes idempotent).
                    if pr.merged && !sha.is_empty() {
                        db.upsert_pr_pre_squash_author(
                            &pr.id,
                            sha,
                            author_login,
                            author_email,
                            &now_iso,
                        )?;
                    }
                }
                if let Some(tag) = new_etag.as_deref() {
                    if !tag.is_empty() {
                        db.upsert_pr_github_etag(&pr.id, "commits", tag, Some(&now_iso))?;
                    }
                }
            }
            Err(e) => {
                warn!(repo, number, error = %e, "Failed to fetch commits");
                let _ = record_attribution_error(
                    db,
                    &pr.id,
                    ATTR_ERR_HTTP_FAILURE,
                    &format!("GET /pulls/{number}/commits: {e}"),
                );
            }
        }
        if null_author_count > 0 {
            let _ = record_attribution_error(
                db,
                &pr.id,
                ATTR_ERR_NULL_AUTHOR,
                &format!("{null_author_count} commit(s) returned with null author.login"),
            );
        }

        // Reviews.
        match gh.get_pr_reviews_conditional(repo, number, reviews_etag.as_deref()) {
            Ok(ConditionalResult::NotModified) => {}
            Ok(ConditionalResult::Fresh {
                value: reviews,
                etag: new_etag,
            }) => {
                for r in reviews {
                    let reviewer = r
                        .get("user")
                        .and_then(|u| u.get("login"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let state = r.get("state").and_then(Value::as_str).unwrap_or("");
                    let submitted = r.get("submitted_at").and_then(Value::as_str).unwrap_or("");
                    db.upsert_pr_review(&pr.id, reviewer, state, submitted)?;
                }
                if let Some(tag) = new_etag.as_deref() {
                    if !tag.is_empty() {
                        db.upsert_pr_github_etag(&pr.id, "reviews", tag, Some(&now_iso))?;
                    }
                }
            }
            Err(e) => warn!(repo, number, error = %e, "Failed to fetch reviews"),
        }

        // Record watermark — subsequent runs will skip this PR until TrackDev
        // reports a newer `updated_at`.
        if let Some(ref u) = pr.updated_at {
            db.conn.execute(
                "UPDATE pull_requests SET last_github_fetch_updated_at = ? WHERE id = ?",
                params![u, pr.id],
            )?;
        }
    }

    collect_github_users(gh, db)?;

    info!(
        fetched,
        skipped_cached = skipped,
        api_calls = gh.call_count(),
        "GitHub collection done"
    );
    Ok(())
}

fn collect_github_users(gh: &GitHubClient, db: &Database) -> Result<(), CollectError> {
    let mut logins: BTreeSet<String> = BTreeSet::new();

    let columns: &[&str] = &[
        "SELECT DISTINCT github_author_login FROM pull_requests WHERE github_author_login IS NOT NULL",
        "SELECT DISTINCT merged_by_login FROM pull_requests WHERE merged_by_login IS NOT NULL",
        "SELECT DISTINCT author_login FROM pr_commits WHERE author_login IS NOT NULL",
        "SELECT DISTINCT reviewer_login FROM pr_reviews WHERE reviewer_login IS NOT NULL",
    ];
    for sql in columns {
        let mut stmt = db.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            logins.insert(row?);
        }
    }

    let mut already: BTreeSet<String> = BTreeSet::new();
    let mut stmt = db
        .conn
        .prepare("SELECT login FROM github_users WHERE fetched_at IS NOT NULL")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for r in rows {
        already.insert(r?);
    }

    let to_fetch: Vec<String> = logins.difference(&already).cloned().collect();
    if to_fetch.is_empty() {
        info!(
            cached = logins.len(),
            "All GitHub user profiles already fetched"
        );
        return Ok(());
    }

    info!(
        to_fetch = to_fetch.len(),
        already = already.len(),
        "Fetching GitHub user profiles"
    );
    let now = Utc::now().to_rfc3339();

    for (i, login) in to_fetch.iter().enumerate() {
        match gh.get_user(login) {
            Ok(profile) => {
                let name = profile.get("name").and_then(Value::as_str);
                let email = profile.get("email").and_then(Value::as_str);
                db.upsert_github_user(login, name, email, None, Some(&now))?;
            }
            Err(e) => {
                warn!(login, error = %e, "Failed to fetch profile");
                // Still record the login so we don't retry endlessly.
                db.upsert_github_user(login, None, None, None, Some(&now))?;
            }
        }
        if (i + 1) % 20 == 0 {
            info!(
                done = i + 1,
                total = to_fetch.len(),
                "  user profiles fetched"
            );
        }
    }

    // Auto-link github_users → students where logins match (case-insensitive).
    db.conn.execute(
        "UPDATE github_users
         SET student_id = (
             SELECT s.id FROM students s
             WHERE LOWER(s.github_login) = LOWER(github_users.login)
         )
         WHERE student_id IS NULL",
        [],
    )?;

    info!("GitHub user profile collection done");
    Ok(())
}

/// Resolve PR author → TrackDev student. Priority order matches the
/// workspace convention (task-assignee is the canonical source for
/// TrackDev-scoped attribution; github identity is a git-side fallback):
///
/// 1. **Task assignee** (`task_pull_requests → tasks.assignee_id`).
///    - Single distinct assignee → resolved.
///    - Multiple distinct assignees → pick the most-points assignee
///      (ties → most tasks → smallest student_id), record an
///      attribution_errors entry so professors can spot the ambiguity.
///      Mirrors the `pr_authors` view ordering used in the report layer.
/// 2. **GitHub identity** (consulted only if no task assignees exist —
///    typically orphan PRs). Look up:
///      - `pr.github_author_login` against `student_github_identity`
///        (login kind), then `github_users.student_id`, then
///        `students.github_login`.
///      - Each row in `pr_commits` for this PR: `author_login` against
///        the same login chain, `author_email` against
///        `student_github_identity` (email kind).
///
///    If all candidate students agree, attribute to that student.
///    If they disagree, leave NULL with an attribution_errors entry.
/// 3. **Last-known author_id** is preserved if all of the above failed.
///
/// `--projects` scoping: the work-set covers PRs linked to ANY task whose
/// assignee belongs to the targeted projects, plus PRs whose currently
/// recorded `author_id` is in those projects. The previous implementation
/// joined on `pr.author_id`, which silently skipped every PR with a NULL
/// author — exactly the rows that needed re-resolution most.
fn resolve_pr_authors(db: &Database, project_ids: Option<&[i64]>) -> Result<(), CollectError> {
    let rows: Vec<(String, Option<String>, Option<String>)> = if let Some(ids) = project_ids {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        // PRs are in scope when ANY linked task's assignee, or the PR's own
        // author_id (carry-over for re-resolution), belongs to a target
        // project. The pr_authors view already filters out USER_STORY parents.
        let sql = format!(
            "SELECT DISTINCT pr.id, pr.author_id, pr.github_author_login
             FROM pull_requests pr
             WHERE pr.id IN (
                 SELECT pa.pr_id FROM pr_authors pa
                 JOIN students s ON s.id = pa.student_id
                 WHERE s.team_project_id IN ({placeholders})
             )
             OR pr.author_id IN (
                 SELECT id FROM students WHERE team_project_id IN ({placeholders})
             )"
        );
        let mut stmt = db.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = ids
            .iter()
            .chain(ids.iter())
            .map(|i| i as &dyn rusqlite::ToSql)
            .collect();
        let collected = stmt
            .query_map(params.as_slice(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        collected
    } else {
        let mut stmt = db
            .conn
            .prepare("SELECT id, author_id, github_author_login FROM pull_requests")?;
        let collected = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        collected
    };

    let mut resolved_count = 0usize;
    let mut error_count = 0usize;
    let total = rows.len();

    for (pr_id, prior_author_id, gh_login) in rows {
        let mut errors: Vec<String> = Vec::new();
        let mut resolved_id: Option<String> = None;

        // Strategy 1 — task assignee (canonical TrackDev source). Reads
        // pr_authors so the points-aware ordering is identical to the
        // primary-author rule used everywhere in the report layer.
        let mut stmt1 = db.conn.prepare(
            "SELECT student_id, author_points, author_task_count
             FROM pr_authors
             WHERE pr_id = ?
             ORDER BY author_points DESC, author_task_count DESC, student_id",
        )?;
        let assignees: Vec<(String, i64, i64)> = stmt1
            .query_map([&pr_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt1);

        match assignees.as_slice() {
            [only] => resolved_id = Some(only.0.clone()),
            [primary, rest @ ..] if !rest.is_empty() => {
                // Multi-assignee — pick most-points (already at index 0
                // thanks to ORDER BY) and record the ambiguity for review.
                resolved_id = Some(primary.0.clone());
                let mut names: Vec<String> = Vec::with_capacity(assignees.len());
                for (aid, points, _) in &assignees {
                    let full_name: Option<String> = db
                        .conn
                        .query_row("SELECT full_name FROM students WHERE id = ?", [aid], |r| {
                            r.get(0)
                        })
                        .ok()
                        .flatten();
                    let label = full_name.unwrap_or_else(|| aid.clone());
                    names.push(format!("{label} ({points}pts)"));
                }
                errors.push(format!(
                    "PR linked to tasks with multiple distinct assignees, picked the most-points one: {names:?}"
                ));
            }
            _ => {
                // Strategy 2 — github identity. Only reachable for orphan
                // PRs (no linked task assignees). Single source of truth:
                // student_github_identity. github_users and
                // students.github_login are kept as cold-start backstops
                // until the resolver has run.
                let mut candidates: HashSet<String> = HashSet::new();

                if let Some(login) = gh_login.as_deref() {
                    if let Some(sid) = lookup_student_by_login(db, login)? {
                        candidates.insert(sid);
                    }
                }

                let mut commit_stmt = db
                    .conn
                    .prepare("SELECT author_login, author_email FROM pr_commits WHERE pr_id = ?")?;
                let commit_rows: Vec<(Option<String>, Option<String>)> = commit_stmt
                    .query_map([&pr_id], |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                drop(commit_stmt);

                for (login, email) in &commit_rows {
                    if let Some(login) = login.as_deref().filter(|s| !s.is_empty()) {
                        if let Some(sid) = lookup_student_by_login(db, login)? {
                            candidates.insert(sid);
                        }
                    }
                    if let Some(email) = email.as_deref().filter(|s| !s.is_empty()) {
                        if let Some(sid) = lookup_student_by_email(db, email)? {
                            candidates.insert(sid);
                        }
                    }
                }

                match candidates.len() {
                    1 => resolved_id = candidates.into_iter().next(),
                    0 => {
                        errors.push(if gh_login.is_some() || !commit_rows.is_empty() {
                            "GitHub identities on this PR did not match any student".to_string()
                        } else {
                            "No GitHub author login, no task assignee, no commit identities"
                                .to_string()
                        });
                    }
                    _ => {
                        errors.push(format!(
                            "PR has conflicting GitHub identities resolving to {} different students; left unresolved",
                            candidates.len()
                        ));
                    }
                }
            }
        }

        let final_id = resolved_id.clone().or(prior_author_id);

        db.conn.execute(
            "UPDATE pull_requests SET author_id = ? WHERE id = ?",
            params![final_id, pr_id],
        )?;
        for err_detail in &errors {
            let _ = record_attribution_error(db, &pr_id, ATTR_ERR_NULL_AUTHOR, err_detail);
        }

        if resolved_id.is_some() {
            resolved_count += 1;
        }
        if !errors.is_empty() {
            error_count += 1;
        }
    }

    info!(
        resolved = resolved_count,
        total,
        errors = error_count,
        "PR author resolution"
    );
    Ok(())
}

/// Resolve a github login to a TrackDev `student_id`, consulting (in
/// order) `student_github_identity` (the resolver's accumulated evidence),
/// `github_users.student_id` (cold-start mapping populated when fetching
/// profiles), and `students.github_login` (manual seed).
///
/// The identity table stores values lowercased; the helper lowercases the
/// input in the same way to keep matches consistent.
fn lookup_student_by_login(db: &Database, login: &str) -> rusqlite::Result<Option<String>> {
    let needle = login.to_lowercase();
    if let Ok(sid) = db.conn.query_row(
        "SELECT student_id FROM student_github_identity
         WHERE identity_kind = 'login' AND identity_value = ?
         ORDER BY weight DESC, confidence DESC, student_id
         LIMIT 1",
        [&needle],
        |r| r.get::<_, String>(0),
    ) {
        return Ok(Some(sid));
    }
    if let Ok(sid) = db.conn.query_row(
        "SELECT student_id FROM github_users
         WHERE LOWER(login) = ? AND student_id IS NOT NULL
         LIMIT 1",
        [&needle],
        |r| r.get::<_, String>(0),
    ) {
        return Ok(Some(sid));
    }
    Ok(db
        .conn
        .query_row(
            "SELECT id FROM students WHERE LOWER(github_login) = ? LIMIT 1",
            [&needle],
            |r| r.get::<_, String>(0),
        )
        .ok())
}

/// Resolve a commit `author_email` to a TrackDev `student_id`. Only the
/// identity table is consulted — `students.email` is TrackDev's email,
/// which only matches a github commit email by accident.
fn lookup_student_by_email(db: &Database, email: &str) -> rusqlite::Result<Option<String>> {
    let needle = email.to_lowercase();
    Ok(db
        .conn
        .query_row(
            "SELECT student_id FROM student_github_identity
             WHERE identity_kind = 'email' AND identity_value = ?
             ORDER BY weight DESC, confidence DESC, student_id
             LIMIT 1",
            [&needle],
            |r| r.get::<_, String>(0),
        )
        .ok())
}

#[cfg(test)]
mod resolve_pr_authors_tests {
    //! End-to-end tests for resolve_pr_authors. They cover the four bugs
    //! the rewrite addresses:
    //!   - NULL pr.author_id with a single task assignee resolves via Strategy 1
    //!     (the headline bug; ~25% of PRs in production were stuck NULL).
    //!   - Multi-assignee PR picks the most-points assignee deterministically.
    //!   - Orphan PR resolves through the github lookup chain
    //!     (student_github_identity → github_users → students.github_login)
    //!     and via pr_commits when github_author_login is NULL.
    //!   - --projects scoping no longer drops NULL-author PRs from the work-set.

    use super::*;
    use rusqlite::Connection;
    use sprint_grader_core::db::apply_schema;
    use std::path::PathBuf;

    fn mk_db() -> Database {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        Database {
            db_path: PathBuf::new(),
            conn,
        }
    }

    fn seed_team(db: &Database) {
        db.conn
            .execute_batch(
                "INSERT INTO projects (id, slug, name) VALUES (1, 'team-alpha', 'Team Alpha');
                 INSERT INTO projects (id, slug, name) VALUES (2, 'team-beta',  'Team Beta');
                 INSERT INTO students (id, full_name, github_login, team_project_id) VALUES
                    ('alice', 'Alice Adams',  'alice-gh', 1),
                    ('bob',   'Bob Brown',    NULL,       1),
                    ('carol', 'Carol Carter', NULL,       1),
                    ('dave',  'Dave Davis',   NULL,       2);",
            )
            .unwrap();
    }

    fn seed_task(db: &Database, id: i64, assignee: &str, points: i64) {
        db.conn
            .execute(
                "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                                    assignee_id, sprint_id)
                 VALUES (?, ?, ?, 'TASK', 'DONE', ?, ?, 10)",
                params![
                    id,
                    format!("T-{}", id),
                    format!("Task {}", id),
                    points,
                    assignee
                ],
            )
            .unwrap();
    }

    fn seed_pr_with_null_author(db: &Database, pr_id: &str, github_author_login: Option<&str>) {
        db.conn
            .execute(
                "INSERT INTO pull_requests (id, pr_number, repo_full_name, title, url,
                                            author_id, github_author_login, merged)
                 VALUES (?, 1, 'org/repo', 't', NULL, NULL, ?, 1)",
                params![pr_id, github_author_login],
            )
            .unwrap();
    }

    fn link(db: &Database, task_id: i64, pr_id: &str) {
        db.conn
            .execute(
                "INSERT INTO task_pull_requests (task_id, pr_id) VALUES (?, ?)",
                params![task_id, pr_id],
            )
            .unwrap();
    }

    fn author_of(db: &Database, pr_id: &str) -> Option<String> {
        db.conn
            .query_row(
                "SELECT author_id FROM pull_requests WHERE id = ?",
                [pr_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
    }

    #[test]
    fn null_author_pr_with_single_assignee_resolves_via_task_assignment() {
        let db = mk_db();
        seed_team(&db);
        seed_task(&db, 1, "alice", 5);
        seed_pr_with_null_author(&db, "pr-1", None);
        link(&db, 1, "pr-1");

        resolve_pr_authors(&db, None).unwrap();

        assert_eq!(
            author_of(&db, "pr-1"),
            Some("alice".to_string()),
            "NULL author_id with single linked-task assignee must resolve to the assignee"
        );
    }

    #[test]
    fn multi_assignee_pr_picks_the_most_points_assignee() {
        let db = mk_db();
        seed_team(&db);
        // Bob 8 points, Alice 3 points → Bob wins on points alone.
        seed_task(&db, 1, "alice", 3);
        seed_task(&db, 2, "bob", 8);
        seed_pr_with_null_author(&db, "pr-multi", None);
        link(&db, 1, "pr-multi");
        link(&db, 2, "pr-multi");

        resolve_pr_authors(&db, None).unwrap();

        assert_eq!(
            author_of(&db, "pr-multi"),
            Some("bob".to_string()),
            "primary author = max-points assignee"
        );

        // Ambiguity must be recorded so professors can audit.
        let attrib_errors: Option<String> = db
            .conn
            .query_row(
                "SELECT attribution_errors FROM pull_requests WHERE id = 'pr-multi'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let detail = attrib_errors.unwrap_or_default();
        assert!(
            detail.contains("multiple distinct assignees"),
            "ambiguity must be reported in attribution_errors, got: {detail}"
        );
    }

    #[test]
    fn multi_assignee_tie_break_falls_through_to_task_count_then_id() {
        let db = mk_db();
        seed_team(&db);
        // Bob & Carol each at 5 points; Bob has two tasks, Carol one.
        seed_task(&db, 1, "bob", 3);
        seed_task(&db, 2, "bob", 2);
        seed_task(&db, 3, "carol", 5);
        seed_pr_with_null_author(&db, "pr-tie", None);
        link(&db, 1, "pr-tie");
        link(&db, 2, "pr-tie");
        link(&db, 3, "pr-tie");

        resolve_pr_authors(&db, None).unwrap();

        assert_eq!(
            author_of(&db, "pr-tie"),
            Some("bob".to_string()),
            "tie on points → most tasks wins (Bob: 2, Carol: 1)"
        );
    }

    #[test]
    fn orphan_pr_resolves_via_student_github_identity() {
        let db = mk_db();
        seed_team(&db);
        // No task linked → Strategy 1 returns nothing → Strategy 2 kicks in.
        // student_github_identity holds the resolver-learned mapping.
        db.conn
            .execute(
                "INSERT INTO student_github_identity
                    (student_id, identity_kind, identity_value, weight, confidence)
                 VALUES ('alice', 'login', 'alice-gh', 10.0, 1.0)",
                [],
            )
            .unwrap();
        seed_pr_with_null_author(&db, "pr-orphan", Some("alice-gh"));

        resolve_pr_authors(&db, None).unwrap();

        assert_eq!(
            author_of(&db, "pr-orphan"),
            Some("alice".to_string()),
            "orphan PR with github_author_login resolves via student_github_identity"
        );
    }

    #[test]
    fn orphan_pr_with_null_login_resolves_via_pr_commits() {
        let db = mk_db();
        seed_team(&db);
        // PR has no github_author_login but has a commit whose author_email
        // matches student_github_identity.
        db.conn
            .execute(
                "INSERT INTO student_github_identity
                    (student_id, identity_kind, identity_value, weight, confidence)
                 VALUES ('bob', 'email', 'bob@example.com', 5.0, 1.0)",
                [],
            )
            .unwrap();
        seed_pr_with_null_author(&db, "pr-no-login", None);
        db.conn
            .execute(
                "INSERT INTO pr_commits (pr_id, sha, author_login, author_email)
                 VALUES ('pr-no-login', 'sha1', NULL, 'bob@example.com')",
                [],
            )
            .unwrap();

        resolve_pr_authors(&db, None).unwrap();

        assert_eq!(
            author_of(&db, "pr-no-login"),
            Some("bob".to_string()),
            "null github_author_login must fall through to pr_commits author_email"
        );
    }

    #[test]
    fn orphan_pr_with_conflicting_github_identities_left_null_with_error() {
        let db = mk_db();
        seed_team(&db);
        db.conn
            .execute_batch(
                "INSERT INTO student_github_identity
                    (student_id, identity_kind, identity_value, weight, confidence) VALUES
                    ('alice', 'login', 'alice-gh', 10.0, 1.0),
                    ('bob',   'email', 'bob@example.com', 5.0, 1.0);",
            )
            .unwrap();
        seed_pr_with_null_author(&db, "pr-conflict", Some("alice-gh"));
        db.conn
            .execute(
                "INSERT INTO pr_commits (pr_id, sha, author_login, author_email)
                 VALUES ('pr-conflict', 'sha1', NULL, 'bob@example.com')",
                [],
            )
            .unwrap();

        resolve_pr_authors(&db, None).unwrap();

        assert_eq!(
            author_of(&db, "pr-conflict"),
            None,
            "conflicting github identities must leave author_id NULL — \
             attribution silently picking one would mask a real ambiguity"
        );
        let detail: Option<String> = db
            .conn
            .query_row(
                "SELECT attribution_errors FROM pull_requests WHERE id = 'pr-conflict'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            detail
                .unwrap_or_default()
                .contains("conflicting GitHub identities"),
            "conflict is recorded in attribution_errors"
        );
    }

    #[test]
    fn projects_scoping_includes_prs_with_null_author_id() {
        // The bug this guards against: the previous --projects work-set
        // filter joined on pr.author_id and silently dropped every NULL row
        // — exactly the rows that needed re-resolution.
        let db = mk_db();
        seed_team(&db);
        seed_task(&db, 1, "alice", 5);
        seed_pr_with_null_author(&db, "pr-null", None);
        link(&db, 1, "pr-null");

        resolve_pr_authors(&db, Some(&[1])).unwrap();

        assert_eq!(
            author_of(&db, "pr-null"),
            Some("alice".to_string()),
            "scoped run must still pick up Alice's NULL-author PR via the \
             task_pull_requests → tasks.assignee_id path"
        );
    }

    #[test]
    fn projects_scoping_excludes_prs_belonging_to_other_teams() {
        let db = mk_db();
        seed_team(&db);
        // Dave is in project 2; his PR must not be touched when we scope
        // to project 1.
        db.conn
            .execute(
                "INSERT INTO tasks (id, task_key, name, type, status, estimation_points,
                                    assignee_id, sprint_id)
                 VALUES (99, 'D-1', 'Dave task', 'TASK', 'DONE', 5, 'dave', 20)",
                [],
            )
            .unwrap();
        seed_pr_with_null_author(&db, "pr-dave", None);
        link(&db, 99, "pr-dave");

        resolve_pr_authors(&db, Some(&[1])).unwrap();

        assert_eq!(
            author_of(&db, "pr-dave"),
            None,
            "PR linked only to a project-2 task must be untouched by a \
             project-1-scoped resolution pass"
        );
    }

    #[test]
    fn previously_resolved_pr_is_not_overwritten_by_strategy_2_when_strategy_1_succeeds() {
        // If Strategy 1 succeeds the value is the authoritative answer.
        // Strategy 2 (github lookup) is only consulted for orphan PRs.
        let db = mk_db();
        seed_team(&db);
        // The github identity points to Bob, but the linked task is
        // Alice's — task assignment wins.
        db.conn
            .execute(
                "INSERT INTO student_github_identity
                    (student_id, identity_kind, identity_value, weight, confidence)
                 VALUES ('bob', 'login', 'bob-gh', 10.0, 1.0)",
                [],
            )
            .unwrap();
        seed_task(&db, 1, "alice", 5);
        seed_pr_with_null_author(&db, "pr-mixed", Some("bob-gh"));
        link(&db, 1, "pr-mixed");

        resolve_pr_authors(&db, None).unwrap();

        assert_eq!(
            author_of(&db, "pr-mixed"),
            Some("alice".to_string()),
            "task-assignee (Alice) is canonical even when github identity \
             would resolve to Bob — that mismatch is the AUTHOR_MISMATCH \
             flag's job, not the resolver's"
        );
    }
}

#[cfg(test)]
mod skip_logic_tests {
    //! Regression tests for the github-details fetch skip predicate.
    //! The pre-fix code skipped PRs whenever the watermark matched
    //! TrackDev's `updated_at`, even if the recorded data was clearly
    //! incomplete (e.g. merged=true but merged_at empty). 99% of merged
    //! PRs in production ended up locked into that state. The fix is
    //! to require both `pr_unchanged_since_last_fetch` AND
    //! `pr_fully_collected` before skipping.
    use super::*;
    use rusqlite::Connection;
    use sprint_grader_core::db::{apply_schema, PullRequestRow};
    use std::path::PathBuf;

    fn mk_db() -> Database {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        Database {
            db_path: PathBuf::new(),
            conn,
        }
    }

    fn pr_row(merged_at: Option<&str>) -> PullRequestRow {
        PullRequestRow {
            id: "pr-1".to_string(),
            pr_number: 1,
            repo_full_name: Some("org/repo".to_string()),
            state: Some("closed".to_string()),
            merged: true,
            merged_at: merged_at.map(str::to_string),
            author_id: None,
            github_author_login: None,
            updated_at: Some("2026-03-24T09:58:38Z".to_string()),
            last_github_fetch_updated_at: Some("2026-03-24T09:58:38Z".to_string()),
        }
    }

    fn seed_pr(db: &Database, pr: &PullRequestRow) {
        db.conn
            .execute(
                "INSERT INTO pull_requests (id, pr_number, repo_full_name, state, merged, merged_at,
                                            updated_at, last_github_fetch_updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    pr.id,
                    pr.pr_number,
                    pr.repo_full_name,
                    pr.state,
                    pr.merged,
                    pr.merged_at,
                    pr.updated_at,
                    pr.last_github_fetch_updated_at,
                ],
            )
            .unwrap();
    }

    fn seed_commit(db: &Database, pr_id: &str) {
        db.conn
            .execute(
                "INSERT INTO pr_commits (pr_id, sha, author_login, author_email, message, timestamp)
                 VALUES (?, 'sha1', 'someone', 'a@b.c', 'commit', '2026-03-24T09:58:00Z')",
                params![pr_id],
            )
            .unwrap();
    }

    #[test]
    fn merged_pr_with_empty_merged_at_is_not_fully_collected_even_when_watermark_matches() {
        // The bug case: merged=true, merged_at empty, watermark matches
        // updated_at. Pre-fix the code skipped because of the watermark
        // alone; post-fix the AND with pr_fully_collected forces a
        // re-fetch since pr_fully_collected returns false here.
        let db = mk_db();
        let pr = pr_row(Some(""));
        seed_pr(&db, &pr);
        seed_commit(&db, &pr.id);

        assert!(
            pr_unchanged_since_last_fetch(&pr),
            "watermark matches updated_at — unchanged check would skip"
        );
        assert!(
            !pr_fully_collected(&db, &pr).unwrap(),
            "merged_at empty must mark the row as not-fully-collected"
        );
    }

    #[test]
    fn merged_pr_with_complete_data_skips_when_watermark_matches() {
        let db = mk_db();
        let pr = pr_row(Some("2026-03-24T09:58:35Z"));
        seed_pr(&db, &pr);
        seed_commit(&db, &pr.id);

        assert!(pr_unchanged_since_last_fetch(&pr));
        assert!(
            pr_fully_collected(&db, &pr).unwrap(),
            "merged_at present + commits present + watermark matches → skip"
        );
    }
}
