use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, require_string},
};

mod content;
mod html;
mod render;
mod sites;
mod transport;

#[cfg(test)]
mod live_canaries;
#[cfg(test)]
mod tests;

use transport::{StdDnsResolver, UreqTransport, fetch_with_clients};

const MAX_REDIRECTS: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_DOWNLOAD_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTENT_CHARS: usize = 256 * 1024;
const MIN_CONTENT_CHARS: usize = 200;
const MIN_LOW_SIGNAL_CHARS: usize = 200;
const MIN_PRIMARY_ROOT_CHARS: usize = 100;
const MAX_ADDRESS_ATTEMPTS: usize = 4;

const NOISY_TAGS: &[&str] = &[
    "script", "style", "noscript", "svg", "canvas", "iframe", "nav", "footer", "form",
];
const NOISY_TOKEN_SUBSTRINGS: &[&str] = &[
    "nav",
    "menu",
    "footer",
    "header",
    "sidebar",
    "cookie",
    "consent",
    "modal",
    "popup",
    "share",
    "social",
    "breadcrumb",
    "advert",
    "promo",
    "copyright",
    "newsletter",
    "subscribe",
    "login",
    "register",
    "banner",
];

pub fn web_fetch_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "web_fetch",
        description: "Fetch a single URL and return cleaned text plus structured fetch metadata. Use this after web_search when you need live verification or deeper reading from a specific page. Cite the exact URL you fetched in the final answer.",
        parameters: json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" }
            },
            "required": ["url"]
        }),
    }
}

pub fn run_web_fetch(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let url = require_string(arguments, "url")?;
    let timeout = context.remaining_budget()?.min(REQUEST_TIMEOUT);
    let result = fetch_url_with_timeout(&url, timeout);
    serde_json::to_value(result).map_err(AppError::from)
}

pub fn fetch_url(url: &str) -> FetchResult {
    fetch_url_with_timeout(url, REQUEST_TIMEOUT)
}

pub fn fetch_url_with_timeout(url: &str, timeout: Duration) -> FetchResult {
    fetch_with_clients(url, timeout, &StdDnsResolver, &UreqTransport)
}

pub fn render_cli_output(result: &FetchResult) -> String {
    let final_url = result.final_url.as_deref().unwrap_or("-");
    let status = result
        .status
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let content_type = result.content_type.as_deref().unwrap_or("-");
    let warnings = if result.warnings.is_empty() {
        "none".to_string()
    } else {
        result
            .warnings
            .iter()
            .map(Warning::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let error = result.error.as_deref().unwrap_or("none");
    let content = if result.content.trim().is_empty() {
        "(no content)"
    } else {
        result.content.trim_end()
    };
    format!(
        "URL:             {}\nFinal URL:       {}\nStatus:          {}\nContent-Type:    {}\nExtraction:      {}\nWarnings:        {}\nTruncated:       {}\nBytes read:      {}\nError:           {}\n\n---\n\n{}\n",
        result.requested_url,
        final_url,
        status,
        content_type,
        result.extraction_kind.as_str(),
        warnings,
        result.truncated,
        result.bytes_read,
        error,
        content
    )
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FetchResult {
    pub ok: bool,
    pub requested_url: String,
    pub final_url: Option<String>,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub content: String,
    pub extraction_kind: ExtractionKind,
    pub warnings: Vec<Warning>,
    pub error: Option<String>,
    pub truncated: bool,
    pub bytes_read: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum ExtractionKind {
    Json,
    Text,
    HtmlPrimary,
    HtmlFallback,
    BinarySummary,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Warning {
    LowContentYield,
    LowSignalExtraction,
    PossibleJsRenderedPage,
    ContentTruncated,
}

#[derive(Debug, Default, Clone)]
pub(super) struct FailureContext {
    pub(super) final_url: Option<String>,
    pub(super) status: Option<u16>,
    pub(super) content_type: Option<String>,
    pub(super) content: String,
    pub(super) warnings: Vec<Warning>,
    pub(super) truncated: bool,
    pub(super) bytes_read: u64,
}

impl ExtractionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "Json",
            Self::Text => "Text",
            Self::HtmlPrimary => "HtmlPrimary",
            Self::HtmlFallback => "HtmlFallback",
            Self::BinarySummary => "BinarySummary",
            Self::Error => "Error",
        }
    }
}

impl Warning {
    fn as_str(&self) -> &'static str {
        match self {
            Self::LowContentYield => "LowContentYield",
            Self::LowSignalExtraction => "LowSignalExtraction",
            Self::PossibleJsRenderedPage => "PossibleJsRenderedPage",
            Self::ContentTruncated => "ContentTruncated",
        }
    }
}

impl FetchResult {
    fn failure(requested_url: &str, error: &str, context: FailureContext) -> Self {
        Self {
            ok: false,
            requested_url: requested_url.to_string(),
            final_url: context.final_url,
            status: context.status,
            content_type: context.content_type,
            content: context.content,
            extraction_kind: ExtractionKind::Error,
            warnings: context.warnings,
            error: Some(error.to_string()),
            truncated: context.truncated,
            bytes_read: context.bytes_read,
        }
    }
}
