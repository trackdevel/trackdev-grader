//! TrackDev project management tool API client (READ-ONLY).
//!
//! Mirrors `src/collect/pm_client.py`. GET-only; never modifies TrackDev state.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::StatusCode;
use serde_json::Value;
use tracing::warn;

const MAX_RETRIES: u32 = 5;
const BACKOFF_BASE_SECS: u64 = 2;
// /export/tasks for a fully-populated project can stream multiple MB of
// nested JSON. The 30 s default was empirically too tight (pds26-3a hit it
// repeatably overnight). 120 s covers the realistic worst case while
// still surfacing genuine hangs within a minute or two.
const REQUEST_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum TrackDevError {
    #[error("TRACKDEV_TOKEN is empty — cannot call TrackDev API")]
    EmptyToken,

    #[error("{method} {path} failed: {status} {body}")]
    Http {
        method: String,
        path: String,
        status: u16,
        body: String,
    },

    #[error("{method} {path} failed after {retries} retries: {source}")]
    RequestFailed {
        method: String,
        path: String,
        retries: u32,
        #[source]
        source: reqwest::Error,
    },

    #[error("failed to parse JSON response from {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: reqwest::Error,
    },
}

pub struct TrackDevClient {
    base_url: String,
    client: Client,
}

impl TrackDevClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self, TrackDevError> {
        if token.is_empty() {
            return Err(TrackDevError::EmptyToken);
        }
        let mut headers = HeaderMap::new();
        let val = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| TrackDevError::EmptyToken)?;
        headers.insert(AUTHORIZATION, val);

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .expect("reqwest client build");

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    fn get(&self, path: &str) -> Result<Value, TrackDevError> {
        let url = format!("{}{path}", self.base_url);
        let mut last_err: Option<reqwest::Error> = None;
        let mut last_was_json_failure = false;

        for attempt in 0..MAX_RETRIES {
            match self.client.get(&url).send() {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        match resp.json::<Value>() {
                            Ok(v) => return Ok(v),
                            Err(e) => {
                                // Body streaming or parse failure. When the
                                // server is slow to render a large export
                                // (`/export/tasks`), reqwest's overall
                                // request timeout fires mid-stream and
                                // surfaces here as a decode error. Treat as
                                // transient and retry with backoff.
                                last_err = Some(e);
                                last_was_json_failure = true;
                                if attempt + 1 < MAX_RETRIES {
                                    let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                                    warn!(
                                        path,
                                        wait_s = wait,
                                        attempt = attempt + 1,
                                        max = MAX_RETRIES,
                                        "TrackDev JSON decode failed — retrying"
                                    );
                                    std::thread::sleep(Duration::from_secs(wait));
                                    continue;
                                }
                            }
                        }
                    } else if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(%status, path, wait_s = wait, "TrackDev retry");
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    } else {
                        let body = resp.text().unwrap_or_default();
                        return Err(TrackDevError::Http {
                            method: "GET".to_string(),
                            path: path.to_string(),
                            status: status.as_u16(),
                            body,
                        });
                    }
                }
                Err(e) => {
                    last_err = Some(e);
                    last_was_json_failure = false;
                    if attempt + 1 < MAX_RETRIES {
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(path, wait_s = wait, "TrackDev request error — retrying");
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                }
            }
        }
        let source = last_err.expect("loop always populates last_err before exit");
        if last_was_json_failure {
            // Preserve the more informative Json error variant when the
            // terminal failure was a decode error rather than a transport
            // error.
            Err(TrackDevError::Json {
                path: path.to_string(),
                source,
            })
        } else {
            Err(TrackDevError::RequestFailed {
                method: "GET".to_string(),
                path: path.to_string(),
                retries: MAX_RETRIES,
                source,
            })
        }
    }

    /// `GET /courses/{course_id}/details` → CourseDetailsDTO.
    pub fn get_course_details(&self, course_id: u32) -> Result<Value, TrackDevError> {
        self.get(&format!("/courses/{course_id}/details"))
    }

    /// `GET /projects/{project_id}/sprints` → `sprints` array (extracted from wrapping object).
    pub fn get_project_sprints(&self, project_id: i64) -> Result<Vec<Value>, TrackDevError> {
        let v = self.get(&format!("/projects/{project_id}/sprints"))?;
        Ok(v.get("sprints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// `GET /sprints/{id}/board` → SprintBoardDTO.
    pub fn get_sprint_board(&self, sprint_id: i64) -> Result<Value, TrackDevError> {
        self.get(&format!("/sprints/{sprint_id}/board"))
    }

    /// `GET /projects/{project_id}/github-repos`.
    pub fn get_github_repos(&self, project_id: i64) -> Result<Value, TrackDevError> {
        self.get(&format!("/projects/{project_id}/github-repos"))
    }

    /// `GET /projects/{project_id}/export/team` → TeamExportDTO.
    /// Team members with roles and profile attribute values.
    pub fn get_project_export_team(&self, project_id: i64) -> Result<Value, TrackDevError> {
        self.get(&format!("/projects/{project_id}/export/team"))
    }

    /// `GET /projects/{project_id}/export/tasks` → TasksExportDTO.
    /// All tasks across all sprints (all types, including subtasks), with
    /// `activeSprints`, `pullRequests`, and task attribute values.
    pub fn get_project_export_tasks(&self, project_id: i64) -> Result<Value, TrackDevError> {
        self.get(&format!("/projects/{project_id}/export/tasks"))
    }

    /// `GET /projects/{project_id}/export/pull-requests` → PullRequestsExportDTO.
    /// All PRs linked to any task in the project, with author, task refs,
    /// and PR attribute values.
    pub fn get_project_export_pull_requests(
        &self,
        project_id: i64,
    ) -> Result<Value, TrackDevError> {
        self.get(&format!("/projects/{project_id}/export/pull-requests"))
    }
}
