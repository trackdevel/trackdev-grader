//! Orchestrates data collection from TrackDev and GitHub.
//! Mirrors `src/collect/collector.py`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use chrono::Utc;
use rusqlite::params;
use serde_json::Value;
use tracing::{info, warn};

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

    // Step 4 — PR author attribution.
    resolve_pr_authors(db)?;

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
    let mut skipped_unchanged = 0usize;
    info!(total, force_refresh, "Fetching GitHub details for PRs");

    for (i, pr) in prs.iter().enumerate() {
        let repo = pr.repo_full_name.as_deref().unwrap_or("");
        let number = pr.pr_number;
        if repo.is_empty() || number == 0 {
            warn!(pr_id = %pr.id, "Skipping PR — missing repo or number");
            continue;
        }

        if !force_refresh && pr_unchanged_since_last_fetch(pr) {
            skipped_unchanged += 1;
            continue;
        }

        if !force_refresh && pr_fully_collected(db, pr)? {
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
            Err(e) => warn!(repo, number, error = %e, "Failed to fetch PR"),
        }

        // Commits.
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
                }
                if let Some(tag) = new_etag.as_deref() {
                    if !tag.is_empty() {
                        db.upsert_pr_github_etag(&pr.id, "commits", tag, Some(&now_iso))?;
                    }
                }
            }
            Err(e) => warn!(repo, number, error = %e, "Failed to fetch commits"),
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
        skipped_unchanged,
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

/// Resolve PR author → TrackDev student. Priority:
/// 1. github_author_login → students.github_login
/// 2. fallback via github_users.student_id
/// 3. sole task assignee across linked non-USER_STORY tasks
fn resolve_pr_authors(db: &Database) -> Result<(), CollectError> {
    let mut stmt = db
        .conn
        .prepare("SELECT id, author_id, github_author_login FROM pull_requests")?;
    let rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut resolved_count = 0usize;
    let mut error_count = 0usize;
    let total = rows.len();

    for (pr_id, trackdev_author_id, gh_login) in rows {
        let mut errors: Vec<String> = Vec::new();
        let mut resolved_id: Option<String> = None;

        // Strategy 1 — direct + github_users fallback.
        if let Some(login) = gh_login.as_deref() {
            let direct: Option<String> = db
                .conn
                .query_row(
                    "SELECT id FROM students WHERE LOWER(github_login) = LOWER(?)",
                    [login],
                    |r| r.get(0),
                )
                .ok();
            if direct.is_some() {
                resolved_id = direct;
            } else {
                let via_gu: Option<String> = db
                    .conn
                    .query_row(
                        "SELECT student_id FROM github_users WHERE LOWER(login) = LOWER(?)",
                        [login],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(Some(sid)) = via_gu.map(Some) {
                    resolved_id = Some(sid);
                }
            }
        }

        // Strategy 2 — task assignee fallback.
        if resolved_id.is_none() {
            let mut stmt2 = db.conn.prepare(
                "SELECT DISTINCT t.assignee_id
                 FROM tasks t
                 JOIN task_pull_requests tpr ON tpr.task_id = t.id
                 WHERE tpr.pr_id = ? AND t.assignee_id IS NOT NULL
                   AND t.type != 'USER_STORY'",
            )?;
            let assignees: Vec<String> = stmt2
                .query_map([&pr_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?;
            match assignees.as_slice() {
                [only] => resolved_id = Some(only.clone()),
                [] => {
                    if let Some(login) = gh_login.as_deref() {
                        errors.push(format!(
                            "GitHub login '{login}' not matched to any student and no task assignee available"
                        ));
                    } else {
                        errors.push(
                            "No GitHub author login and no task assignee available".to_string(),
                        );
                    }
                }
                many => {
                    // Multiple distinct assignees → error + best guess.
                    let mut names: Vec<String> = Vec::new();
                    for aid in many {
                        let full_name: Option<String> = db
                            .conn
                            .query_row("SELECT full_name FROM students WHERE id = ?", [aid], |r| {
                                r.get(0)
                            })
                            .ok();
                        names.push(full_name.unwrap_or_else(|| aid.clone()));
                    }
                    errors.push(format!(
                        "PR linked to tasks with different assignees: {names:?}"
                    ));
                    resolved_id = Some(many[0].clone());
                }
            }
        }

        let final_id = resolved_id.clone().or(trackdev_author_id);
        let error_json = if errors.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&errors)?)
        };

        db.conn.execute(
            "UPDATE pull_requests SET author_id = ?, attribution_errors = ? WHERE id = ?",
            params![final_id, error_json, pr_id],
        )?;

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
