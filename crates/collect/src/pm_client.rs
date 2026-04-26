//! TrackDev project management tool API client (READ-ONLY).
//!
//! Mirrors `src/collect/pm_client.py`. GET-only; never modifies TrackDev state.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::StatusCode;
use serde_json::Value;
use tracing::warn;

const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE_SECS: u64 = 2;

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
            .timeout(Duration::from_secs(30))
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

        for attempt in 0..MAX_RETRIES {
            match self.client.get(&url).send() {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return resp.json::<Value>().map_err(|e| TrackDevError::Json {
                            path: path.to_string(),
                            source: e,
                        });
                    }
                    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(%status, path, wait_s = wait, "TrackDev retry");
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                    let body = resp.text().unwrap_or_default();
                    return Err(TrackDevError::Http {
                        method: "GET".to_string(),
                        path: path.to_string(),
                        status: status.as_u16(),
                        body,
                    });
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < MAX_RETRIES {
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(path, wait_s = wait, "TrackDev request error — retrying");
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                }
            }
        }
        Err(TrackDevError::RequestFailed {
            method: "GET".to_string(),
            path: path.to_string(),
            retries: MAX_RETRIES,
            source: last_err.expect("loop always populates last_err before exit"),
        })
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
