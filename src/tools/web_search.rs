use std::{collections::HashSet, time::Duration};

use serde::Serialize;
use serde_json::{Value, json};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    error::AppError,
    provider::exa::{
        ExaClient, HighlightsSpec, SearchContentsSpec, SearchRequest, SearchResponse, SearchResult,
    },
    tools::{ToolContext, require_non_empty_trimmed_string},
};

const AUTO_TIMEOUT: Duration = Duration::from_secs(15);
const NEURAL_TIMEOUT: Duration = Duration::from_secs(10);
const DEEP_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_SEARCH_TYPE: &str = "auto";
const DEFAULT_NUM_RESULTS: usize = 5;
const MIN_NUM_RESULTS: usize = 1;
const MAX_NUM_RESULTS: usize = 20;
const ALLOWED_SEARCH_TYPES: &[&str] = &["auto", "neural", "deep"];

pub fn web_search_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "web_search",
        description: "Search the web for relevant sources with Exa. Use this first when you need current or external information, then use web_fetch for live verification or deeper reading. Supports type=auto|neural|deep. Requires EXA_API_KEY in the environment. Cite the exact URLs you use in the final answer.",
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "type": {
                    "type": "string",
                    "description": "Exa search mode: auto, neural, or deep"
                },
                "num_results": {
                    "type": "integer",
                    "description": "How many results to return (1-20, default 5)"
                },
                "published_within_days": {
                    "type": "integer",
                    "description": "Only include results published within the last N days (1-365)"
                },
                "include_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only include results from these domains"
                },
                "exclude_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exclude results from these domains"
                }
            },
            "required": ["query"]
        }),
    }
}

pub fn run_web_search(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let input = parse_search_input(arguments)?;
    let timeout = context
        .remaining_budget()?
        .min(search_timeout_for_type(&input.search_type));
    let client = context.exa_client()?;
    let output = search_with_client(input, timeout, OffsetDateTime::now_utc(), &client)?;
    serde_json::to_value(output).map_err(AppError::from)
}

pub fn search_cli(
    query: &str,
    search_type: Option<&str>,
    num_results: Option<usize>,
    published_within_days: Option<usize>,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<WebSearchOutput, AppError> {
    let input = build_cli_input(
        query,
        search_type,
        num_results,
        published_within_days,
        include_domains,
        exclude_domains,
    )?;
    let timeout = search_timeout_for_type(&input.search_type);
    let client = ExaClient::from_env()?;
    search_with_client(input, timeout, OffsetDateTime::now_utc(), &client)
}

pub fn render_cli_output(result: &WebSearchOutput) -> String {
    let mut output = String::new();
    output.push_str(&format!("Query:          {}\n", result.query));
    output.push_str(&format!("Type:           {}\n", result.search_type));
    if let Some(requested_type) = &result.requested_type {
        output.push_str(&format!("Requested type: {}\n", requested_type));
    }
    output.push_str(&format!("Requested:      {}\n", result.num_results));
    output.push_str(&format!("Result count:   {}\n", result.result_count));
    if result.results.is_empty() {
        output.push_str("\n(no results)\n");
        return output;
    }

    for (index, result) in result.results.iter().enumerate() {
        output.push_str(&format!(
            "\n{}. {}\n",
            index + 1,
            result.title.as_deref().unwrap_or("(untitled)")
        ));
        output.push_str(&format!("   URL: {}\n", result.url));
        if let Some(score) = result.score {
            output.push_str(&format!("   Score: {:.3}\n", score));
        }
        if let Some(published_date) = &result.published_date {
            output.push_str(&format!("   Published: {}\n", published_date));
        }
        if !result.highlights.is_empty() {
            output.push_str("   Highlights:\n");
            for highlight in &result.highlights {
                output.push_str(&format!("   - {}\n", highlight));
            }
        }
    }

    output
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebSearchInput {
    query: String,
    search_type: String,
    num_results: usize,
    published_within_days: Option<usize>,
    include_domains: Vec<String>,
    exclude_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WebSearchOutput {
    pub query: String,
    #[serde(rename = "type")]
    pub search_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_type: Option<String>,
    pub num_results: usize,
    pub results: Vec<WebSearchResultOutput>,
    pub result_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WebSearchResultOutput {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlights: Vec<String>,
}

fn search_with_client(
    input: WebSearchInput,
    timeout: Duration,
    now: OffsetDateTime,
    client: &ExaClient,
) -> Result<WebSearchOutput, AppError> {
    let request = build_search_request(&input, now)?;
    let response = client.search(&request, timeout)?;
    Ok(build_search_output(
        input.query,
        input.search_type,
        input.num_results,
        response,
    ))
}

fn build_search_request(
    input: &WebSearchInput,
    now: OffsetDateTime,
) -> Result<SearchRequest, AppError> {
    let start_published_date = match input.published_within_days {
        Some(days) => Some((now - TimeDuration::days(days as i64)).format(&Rfc3339)?),
        None => None,
    };

    Ok(SearchRequest {
        query: input.query.clone(),
        search_type: input.search_type.clone(),
        num_results: input.num_results,
        contents: SearchContentsSpec {
            highlights: HighlightsSpec::default(),
        },
        include_domains: (!input.include_domains.is_empty()).then(|| input.include_domains.clone()),
        exclude_domains: (!input.exclude_domains.is_empty()).then(|| input.exclude_domains.clone()),
        start_published_date,
    })
}

fn build_search_output(
    query: String,
    requested_search_type: String,
    num_results: usize,
    response: SearchResponse,
) -> WebSearchOutput {
    let effective_search_type = response
        .search_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&requested_search_type)
        .to_string();
    let results = response
        .results
        .into_iter()
        .map(compact_search_result)
        .collect::<Vec<_>>();
    let result_count = results.len();
    WebSearchOutput {
        query,
        requested_type: (effective_search_type != requested_search_type)
            .then_some(requested_search_type),
        search_type: effective_search_type,
        num_results,
        results,
        result_count,
    }
}

fn compact_search_result(result: SearchResult) -> WebSearchResultOutput {
    WebSearchResultOutput {
        url: result.url,
        title: compact_optional_string(result.title),
        published_date: compact_optional_string(result.published_date),
        score: result.score,
        highlights: compact_strings(result.highlights.unwrap_or_default()),
    }
}

fn parse_search_input(arguments: &Value) -> Result<WebSearchInput, AppError> {
    let query = require_non_empty_trimmed_string(arguments, "query")?;
    let search_type = parse_search_type(arguments)?;
    let num_results = parse_num_results(arguments)?;
    let published_within_days = parse_published_within_days(arguments)?;
    let include_domains = parse_domain_array(arguments, "include_domains")?;
    let exclude_domains = parse_domain_array(arguments, "exclude_domains")?;
    ensure_no_domain_overlap(&include_domains, &exclude_domains)?;

    Ok(WebSearchInput {
        query,
        search_type,
        num_results,
        published_within_days,
        include_domains,
        exclude_domains,
    })
}

fn build_cli_input(
    query: &str,
    search_type: Option<&str>,
    num_results: Option<usize>,
    published_within_days: Option<usize>,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<WebSearchInput, AppError> {
    let query = validate_query(query)?;
    let search_type = validate_search_type(search_type)?;
    let num_results = validate_num_results(num_results.map(|value| value as u64))?;
    let published_within_days =
        validate_published_within_days(published_within_days.map(|value| value as u64))?;
    let include_domains = normalize_domains(include_domains.to_vec());
    let exclude_domains = normalize_domains(exclude_domains.to_vec());
    ensure_no_domain_overlap(&include_domains, &exclude_domains)?;

    Ok(WebSearchInput {
        query,
        search_type,
        num_results,
        published_within_days,
        include_domains,
        exclude_domains,
    })
}

fn validate_query(query: &str) -> Result<String, AppError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(AppError::Tool(
            "tool argument `query` must be a non-empty string".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn parse_search_type(arguments: &Value) -> Result<String, AppError> {
    let Some(value) = arguments.get("type") else {
        return validate_search_type(None);
    };
    if value.is_null() {
        return validate_search_type(None);
    }
    let raw = value.as_str().ok_or_else(|| {
        AppError::Tool(format!(
            "tool argument `type` must be one of {}",
            ALLOWED_SEARCH_TYPES.join("|")
        ))
    })?;
    validate_search_type(Some(raw))
}

pub(crate) fn validate_search_type(value: Option<&str>) -> Result<String, AppError> {
    let Some(raw) = value else {
        return Ok(DEFAULT_SEARCH_TYPE.to_string());
    };
    let normalized = raw.trim();
    if ALLOWED_SEARCH_TYPES.contains(&normalized) {
        return Ok(normalized.to_string());
    }
    Err(AppError::Tool(format!(
        "tool argument `type` must be one of {}",
        ALLOWED_SEARCH_TYPES.join("|")
    )))
}

fn parse_num_results(arguments: &Value) -> Result<usize, AppError> {
    let Some(value) = arguments.get("num_results") else {
        return validate_num_results(None);
    };
    if value.is_null() {
        return validate_num_results(None);
    }
    let num_results = value.as_u64().ok_or_else(|| {
        AppError::Tool(format!(
            "tool argument `num_results` must be an integer between {MIN_NUM_RESULTS} and {MAX_NUM_RESULTS}"
        ))
    })?;
    validate_num_results(Some(num_results))
}

pub(crate) fn validate_num_results(value: Option<u64>) -> Result<usize, AppError> {
    let Some(num_results) = value else {
        return Ok(DEFAULT_NUM_RESULTS);
    };
    if !(MIN_NUM_RESULTS as u64..=MAX_NUM_RESULTS as u64).contains(&num_results) {
        return Err(AppError::Tool(format!(
            "tool argument `num_results` must be between {MIN_NUM_RESULTS} and {MAX_NUM_RESULTS}"
        )));
    }
    usize::try_from(num_results).map_err(|_| {
        AppError::Tool(format!(
            "tool argument `num_results` must be between {MIN_NUM_RESULTS} and {MAX_NUM_RESULTS}"
        ))
    })
}

fn search_timeout_for_type(search_type: &str) -> Duration {
    match search_type {
        "auto" => AUTO_TIMEOUT,
        "neural" => NEURAL_TIMEOUT,
        "deep" => DEEP_TIMEOUT,
        _ => AUTO_TIMEOUT,
    }
}

fn parse_published_within_days(arguments: &Value) -> Result<Option<usize>, AppError> {
    let Some(value) = arguments.get("published_within_days") else {
        return validate_published_within_days(None);
    };
    if value.is_null() {
        return validate_published_within_days(None);
    }
    let days = value.as_u64().ok_or_else(|| {
        AppError::Tool(
            "tool argument `published_within_days` must be an integer between 1 and 365"
                .to_string(),
        )
    })?;
    validate_published_within_days(Some(days))
}

pub(crate) fn validate_published_within_days(
    value: Option<u64>,
) -> Result<Option<usize>, AppError> {
    let Some(days) = value else {
        return Ok(None);
    };
    if !(1_u64..=365_u64).contains(&days) {
        return Err(AppError::Tool(
            "tool argument `published_within_days` must be between 1 and 365".to_string(),
        ));
    }
    let days = usize::try_from(days).map_err(|_| {
        AppError::Tool(
            "tool argument `published_within_days` must be between 1 and 365".to_string(),
        )
    })?;
    Ok(Some(days))
}

fn normalize_domains(domains: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for domain in domains {
        let lowered = domain.trim().to_ascii_lowercase();
        if lowered.is_empty() {
            continue;
        }
        if seen.insert(lowered.clone()) {
            normalized.push(lowered);
        }
    }
    normalized
}

fn parse_domain_array(arguments: &Value, key: &str) -> Result<Vec<String>, AppError> {
    let Some(value) = arguments.get(key) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let items = value.as_array().ok_or_else(|| {
        AppError::Tool(format!("tool argument `{key}` must be an array of strings"))
    })?;
    let domains = items
        .iter()
        .map(|item| {
            item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                AppError::Tool(format!("tool argument `{key}` must be an array of strings"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(normalize_domains(domains))
}

fn ensure_no_domain_overlap(
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<(), AppError> {
    let excludes = exclude_domains.iter().collect::<HashSet<_>>();
    let overlapping = include_domains
        .iter()
        .filter(|domain| excludes.contains(domain))
        .cloned()
        .collect::<Vec<_>>();
    if overlapping.is_empty() {
        return Ok(());
    }
    Err(AppError::Tool(format!(
        "include_domains and exclude_domains overlap: {}",
        overlapping.join(", ")
    )))
}

fn compact_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|entry| {
        let normalized = normalize_whitespace(&entry);
        (!normalized.is_empty()).then_some(normalized)
    })
}

fn compact_strings(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| {
            let normalized = normalize_whitespace(&value);
            (!normalized.is_empty()).then_some(normalized)
        })
        .collect()
}

/// Normalize scraped web content whitespace:
/// - \r\n → \n
/// - collapse runs of spaces/tabs within a line to a single space
/// - trim trailing whitespace per line
/// - collapse 3+ consecutive newlines to 2 (one blank line max)
/// - trim leading/trailing whitespace from the whole string
fn normalize_whitespace(input: &str) -> String {
    let input = input.replace("\r\n", "\n");
    let mut result = String::with_capacity(input.len());
    for line in input.split('\n') {
        // Collapse runs of horizontal whitespace within the line and trim leading space
        let mut prev_space = true; // start true to skip leading whitespace
        for ch in line.chars() {
            if ch == ' ' || ch == '\t' {
                if !prev_space {
                    result.push(' ');
                }
                prev_space = true;
            } else {
                result.push(ch);
                prev_space = false;
            }
        }
        // Trim trailing space we may have just added
        while result.ends_with(' ') {
            result.pop();
        }
        result.push('\n');
    }

    // Collapse 3+ consecutive newlines to 2
    let mut collapsed = String::with_capacity(result.len());
    let mut newline_count = 0u32;
    for ch in result.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                collapsed.push('\n');
            }
        } else {
            newline_count = 0;
            collapsed.push(ch);
        }
    }

    collapsed.trim().to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

    use super::{
        WebSearchInput, build_cli_input, build_search_output, build_search_request,
        parse_search_input, render_cli_output, search_with_client,
    };
    use crate::{
        error::AppError,
        provider::exa::{ExaClient, SearchResponse, SearchResult},
        test_support::{request_json, spawn_json_http_server},
    };

    fn fixed_now() -> OffsetDateTime {
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::April, 4).expect("valid date"),
            Time::MIDNIGHT,
        )
        .assume_utc()
    }

    #[test]
    fn published_within_days_accepts_boundaries() {
        let parsed_min = parse_search_input(&json!({
            "query": "rust",
            "published_within_days": 1
        }))
        .expect("parse min");
        assert_eq!(parsed_min.search_type, "auto");
        assert_eq!(parsed_min.num_results, 5);
        assert_eq!(parsed_min.published_within_days, Some(1));

        let parsed_max = parse_search_input(&json!({
            "query": "rust",
            "published_within_days": 365
        }))
        .expect("parse max");
        assert_eq!(parsed_max.published_within_days, Some(365));
    }

    #[test]
    fn search_type_defaults_and_validates() {
        let parsed_default = parse_search_input(&json!({
            "query": "rust"
        }))
        .expect("default type");
        assert_eq!(parsed_default.search_type, "auto");

        let parsed_neural = parse_search_input(&json!({
            "query": "rust",
            "type": "neural"
        }))
        .expect("neural type");
        assert_eq!(parsed_neural.search_type, "neural");

        let error = parse_search_input(&json!({
            "query": "rust",
            "type": "keyword"
        }))
        .expect_err("expected invalid type");
        match error {
            AppError::Tool(message) => assert!(message.contains("tool argument `type`")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn num_results_defaults_and_validates() {
        let parsed_default = parse_search_input(&json!({
            "query": "rust"
        }))
        .expect("default num_results");
        assert_eq!(parsed_default.num_results, 5);

        let parsed_custom = parse_search_input(&json!({
            "query": "rust",
            "num_results": 12
        }))
        .expect("custom num_results");
        assert_eq!(parsed_custom.num_results, 12);

        for invalid in [0_u64, 21_u64] {
            let error = parse_search_input(&json!({
                "query": "rust",
                "num_results": invalid
            }))
            .expect_err("expected invalid num_results");
            match error {
                AppError::Tool(message) => assert!(message.contains("tool argument `num_results`")),
                other => panic!("unexpected error: {other}"),
            }
        }
    }

    #[test]
    fn num_results_rejects_oversized_u64_values() {
        let error = parse_search_input(&json!({
            "query": "rust",
            "num_results": u64::MAX
        }))
        .expect_err("expected invalid num_results");
        match error {
            AppError::Tool(message) => assert!(message.contains("tool argument `num_results`")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn search_timeout_depends_on_type() {
        assert_eq!(
            super::search_timeout_for_type("auto"),
            Duration::from_secs(15)
        );
        assert_eq!(
            super::search_timeout_for_type("neural"),
            Duration::from_secs(10)
        );
        assert_eq!(
            super::search_timeout_for_type("deep"),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn published_within_days_rejects_out_of_range_values() {
        for invalid in [0_u64, 366_u64] {
            let error = parse_search_input(&json!({
                "query": "rust",
                "published_within_days": invalid
            }))
            .expect_err("expected validation error");
            match error {
                AppError::Tool(message) => {
                    assert!(message.contains("published_within_days"));
                }
                other => panic!("unexpected error: {other}"),
            }
        }
    }

    #[test]
    fn published_within_days_rejects_oversized_u64_values() {
        let error = parse_search_input(&json!({
            "query": "rust",
            "published_within_days": u64::MAX
        }))
        .expect_err("expected validation error");
        match error {
            AppError::Tool(message) => assert!(message.contains("published_within_days")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn domain_arrays_are_trimmed_deduped_and_overlap_rejected() {
        let parsed = parse_search_input(&json!({
            "query": "rust",
            "include_domains": [" Docs.rs ", "", "docs.rs", "crates.io"],
            "exclude_domains": ["example.com", " example.com "]
        }))
        .expect("parsed");

        assert_eq!(parsed.include_domains, vec!["docs.rs", "crates.io"]);
        assert_eq!(parsed.exclude_domains, vec!["example.com"]);

        let error = parse_search_input(&json!({
            "query": "rust",
            "include_domains": ["Docs.rs"],
            "exclude_domains": [" docs.rs "]
        }))
        .expect_err("expected overlap error");
        match error {
            AppError::Tool(message) => assert!(message.contains("overlap")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn cli_input_builder_defaults_and_normalizes_without_json_round_trip() {
        let input = build_cli_input(
            " rust async ",
            None,
            None,
            None,
            &[" Docs.rs ".to_string(), "docs.rs".to_string()],
            &["Example.com".to_string()],
        )
        .expect("cli input");

        assert_eq!(input.query, "rust async");
        assert_eq!(input.search_type, "auto");
        assert_eq!(input.num_results, 5);
        assert_eq!(input.published_within_days, None);
        assert_eq!(input.include_domains, vec!["docs.rs"]);
        assert_eq!(input.exclude_domains, vec!["example.com"]);
    }

    #[test]
    fn search_request_sets_start_published_date_and_sends_num_results() {
        let input = WebSearchInput {
            query: "rust".to_string(),
            search_type: "deep".to_string(),
            num_results: 7,
            published_within_days: Some(7),
            include_domains: vec!["docs.rs".to_string()],
            exclude_domains: Vec::new(),
        };

        let request = build_search_request(&input, fixed_now()).expect("request");
        assert_eq!(
            request.start_published_date.as_deref(),
            Some("2026-03-28T00:00:00Z")
        );
        assert_eq!(request.search_type, "deep");
        assert_eq!(request.num_results, 7);

        let request_json = serde_json::to_value(&request).expect("request json");
        assert_eq!(request_json["includeDomains"][0], "docs.rs");
        assert_eq!(request_json["type"], "deep");
        assert_eq!(request_json["numResults"], 7);
    }

    #[test]
    fn search_with_client_uses_fake_exa_server_and_returns_structured_output() {
        let (address, request, handle) = spawn_json_http_server(
            "200 OK",
            r#"{"results":[{"title":"Rust Docs","url":"https://docs.rs","publishedDate":"2026-04-01T00:00:00Z","score":0.9,"highlights":[" Rust docs "]}],"searchType":"neural"}"#,
        );
        let client = ExaClient::with_base_url(format!("http://{address}"), "test-key".to_string());
        let input = WebSearchInput {
            query: "rust".to_string(),
            search_type: "auto".to_string(),
            num_results: 5,
            published_within_days: Some(7),
            include_domains: vec!["docs.rs".to_string()],
            exclude_domains: vec!["example.com".to_string()],
        };

        let output = search_with_client(input, Duration::from_secs(2), fixed_now(), &client)
            .expect("search output");
        handle.join().expect("join server");

        assert_eq!(output.query, "rust");
        assert_eq!(output.search_type, "neural");
        assert_eq!(output.requested_type.as_deref(), Some("auto"));
        assert_eq!(output.num_results, 5);
        assert_eq!(output.result_count, 1);
        assert_eq!(output.results[0].title.as_deref(), Some("Rust Docs"));
        assert_eq!(output.results[0].url, "https://docs.rs");
        assert_eq!(output.results[0].highlights, vec!["Rust docs"]);

        let raw_request = request.lock().expect("request lock").clone();
        assert!(
            raw_request
                .to_ascii_lowercase()
                .contains("accept: application/json")
        );
        let request_json = request_json(&raw_request);
        assert_eq!(request_json["query"], "rust");
        assert_eq!(request_json["type"], "auto");
        assert_eq!(request_json["numResults"], 5);
        assert_eq!(request_json["includeDomains"][0], "docs.rs");
        assert_eq!(request_json["excludeDomains"][0], "example.com");
        assert_eq!(request_json["startPublishedDate"], "2026-03-28T00:00:00Z");
        assert_eq!(request_json["contents"]["highlights"], json!({}));
    }

    #[test]
    fn search_output_preserves_all_results_without_truncation() {
        let response = SearchResponse {
            search_type: None,
            results: (0..11)
                .map(|index| SearchResult {
                    title: Some(format!("Result {index}")),
                    url: format!("https://example.com/{index}"),
                    id: None,
                    score: Some(index as f64),
                    published_date: Some("2026-04-01T00:00:00Z".to_string()),
                    author: None,
                    highlights: Some(vec![format!("Highlight {index}")]),
                    highlight_scores: None,
                })
                .collect(),
        };

        let output = build_search_output("rust".to_string(), "auto".to_string(), 5, response);
        assert_eq!(output.result_count, 11);
        assert_eq!(output.results.len(), 11);
        assert_eq!(output.num_results, 5);
        assert_eq!(output.requested_type, None);

        let output_json = serde_json::to_value(&output).expect("output json");
        assert_eq!(output_json["results"].as_array().map(Vec::len), Some(11));
        assert_eq!(output_json["num_results"], 5);
    }

    #[test]
    fn search_output_omits_empty_optional_fields() {
        let response = SearchResponse {
            search_type: None,
            results: vec![SearchResult {
                title: Some(" ".to_string()),
                url: "https://example.com".to_string(),
                id: None,
                score: None,
                published_date: Some(" ".to_string()),
                author: None,
                highlights: Some(vec![" ".to_string()]),
                highlight_scores: None,
            }],
        };

        let output = build_search_output("rust".to_string(), "auto".to_string(), 5, response);
        let output_json = serde_json::to_value(&output).expect("output json");
        let first = output_json["results"][0]
            .as_object()
            .expect("result object");
        assert_eq!(output_json["type"], "auto");
        assert_eq!(
            first.get("url"),
            Some(&Value::String("https://example.com".to_string()))
        );
        assert!(first.get("title").is_none());
        assert!(first.get("published_date").is_none());
        assert!(first.get("score").is_none());
        assert!(first.get("highlights").is_none());
    }

    #[test]
    fn search_output_uses_effective_search_type_when_response_overrides_request() {
        let response = SearchResponse {
            search_type: Some("deep".to_string()),
            results: Vec::new(),
        };

        let output = build_search_output("rust".to_string(), "auto".to_string(), 5, response);
        assert_eq!(output.search_type, "deep");
        assert_eq!(output.requested_type.as_deref(), Some("auto"));

        let output_json = serde_json::to_value(&output).expect("output json");
        assert_eq!(output_json["type"], "deep");
        assert_eq!(output_json["requested_type"], "auto");
    }

    #[test]
    fn render_cli_output_includes_requested_type_only_when_effective_type_differs() {
        let with_override = render_cli_output(&super::WebSearchOutput {
            query: "rust".to_string(),
            search_type: "neural".to_string(),
            requested_type: Some("auto".to_string()),
            num_results: 5,
            results: Vec::new(),
            result_count: 0,
        });
        assert!(with_override.contains("Type:           neural"));
        assert!(with_override.contains("Requested type: auto"));

        let without_override = render_cli_output(&super::WebSearchOutput {
            query: "rust".to_string(),
            search_type: "auto".to_string(),
            requested_type: None,
            num_results: 5,
            results: Vec::new(),
            result_count: 0,
        });
        assert!(without_override.contains("Type:           auto"));
        assert!(!without_override.contains("Requested type:"));
    }

    #[test]
    fn normalize_whitespace_collapses_blank_lines_and_horizontal_runs() {
        use super::normalize_whitespace;

        // Collapse 3+ newlines to 2
        assert_eq!(normalize_whitespace("a\n\n\nb"), "a\n\nb");
        assert_eq!(normalize_whitespace("a\n\n\n\n\nb"), "a\n\nb");

        // Preserve single blank line (2 newlines)
        assert_eq!(normalize_whitespace("a\n\nb"), "a\n\nb");

        // Collapse horizontal whitespace runs
        assert_eq!(normalize_whitespace("a   b\t\tc"), "a b c");

        // Trim trailing whitespace per line
        assert_eq!(normalize_whitespace("a   \nb"), "a\nb");

        // \r\n → \n
        assert_eq!(normalize_whitespace("a\r\n\r\nb"), "a\n\nb");

        // Combined: messy scraped content
        assert_eq!(
            normalize_whitespace("  hello   world  \n\n\n\n  foo  \n\n  bar  "),
            "hello world\n\nfoo\n\nbar"
        );

        // Empty / whitespace-only
        assert_eq!(normalize_whitespace(""), "");
        assert_eq!(normalize_whitespace("  \n\n\n  "), "");
    }
}
