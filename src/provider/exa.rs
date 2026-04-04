use std::{env, time::Duration};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::error::AppError;

const DEFAULT_BASE_URL: &str = "https://api.exa.ai";
const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct ExaClient {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
}

impl ExaClient {
    pub fn new(api_key: String) -> Self {
        Self::with_base_url(DEFAULT_BASE_URL.to_string(), api_key)
    }

    pub fn with_base_url(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            agent: build_agent(),
        }
    }

    pub fn from_env() -> Result<Self, AppError> {
        let api_key = env::var("EXA_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AppError::Tool(
                    "missing EXA_API_KEY; set it in the environment before using Exa tools"
                        .to_string(),
                )
            })?;
        Ok(Self::new(api_key))
    }

    pub fn search(
        &self,
        request: &SearchRequest,
        timeout: Duration,
    ) -> Result<SearchResponse, AppError> {
        self.post("/search", request, timeout)
    }

    fn post<Req, Resp>(
        &self,
        path: &str,
        request: &Req,
        timeout: Duration,
    ) -> Result<Resp, AppError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        let response = self
            .agent
            .post(&url)
            .timeout(timeout)
            .set("x-api-key", &self.api_key)
            .set("Accept", "application/json")
            .set("Content-Type", "application/json")
            .send_json(request);

        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                return Err(AppError::Tool(format!(
                    "exa {path} returned HTTP {code}: {}",
                    render_body(&body)
                )));
            }
            Err(ureq::Error::Transport(error)) => {
                return Err(AppError::Tool(format!(
                    "exa {path} request failed: {error}"
                )));
            }
        };

        response.into_json().map_err(|error| {
            AppError::Tool(format!("failed to decode exa {path} response: {error}"))
        })
    }
}

fn build_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT_CAP)
        .build()
}

fn render_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "(empty response body)".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(rename = "type")]
    pub search_type: String,
    #[serde(rename = "numResults")]
    pub num_results: usize,
    pub contents: SearchContentsSpec,
    #[serde(rename = "includeDomains", skip_serializing_if = "Option::is_none")]
    pub include_domains: Option<Vec<String>>,
    #[serde(rename = "excludeDomains", skip_serializing_if = "Option::is_none")]
    pub exclude_domains: Option<Vec<String>>,
    #[serde(rename = "startPublishedDate", skip_serializing_if = "Option::is_none")]
    pub start_published_date: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchContentsSpec {
    pub highlights: HighlightsSpec,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HighlightsSpec {}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub results: Vec<SearchResult>,
    #[serde(rename = "searchType")]
    pub search_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult {
    pub title: Option<String>,
    pub url: String,
    pub id: Option<String>,
    pub score: Option<f64>,
    #[serde(rename = "publishedDate")]
    pub published_date: Option<String>,
    pub author: Option<String>,
    pub highlights: Option<Vec<String>>,
    #[serde(rename = "highlightScores")]
    pub highlight_scores: Option<Vec<f64>>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ExaClient, HighlightsSpec, SearchContentsSpec, SearchRequest};
    use crate::error::AppError;
    use crate::test_support::{request_json, spawn_json_http_server};

    #[test]
    fn search_request_sends_num_results_and_start_published_date() {
        let (address, request, handle) = spawn_json_http_server(
            "200 OK",
            r#"{"results":[],"searchType":"auto"}"#,
        );
        let client = ExaClient::with_base_url(format!("http://{address}"), "test-key".to_string());
        let search = SearchRequest {
            query: "rust async runtimes".to_string(),
            search_type: "auto".to_string(),
            num_results: 5,
            contents: SearchContentsSpec {
                highlights: HighlightsSpec::default(),
            },
            include_domains: None,
            exclude_domains: None,
            start_published_date: Some("2026-03-28T00:00:00Z".to_string()),
        };

        let response = client
            .search(&search, Duration::from_secs(2))
            .expect("search response");
        assert!(response.results.is_empty());

        handle.join().expect("join server");
        let raw_request = request.lock().expect("request lock").clone();
        assert!(raw_request.starts_with("POST /search HTTP/1.1"));
        assert!(
            raw_request
                .to_ascii_lowercase()
                .contains("x-api-key: test-key")
        );
        assert!(
            raw_request
                .to_ascii_lowercase()
                .contains("accept: application/json")
        );

        let request_json = request_json(&raw_request);
        assert_eq!(request_json["query"], "rust async runtimes");
        assert_eq!(request_json["type"], "auto");
        assert_eq!(request_json["numResults"], 5);
        assert_eq!(request_json["startPublishedDate"], "2026-03-28T00:00:00Z");
    }

    #[test]
    fn non_2xx_responses_surface_as_tool_errors() {
        let (address, _request, handle) =
            spawn_json_http_server("401 Unauthorized", r#"{"error":"bad key"}"#);
        let client = ExaClient::with_base_url(format!("http://{address}"), "test-key".to_string());
        let search = SearchRequest {
            query: "rust".to_string(),
            search_type: "auto".to_string(),
            num_results: 5,
            contents: SearchContentsSpec {
                highlights: HighlightsSpec::default(),
            },
            include_domains: None,
            exclude_domains: None,
            start_published_date: None,
        };

        let error = client
            .search(&search, Duration::from_secs(2))
            .expect_err("expected error");
        handle.join().expect("join server");

        match error {
            AppError::Tool(message) => {
                assert!(message.contains("exa /search returned HTTP 401"));
                assert!(message.contains(r#"{"error":"bad key"}"#));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn transport_failures_surface_as_tool_errors() {
        let client =
            ExaClient::with_base_url("http://127.0.0.1:1".to_string(), "test-key".to_string());
        let search = SearchRequest {
            query: "rust".to_string(),
            search_type: "auto".to_string(),
            num_results: 5,
            contents: SearchContentsSpec {
                highlights: HighlightsSpec::default(),
            },
            include_domains: None,
            exclude_domains: None,
            start_published_date: None,
        };

        let error = client
            .search(&search, Duration::from_millis(200))
            .expect_err("expected error");

        match error {
            AppError::Tool(message) => {
                assert!(message.contains("exa /search request failed"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
