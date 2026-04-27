//! GitHub API client (READ-ONLY). Mirrors `src/collect/github_client.py`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, IF_NONE_MATCH, USER_AGENT};
use reqwest::StatusCode;
use serde_json::Value;
use tracing::warn;

/// Outcome of a conditional GET.
pub enum ConditionalResult<T> {
    /// Server returned 304; caller may reuse cached data.
    NotModified,
    /// Server returned 200 with a fresh body. `etag` is the value to store
    /// for the next request; `None` if the server omitted the header.
    Fresh { value: T, etag: Option<String> },
}

type PageResponse = (Value, Option<String>, Option<String>);

const GITHUB_API: &str = "https://api.github.com";
const MAX_RETRIES: u32 = 3;
const BACKOFF_BASE_SECS: u64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum GitHubClientError {
    #[error("GITHUB_TOKEN is empty — cannot call GitHub API")]
    EmptyToken,

    #[error("GitHub API error: {status} {body}")]
    Http { status: u16, body: String },

    #[error("GitHub API failed after {0} retries: {1}")]
    RequestFailed(u32, #[source] reqwest::Error),

    #[error("failed to parse GitHub JSON response: {0}")]
    Json(#[source] reqwest::Error),
}

pub struct GitHubClient {
    client: Client,
    call_count: AtomicU64,
}

impl GitHubClient {
    pub fn new(token: &str) -> Result<Self, GitHubClientError> {
        if token.is_empty() {
            return Err(GitHubClientError::EmptyToken);
        }
        let mut headers = HeaderMap::new();
        let auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| GitHubClientError::EmptyToken)?;
        headers.insert(AUTHORIZATION, auth);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        // GitHub rejects requests without a User-Agent with 403
        // "Request forbidden by administrative rules". reqwest sets one by
        // default on the async Client but not reliably for the blocking
        // Client — send an explicit one tied to the crate version.
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!(
                "sprint-grader/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/udg-pds)"
            )),
        );

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client build");

        Ok(Self {
            client,
            call_count: AtomicU64::new(0),
        })
    }

    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::Relaxed)
    }

    /// GET with retry, rate-limit handling, and `Link: rel="next"` pagination.
    /// For array endpoints, pages are concatenated. Object endpoints return the first response.
    fn get(&self, path: &str) -> Result<Value, GitHubClientError> {
        let url = format!("{GITHUB_API}{path}");
        match self.get_conditional_url(&url, None)? {
            ConditionalResult::Fresh { value, .. } => Ok(value),
            // Cannot happen: conditional without an ETag never 304s.
            ConditionalResult::NotModified => Ok(Value::Array(Vec::new())),
        }
    }

    /// Conditional GET: if `etag` is provided, sends `If-None-Match` on the
    /// first request. 304 on the first page → `NotModified` (no further
    /// fetching). 200 → pagination runs as usual and the first-page ETag is
    /// returned for the caller to store.
    fn get_conditional_url(
        &self,
        first_url: &str,
        etag: Option<&str>,
    ) -> Result<ConditionalResult<Value>, GitHubClientError> {
        let mut url = first_url.to_string();
        let (first_body, first_next, first_etag) = match self.get_page(&url, etag)? {
            Some(page) => page,
            None => return Ok(ConditionalResult::NotModified),
        };
        let stored_etag = first_etag;

        match first_body {
            Value::Array(items) => {
                let mut combined = items;
                url = match first_next {
                    Some(u) => u,
                    None => {
                        return Ok(ConditionalResult::Fresh {
                            value: Value::Array(combined),
                            etag: stored_etag,
                        })
                    }
                };
                loop {
                    // Subsequent pages: never send If-None-Match — only the
                    // first page's ETag is the cache key.
                    let (body, next_url, _etag) = self
                        .get_page(&url, None)?
                        .expect("unconditional GET cannot 304");
                    if let Value::Array(items) = body {
                        combined.extend(items);
                    }
                    match next_url {
                        Some(next) => url = next,
                        None => break,
                    }
                }
                Ok(ConditionalResult::Fresh {
                    value: Value::Array(combined),
                    etag: stored_etag,
                })
            }
            other => Ok(ConditionalResult::Fresh {
                value: other,
                etag: stored_etag,
            }),
        }
    }

    fn get_page(
        &self,
        url: &str,
        if_none_match: Option<&str>,
    ) -> Result<Option<PageResponse>, GitHubClientError> {
        let mut last_err: Option<reqwest::Error> = None;

        for attempt in 0..MAX_RETRIES {
            let req: RequestBuilder = match if_none_match {
                Some(tag) if !tag.is_empty() => self.client.get(url).header(
                    IF_NONE_MATCH,
                    HeaderValue::from_str(tag).unwrap_or_else(|_| HeaderValue::from_static("")),
                ),
                _ => self.client.get(url),
            };
            match req.send() {
                Ok(resp) => {
                    self.call_count.fetch_add(1, Ordering::Relaxed);

                    if let Some(remaining) = resp
                        .headers()
                        .get("X-RateLimit-Remaining")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok())
                    {
                        if remaining < 100 {
                            warn!(remaining, "GitHub rate limit low");
                        }
                    }

                    let status = resp.status();

                    if status == StatusCode::NOT_MODIFIED {
                        return Ok(None);
                    }

                    if status == StatusCode::FORBIDDEN {
                        let reset_ts = resp
                            .headers()
                            .get("X-RateLimit-Reset")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(0);
                        let text = resp.text().unwrap_or_default();
                        if text.to_lowercase().contains("rate limit") {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let wait = reset_ts.saturating_sub(now).max(10);
                            warn!(wait_s = wait, "GitHub rate limit hit — waiting");
                            std::thread::sleep(Duration::from_secs(wait));
                            continue;
                        }
                        return Err(GitHubClientError::Http {
                            status: status.as_u16(),
                            body: text,
                        });
                    }

                    if !status.is_success() {
                        let body = resp.text().unwrap_or_default();
                        if status.is_server_error() {
                            let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                            warn!(%status, wait_s = wait, "GitHub 5xx — retrying");
                            std::thread::sleep(Duration::from_secs(wait));
                            continue;
                        }
                        return Err(GitHubClientError::Http {
                            status: status.as_u16(),
                            body,
                        });
                    }

                    let next_url = resp
                        .headers()
                        .get("Link")
                        .and_then(|h| h.to_str().ok())
                        .and_then(parse_next_link);
                    let resp_etag = resp
                        .headers()
                        .get(reqwest::header::ETAG)
                        .and_then(|h| h.to_str().ok())
                        .map(str::to_string);
                    let value = resp.json::<Value>().map_err(GitHubClientError::Json)?;
                    return Ok(Some((value, next_url, resp_etag)));
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < MAX_RETRIES {
                        let wait = BACKOFF_BASE_SECS.pow(attempt + 1);
                        warn!(wait_s = wait, "GitHub request error — retrying");
                        std::thread::sleep(Duration::from_secs(wait));
                        continue;
                    }
                }
            }
        }
        Err(GitHubClientError::RequestFailed(
            MAX_RETRIES,
            last_err.expect("loop always populates last_err before exit"),
        ))
    }

    pub fn get_pr(&self, repo: &str, pr_number: i64) -> Result<Value, GitHubClientError> {
        self.get(&format!("/repos/{repo}/pulls/{pr_number}"))
    }

    pub fn get_pr_commits(
        &self,
        repo: &str,
        pr_number: i64,
    ) -> Result<Vec<Value>, GitHubClientError> {
        let v = self.get(&format!("/repos/{repo}/pulls/{pr_number}/commits"))?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub fn get_pr_reviews(
        &self,
        repo: &str,
        pr_number: i64,
    ) -> Result<Vec<Value>, GitHubClientError> {
        let v = self.get(&format!("/repos/{repo}/pulls/{pr_number}/reviews"))?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    /// Conditional variant of `get_pr`. Sends `If-None-Match: <etag>` when
    /// `etag` is provided; 304 short-circuits with no JSON body.
    pub fn get_pr_conditional(
        &self,
        repo: &str,
        pr_number: i64,
        etag: Option<&str>,
    ) -> Result<ConditionalResult<Value>, GitHubClientError> {
        let url = format!("{GITHUB_API}/repos/{repo}/pulls/{pr_number}");
        self.get_conditional_url(&url, etag)
    }

    pub fn get_pr_commits_conditional(
        &self,
        repo: &str,
        pr_number: i64,
        etag: Option<&str>,
    ) -> Result<ConditionalResult<Vec<Value>>, GitHubClientError> {
        let url = format!("{GITHUB_API}/repos/{repo}/pulls/{pr_number}/commits");
        match self.get_conditional_url(&url, etag)? {
            ConditionalResult::NotModified => Ok(ConditionalResult::NotModified),
            ConditionalResult::Fresh { value, etag } => Ok(ConditionalResult::Fresh {
                value: value.as_array().cloned().unwrap_or_default(),
                etag,
            }),
        }
    }

    pub fn get_pr_reviews_conditional(
        &self,
        repo: &str,
        pr_number: i64,
        etag: Option<&str>,
    ) -> Result<ConditionalResult<Vec<Value>>, GitHubClientError> {
        let url = format!("{GITHUB_API}/repos/{repo}/pulls/{pr_number}/reviews");
        match self.get_conditional_url(&url, etag)? {
            ConditionalResult::NotModified => Ok(ConditionalResult::NotModified),
            ConditionalResult::Fresh { value, etag } => Ok(ConditionalResult::Fresh {
                value: value.as_array().cloned().unwrap_or_default(),
                etag,
            }),
        }
    }

    pub fn get_user(&self, login: &str) -> Result<Value, GitHubClientError> {
        self.get(&format!("/users/{login}"))
    }
}

/// Parse the `Link` header and return the URL for the `rel="next"` entry.
fn parse_next_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        if part.contains(r#"rel="next""#) {
            let raw = part.split(';').next()?.trim();
            let trimmed = raw.trim_start_matches('<').trim_end_matches('>');
            return Some(trimmed.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_next_link;

    #[test]
    fn next_link_extraction() {
        let h = r#"<https://api.github.com/foo?page=2>; rel="next", <https://api.github.com/foo?page=5>; rel="last""#;
        assert_eq!(
            parse_next_link(h),
            Some("https://api.github.com/foo?page=2".to_string())
        );
    }

    #[test]
    fn no_next_returns_none() {
        let h = r#"<https://api.github.com/foo?page=5>; rel="last""#;
        assert_eq!(parse_next_link(h), None);
    }
}
