use std::{
    error::Error as _,
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::OnceLock,
    time::Duration,
};

use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use serde_json::{Value, json};
use ureq::OrAnyStatus;
use url::{Host, Url};

use crate::{
    error::AppError,
    tools::{ToolContext, require_string},
};

const MAX_REDIRECTS: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_DOWNLOAD_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTENT_CHARS: usize = 16_000;
const MIN_CONTENT_CHARS: usize = 100;

const CAPTURE_TAGS: &[&str] = &[
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "p",
    "li",
    "td",
    "th",
    "blockquote",
    "pre",
    "code",
    "label",
    "button",
];
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
];

static SELECTORS: OnceLock<Result<Selectors, String>> = OnceLock::new();

pub fn web_fetch_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "web_fetch",
        description: "Fetch a single URL and return cleaned text plus structured fetch metadata. Use when you need page content, docs, help articles, policies, changelogs, or JSON from the web.",
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
    PossibleJsRenderedPage,
    ContentTruncated,
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
            Self::PossibleJsRenderedPage => "PossibleJsRenderedPage",
            Self::ContentTruncated => "ContentTruncated",
        }
    }
}

impl FetchResult {
    fn failure(
        requested_url: &str,
        final_url: Option<String>,
        status: Option<u16>,
        content_type: Option<String>,
        content: String,
        warnings: Vec<Warning>,
        error: &str,
        truncated: bool,
        bytes_read: u64,
    ) -> Self {
        Self {
            ok: false,
            requested_url: requested_url.to_string(),
            final_url,
            status,
            content_type,
            content,
            extraction_kind: ExtractionKind::Error,
            warnings,
            error: Some(error.to_string()),
            truncated,
            bytes_read,
        }
    }
}

#[derive(Debug)]
struct Selectors {
    title: Selector,
    h1: Selector,
    meta_description: Selector,
    main: Selector,
    article: Selector,
    role_main: Selector,
    id_content: Selector,
    id_main: Selector,
    class_content: Selector,
    body: Selector,
    blocks: Selector,
}

#[derive(Debug, Clone)]
struct ResolvedTarget {
    url: Url,
    addresses: Vec<SocketAddr>,
}

#[derive(Debug, Clone)]
struct TransportResponse {
    url: Url,
    status: u16,
    content_type: Option<String>,
    location: Option<String>,
    content_length: Option<u64>,
    body: Vec<u8>,
    bytes_read: u64,
    body_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportFailureKind {
    Dns,
    Connect,
    Timeout,
    Redirect,
    Decode,
}

#[derive(Debug, Clone)]
struct TransportFailure {
    kind: TransportFailureKind,
    url: Option<Url>,
}

trait DnsResolver {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>>;
}

trait HttpTransport {
    fn get(
        &self,
        target: &ResolvedTarget,
        timeout: Duration,
    ) -> Result<TransportResponse, TransportFailure>;
}

struct StdDnsResolver;
struct UreqTransport;

impl DnsResolver for StdDnsResolver {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        (host, port).to_socket_addrs().map(|iter| iter.collect())
    }
}

impl HttpTransport for UreqTransport {
    fn get(
        &self,
        target: &ResolvedTarget,
        timeout: Duration,
    ) -> Result<TransportResponse, TransportFailure> {
        let connect_timeout = CONNECT_TIMEOUT.min(timeout);
        let addresses = target.addresses.clone();
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout_connect(connect_timeout)
            .timeout(timeout)
            .timeout_read(timeout)
            .timeout_write(timeout)
            .resolver(move |_netloc: &str| Ok(addresses.clone()))
            .build();
        let response = agent
            .get(target.url.as_str())
            .set(
                "User-Agent",
                concat!("headless/", env!("CARGO_PKG_VERSION")),
            )
            .set(
                "Accept",
                "text/html,application/xhtml+xml,application/json,text/plain;q=0.9,*/*;q=0.8",
            )
            .set("Accept-Language", "en-US,en;q=0.9")
            .timeout(timeout)
            .call()
            .or_any_status()
            .map_err(|error| map_transport_error(error, Some(target.url.clone())))?;

        let status = response.status();
        let url = response
            .get_url()
            .parse::<Url>()
            .map_err(|_| TransportFailure {
                kind: TransportFailureKind::Decode,
                url: Some(target.url.clone()),
            })?;
        let content_type = response.header("content-type").map(ToOwned::to_owned);
        let location = response.header("location").map(ToOwned::to_owned);
        let content_length = response
            .header("content-length")
            .and_then(|value: &str| value.parse::<u64>().ok());
        let mut reader = response.into_reader().take((MAX_DOWNLOAD_BYTES as u64) + 1);
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|error| map_read_error(error, Some(url.clone())))?;
        let body_truncated = body.len() > MAX_DOWNLOAD_BYTES;
        if body_truncated {
            body.truncate(MAX_DOWNLOAD_BYTES);
        }
        Ok(TransportResponse {
            url,
            status,
            content_type,
            location,
            content_length,
            bytes_read: body.len() as u64,
            body,
            body_truncated,
        })
    }
}

fn fetch_with_clients(
    requested_url: &str,
    timeout: Duration,
    resolver: &dyn DnsResolver,
    transport: &dyn HttpTransport,
) -> FetchResult {
    let mut current = match resolve_target(requested_url, resolver) {
        Ok(target) => target,
        Err(result) => return result,
    };

    let mut redirects_followed = 0usize;
    loop {
        let response = match transport.get(&current, timeout) {
            Ok(response) => response,
            Err(error) => {
                let final_url = error
                    .url
                    .map(|value| value.to_string())
                    .or_else(|| Some(current.url.to_string()));
                return FetchResult::failure(
                    requested_url,
                    final_url,
                    None,
                    None,
                    String::new(),
                    Vec::new(),
                    transport_error_code(error.kind),
                    false,
                    0,
                );
            }
        };

        if is_redirect_status(response.status) {
            if redirects_followed >= MAX_REDIRECTS {
                return FetchResult::failure(
                    requested_url,
                    Some(response.url.to_string()),
                    Some(response.status),
                    normalize_content_type(response.content_type.as_deref()),
                    String::new(),
                    Vec::new(),
                    "redirect_error",
                    false,
                    response.bytes_read,
                );
            }
            let Some(location) = response.location.as_deref() else {
                return FetchResult::failure(
                    requested_url,
                    Some(response.url.to_string()),
                    Some(response.status),
                    normalize_content_type(response.content_type.as_deref()),
                    String::new(),
                    Vec::new(),
                    "redirect_error",
                    false,
                    response.bytes_read,
                );
            };
            let next_url = match response.url.join(location) {
                Ok(url) => url,
                Err(_) => {
                    return FetchResult::failure(
                        requested_url,
                        Some(response.url.to_string()),
                        Some(response.status),
                        normalize_content_type(response.content_type.as_deref()),
                        String::new(),
                        Vec::new(),
                        "redirect_error",
                        false,
                        response.bytes_read,
                    );
                }
            };
            current = match resolve_target_url(
                next_url,
                resolver,
                requested_url,
                Some(response.url.to_string()),
            ) {
                Ok(target) => target,
                Err(result) => return result,
            };
            redirects_followed += 1;
            continue;
        }

        return build_fetch_result(requested_url, response);
    }
}

fn build_fetch_result(requested_url: &str, response: TransportResponse) -> FetchResult {
    let extraction = extract_content(
        response.content_type.as_deref(),
        &response.body,
        response.content_length,
        response.body_truncated,
    );
    let mut warnings = extraction.warnings;
    let truncated = extraction.truncated;
    if truncated {
        push_warning(&mut warnings, Warning::ContentTruncated);
    }

    let content_type = extraction.content_type;
    let mut result = FetchResult {
        ok: response.status >= 200 && response.status < 300 && extraction.error.is_none(),
        requested_url: requested_url.to_string(),
        final_url: Some(response.url.to_string()),
        status: Some(response.status),
        content_type,
        content: extraction.content,
        extraction_kind: extraction.kind,
        warnings,
        error: extraction.error.map(ToOwned::to_owned),
        truncated,
        bytes_read: response.bytes_read,
    };

    if response.status >= 400 {
        result.ok = false;
        result.error = Some(format!("http_{}", response.status));
        result.extraction_kind = ExtractionKind::Error;
    } else if result.error.is_some() {
        result.ok = false;
        result.extraction_kind = ExtractionKind::Error;
    }

    result
}

fn resolve_target(input: &str, resolver: &dyn DnsResolver) -> Result<ResolvedTarget, FetchResult> {
    let url = match Url::parse(input) {
        Ok(url) => url,
        Err(_) => {
            return Err(FetchResult::failure(
                input,
                None,
                None,
                None,
                String::new(),
                Vec::new(),
                "invalid_url",
                false,
                0,
            ));
        }
    };
    resolve_target_url(url, resolver, input, None)
}

fn resolve_target_url(
    url: Url,
    resolver: &dyn DnsResolver,
    requested_url: &str,
    final_url: Option<String>,
) -> Result<ResolvedTarget, FetchResult> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "unsupported_scheme",
            false,
            0,
        ));
    }

    let Some(host) = url.host() else {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "invalid_url",
            false,
            0,
        ));
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let host_string = host.to_string();

    if matches!(host, Host::Domain(_)) && is_blocked_hostname(&host_string) {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "blocked_address",
            false,
            0,
        ));
    }

    let addresses = match host {
        Host::Ipv4(addr) => vec![SocketAddr::new(IpAddr::V4(addr), port)],
        Host::Ipv6(addr) => vec![SocketAddr::new(IpAddr::V6(addr), port)],
        Host::Domain(_) => match resolver.resolve(&host_string, port) {
            Ok(addresses) if !addresses.is_empty() => addresses,
            _ => {
                return Err(FetchResult::failure(
                    requested_url,
                    final_url,
                    None,
                    None,
                    String::new(),
                    Vec::new(),
                    "dns_error",
                    false,
                    0,
                ));
            }
        },
    };

    if addresses.iter().any(|value| is_blocked_ip(value.ip())) {
        return Err(FetchResult::failure(
            requested_url,
            final_url,
            None,
            None,
            String::new(),
            Vec::new(),
            "blocked_address",
            false,
            0,
        ));
    }

    Ok(ResolvedTarget { url, addresses })
}

fn extract_content(
    content_type_header: Option<&str>,
    body: &[u8],
    content_length: Option<u64>,
    body_truncated: bool,
) -> ExtractedContent {
    let kind = sniff_content_kind(content_type_header, body);
    match kind {
        SniffedKind::Json => extract_json(body, body_truncated),
        SniffedKind::Html => extract_html(content_type_header, body, body_truncated),
        SniffedKind::Text => extract_text(content_type_header, body, body_truncated),
        SniffedKind::Binary => {
            extract_binary(content_type_header, content_length, body, body_truncated)
        }
    }
}

#[derive(Debug)]
struct ExtractedContent {
    kind: ExtractionKind,
    content_type: Option<String>,
    content: String,
    warnings: Vec<Warning>,
    truncated: bool,
    error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SniffedKind {
    Json,
    Html,
    Text,
    Binary,
}

fn extract_json(body: &[u8], body_truncated: bool) -> ExtractedContent {
    let content_type = Some("application/json".to_string());
    if !body_truncated {
        if let Ok(value) = serde_json::from_slice::<Value>(body) {
            let rendered =
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            let (content, content_truncated) = truncate_chars(&rendered, MAX_CONTENT_CHARS);
            return ExtractedContent {
                kind: ExtractionKind::Json,
                content_type,
                content,
                warnings: warning_list(content_truncated),
                truncated: content_truncated,
                error: None,
            };
        }
    }

    let rendered = String::from_utf8_lossy(body).to_string();
    let (content, content_truncated) = truncate_chars(&rendered, MAX_CONTENT_CHARS);
    let mut warnings = Vec::new();
    if body_truncated || content_truncated {
        push_warning(&mut warnings, Warning::ContentTruncated);
    }
    ExtractedContent {
        kind: ExtractionKind::Json,
        content_type,
        content,
        warnings,
        truncated: body_truncated || content_truncated,
        error: (!body_truncated).then_some("decode_error"),
    }
}

fn extract_text(
    content_type_header: Option<&str>,
    body: &[u8],
    body_truncated: bool,
) -> ExtractedContent {
    let rendered = normalize_text_body(&String::from_utf8_lossy(body));
    let (content, content_truncated) = truncate_chars(&rendered, MAX_CONTENT_CHARS);
    let mut warnings = Vec::new();
    if body_truncated || content_truncated {
        push_warning(&mut warnings, Warning::ContentTruncated);
    }
    ExtractedContent {
        kind: ExtractionKind::Text,
        content_type: normalize_content_type(content_type_header)
            .or_else(|| Some("text/plain".to_string())),
        content,
        warnings,
        truncated: body_truncated || content_truncated,
        error: None,
    }
}

fn extract_binary(
    content_type_header: Option<&str>,
    content_length: Option<u64>,
    body: &[u8],
    body_truncated: bool,
) -> ExtractedContent {
    let content_type = normalize_content_type(content_type_header)
        .or_else(|| Some("application/octet-stream".to_string()));
    let bytes = content_length.unwrap_or(body.len() as u64);
    let content = format!(
        "[BINARY CONTENT]\ncontent_type: {}\nbytes: {}",
        content_type
            .as_deref()
            .unwrap_or("application/octet-stream"),
        bytes
    );
    let (content, content_truncated) = truncate_chars(&content, MAX_CONTENT_CHARS);
    let mut warnings = Vec::new();
    if body_truncated || content_truncated {
        push_warning(&mut warnings, Warning::ContentTruncated);
    }
    ExtractedContent {
        kind: ExtractionKind::BinarySummary,
        content_type,
        content,
        warnings,
        truncated: body_truncated || content_truncated,
        error: None,
    }
}

fn extract_html(
    content_type_header: Option<&str>,
    body: &[u8],
    body_truncated: bool,
) -> ExtractedContent {
    let selectors = match selectors() {
        Ok(selectors) => selectors,
        Err(_) => {
            return ExtractedContent {
                kind: ExtractionKind::Error,
                content_type: normalize_content_type(content_type_header)
                    .or_else(|| Some("text/html".to_string())),
                content: String::new(),
                warnings: warning_list(body_truncated),
                truncated: body_truncated,
                error: Some("decode_error"),
            };
        }
    };

    let html = String::from_utf8_lossy(body).to_string();
    let document = Html::parse_document(&html);
    let root = select_root(&document, selectors);
    let metadata = HtmlMetadata {
        title: document
            .select(&selectors.title)
            .next()
            .map(|value| normalize_inline(&value.text().collect::<String>()))
            .filter(|value| !value.is_empty()),
        h1: document
            .select(&selectors.h1)
            .next()
            .map(|value| normalize_inline(&value.text().collect::<String>()))
            .filter(|value| !value.is_empty()),
        description: document
            .select(&selectors.meta_description)
            .next()
            .and_then(|value| value.value().attr("content"))
            .map(normalize_inline)
            .filter(|value| !value.is_empty()),
    };

    let mut blocks = Vec::new();
    for element in root.element.select(&selectors.blocks) {
        if has_capture_ancestor(&element) || has_noisy_ancestor(&element) {
            continue;
        }
        let text = collect_block_text(&element);
        if !text.is_empty() {
            blocks.push(text);
        }
    }

    let body_text = blocks.join("\n\n");
    let mut content = String::new();
    append_metadata_line(&mut content, "Title", metadata.title.as_deref());
    append_metadata_line(&mut content, "H1", metadata.h1.as_deref());
    append_metadata_line(&mut content, "Description", metadata.description.as_deref());
    if !content.is_empty() && !body_text.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(&body_text);
    let content = normalize_text_body(&content);
    let (content, content_truncated) = truncate_chars(&content, MAX_CONTENT_CHARS);

    let mut warnings = Vec::new();
    if body_text.chars().count() < MIN_CONTENT_CHARS {
        push_warning(&mut warnings, Warning::LowContentYield);
        if metadata.title.is_some() || html_has_shell_markers(&html) {
            push_warning(&mut warnings, Warning::PossibleJsRenderedPage);
        }
    }
    if body_truncated || content_truncated {
        push_warning(&mut warnings, Warning::ContentTruncated);
    }

    ExtractedContent {
        kind: root.kind,
        content_type: normalize_content_type(content_type_header)
            .or_else(|| Some("text/html".to_string())),
        content,
        warnings,
        truncated: body_truncated || content_truncated,
        error: None,
    }
}

#[derive(Debug)]
struct HtmlMetadata {
    title: Option<String>,
    h1: Option<String>,
    description: Option<String>,
}

#[derive(Clone)]
struct RootSelection<'a> {
    element: ElementRef<'a>,
    kind: ExtractionKind,
}

fn select_root<'a>(document: &'a Html, selectors: &'a Selectors) -> RootSelection<'a> {
    if let Some(element) = document.select(&selectors.main).next() {
        return RootSelection {
            element,
            kind: ExtractionKind::HtmlPrimary,
        };
    }
    if let Some(element) = document.select(&selectors.article).next() {
        return RootSelection {
            element,
            kind: ExtractionKind::HtmlPrimary,
        };
    }
    if let Some(element) = document.select(&selectors.role_main).next() {
        return RootSelection {
            element,
            kind: ExtractionKind::HtmlPrimary,
        };
    }
    if let Some(element) = document.select(&selectors.id_content).next() {
        return RootSelection {
            element,
            kind: ExtractionKind::HtmlPrimary,
        };
    }
    if let Some(element) = document.select(&selectors.id_main).next() {
        return RootSelection {
            element,
            kind: ExtractionKind::HtmlPrimary,
        };
    }
    if let Some(element) = document.select(&selectors.class_content).next() {
        return RootSelection {
            element,
            kind: ExtractionKind::HtmlPrimary,
        };
    }
    RootSelection {
        element: document
            .select(&selectors.body)
            .next()
            .or_else(|| document.root_element().select(&selectors.body).next())
            .unwrap_or_else(|| document.root_element()),
        kind: ExtractionKind::HtmlFallback,
    }
}

fn selectors() -> Result<&'static Selectors, String> {
    SELECTORS
        .get_or_init(build_selectors)
        .as_ref()
        .map_err(Clone::clone)
}

fn build_selectors() -> Result<Selectors, String> {
    Ok(Selectors {
        title: parse_selector("title")?,
        h1: parse_selector("h1")?,
        meta_description: parse_selector("meta[name='description']")?,
        main: parse_selector("main")?,
        article: parse_selector("article")?,
        role_main: parse_selector("[role='main']")?,
        id_content: parse_selector("#content")?,
        id_main: parse_selector("#main")?,
        class_content: parse_selector(".content")?,
        body: parse_selector("body")?,
        blocks: parse_selector(
            "h1, h2, h3, h4, h5, h6, p, li, td, th, blockquote, pre, code, label, button",
        )?,
    })
}

fn parse_selector(input: &str) -> Result<Selector, String> {
    Selector::parse(input).map_err(|_| format!("failed to parse selector `{input}`"))
}

fn append_metadata_line(buffer: &mut String, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        if value.is_empty() {
            return;
        }
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(label);
        buffer.push_str(": ");
        buffer.push_str(value);
    }
}

fn collect_block_text(element: &ElementRef<'_>) -> String {
    let tag = element.value().name();
    if tag == "pre" {
        return normalize_preformatted(&element.text().collect::<String>());
    }
    normalize_inline(&element.text().collect::<Vec<_>>().join(" "))
}

fn has_capture_ancestor(element: &ElementRef<'_>) -> bool {
    for ancestor in element.ancestors().skip(1).filter_map(ElementRef::wrap) {
        if CAPTURE_TAGS.contains(&ancestor.value().name()) {
            return true;
        }
    }
    false
}

fn has_noisy_ancestor(element: &ElementRef<'_>) -> bool {
    element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|ancestor| is_noisy_element(&ancestor))
}

fn is_noisy_element(element: &ElementRef<'_>) -> bool {
    let name = element.value().name();
    if NOISY_TAGS.contains(&name) {
        return true;
    }

    if element.value().attr("hidden").is_some() {
        return true;
    }

    if let Some(value) = element.value().attr("aria-hidden") {
        if value.eq_ignore_ascii_case("true") {
            return true;
        }
    }

    if let Some(value) = element.value().attr("role") {
        let value = value.to_ascii_lowercase();
        if matches!(value.as_str(), "navigation" | "banner" | "complementary") {
            return true;
        }
    }

    if let Some(value) = element.value().attr("style") {
        let value = value.to_ascii_lowercase();
        if value.contains("display:none") || value.contains("visibility:hidden") {
            return true;
        }
    }

    for attr in ["class", "id"] {
        if let Some(value) = element.value().attr(attr) {
            let lower = value.to_ascii_lowercase();
            if NOISY_TOKEN_SUBSTRINGS
                .iter()
                .any(|token| lower.contains(token))
            {
                return true;
            }
        }
    }

    false
}

fn sniff_content_kind(content_type_header: Option<&str>, body: &[u8]) -> SniffedKind {
    if let Some(content_type) = normalize_content_type(content_type_header) {
        if content_type == "application/json" || content_type.ends_with("+json") {
            return SniffedKind::Json;
        }
        if matches!(content_type.as_str(), "text/html" | "application/xhtml+xml") {
            return SniffedKind::Html;
        }
        if content_type.starts_with("text/") {
            return SniffedKind::Text;
        }
        if !is_generic_content_type(&content_type) {
            return SniffedKind::Binary;
        }
    }

    if looks_like_html(body) {
        return SniffedKind::Html;
    }
    if looks_like_json(body) {
        return SniffedKind::Json;
    }
    if looks_like_text(body) {
        return SniffedKind::Text;
    }
    SniffedKind::Binary
}

fn normalize_content_type(header: Option<&str>) -> Option<String> {
    header.map(|value| {
        value
            .split(';')
            .next()
            .unwrap_or(value)
            .trim()
            .to_ascii_lowercase()
    })
}

fn is_generic_content_type(content_type: &str) -> bool {
    matches!(
        content_type,
        "" | "application/octet-stream" | "binary/octet-stream"
    )
}

fn looks_like_html(body: &[u8]) -> bool {
    let sample = String::from_utf8_lossy(&body[..body.len().min(2048)]).to_ascii_lowercase();
    let trimmed = sample.trim_start();
    trimmed.starts_with("<!doctype html")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<body")
        || trimmed.starts_with("<main")
        || trimmed.starts_with("<article")
        || sample.contains("<html")
        || sample.contains("<body")
        || sample.contains("<main")
        || sample.contains("<article")
}

fn looks_like_json(body: &[u8]) -> bool {
    let sample = String::from_utf8_lossy(&body[..body.len().min(8192)]);
    let trimmed = sample.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    serde_json::from_slice::<Value>(body).is_ok()
}

fn looks_like_text(body: &[u8]) -> bool {
    if body.is_empty() {
        return true;
    }
    let printable = body
        .iter()
        .filter(|byte| matches!(byte, 0x09 | 0x0A | 0x0D | 0x20..=0x7E))
        .count();
    printable * 100 / body.len() >= 85
}

fn normalize_inline(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_preformatted(input: &str) -> String {
    let lines = input
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    lines.join("\n")
}

fn normalize_text_body(input: &str) -> String {
    let mut lines = Vec::new();
    let mut previous_blank = false;
    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            if !previous_blank && !lines.is_empty() {
                lines.push(String::new());
            }
            previous_blank = true;
            continue;
        }
        lines.push(normalize_inline(line));
        previous_blank = false;
    }
    lines.join("\n")
}

fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
    let mut count = 0usize;
    let mut output = String::new();
    for ch in input.chars() {
        if count == limit {
            return (output, true);
        }
        output.push(ch);
        count += 1;
    }
    (output, false)
}

fn warning_list(content_truncated: bool) -> Vec<Warning> {
    if content_truncated {
        vec![Warning::ContentTruncated]
    } else {
        Vec::new()
    }
}

fn push_warning(warnings: &mut Vec<Warning>, warning: Warning) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

fn html_has_shell_markers(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("__next")
        || lower.contains("id=\"root\"")
        || lower.contains("id='root'")
        || lower.contains("id=\"app\"")
        || lower.contains("id='app'")
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn map_transport_error(error: ureq::Transport, fallback_url: Option<Url>) -> TransportFailure {
    use std::io::ErrorKind;
    use ureq::ErrorKind as UreqErrorKind;

    let kind = match error.kind() {
        UreqErrorKind::Dns => TransportFailureKind::Dns,
        UreqErrorKind::TooManyRedirects => TransportFailureKind::Redirect,
        UreqErrorKind::ConnectionFailed | UreqErrorKind::ProxyConnect => {
            TransportFailureKind::Connect
        }
        UreqErrorKind::InvalidUrl | UreqErrorKind::UnknownScheme => TransportFailureKind::Decode,
        UreqErrorKind::Io => match error
            .source()
            .and_then(|source: &(dyn std::error::Error + 'static)| {
                source.downcast_ref::<std::io::Error>()
            })
            .map(std::io::Error::kind)
        {
            Some(ErrorKind::TimedOut) | Some(ErrorKind::WouldBlock) => {
                TransportFailureKind::Timeout
            }
            _ => TransportFailureKind::Connect,
        },
        _ => TransportFailureKind::Decode,
    };
    TransportFailure {
        kind,
        url: error.url().cloned().or(fallback_url),
    }
}

fn map_read_error(error: std::io::Error, url: Option<Url>) -> TransportFailure {
    use std::io::ErrorKind;
    let kind = match error.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => TransportFailureKind::Timeout,
        _ => TransportFailureKind::Decode,
    };
    TransportFailure { kind, url }
}

fn transport_error_code(kind: TransportFailureKind) -> &'static str {
    match kind {
        TransportFailureKind::Dns => "dns_error",
        TransportFailureKind::Connect => "connect_error",
        TransportFailureKind::Timeout => "timeout",
        TransportFailureKind::Redirect => "redirect_error",
        TransportFailureKind::Decode => "decode_error",
    }
}

fn is_blocked_hostname(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "localhost"
        || lower == "localhost.localdomain"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    matches!(octets, [0, ..])
        || octets[0] == 10
        || octets[0] == 127
        || (octets[0] == 169 && octets[1] == 254)
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        || octets[0] >= 224
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segments = ip.segments();
    (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, io, sync::Arc};

    #[derive(Default)]
    struct FakeResolver {
        values: HashMap<(String, u16), io::Result<Vec<SocketAddr>>>,
    }

    impl FakeResolver {
        fn with_mapping(mut self, host: &str, port: u16, addresses: Vec<SocketAddr>) -> Self {
            self.values.insert((host.to_string(), port), Ok(addresses));
            self
        }
    }

    impl DnsResolver for FakeResolver {
        fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
            match self.values.get(&(host.to_string(), port)) {
                Some(Ok(addresses)) => Ok(addresses.clone()),
                Some(Err(error)) => Err(io::Error::new(error.kind(), error.to_string())),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "missing fake dns entry",
                )),
            }
        }
    }

    #[derive(Default, Clone)]
    struct FakeTransport {
        responses: Arc<HashMap<String, Result<TransportResponse, TransportFailureKind>>>,
    }

    impl FakeTransport {
        fn new(map: HashMap<String, Result<TransportResponse, TransportFailureKind>>) -> Self {
            Self {
                responses: Arc::new(map),
            }
        }
    }

    impl HttpTransport for FakeTransport {
        fn get(
            &self,
            target: &ResolvedTarget,
            _timeout: Duration,
        ) -> Result<TransportResponse, TransportFailure> {
            match self.responses.get(target.url.as_str()) {
                Some(Ok(response)) => Ok(response.clone()),
                Some(Err(kind)) => Err(TransportFailure {
                    kind: *kind,
                    url: Some(target.url.clone()),
                }),
                None => Err(TransportFailure {
                    kind: TransportFailureKind::Connect,
                    url: Some(target.url.clone()),
                }),
            }
        }
    }

    fn socket(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), port)
    }

    fn response(
        url: &str,
        status: u16,
        content_type: Option<&str>,
        body: &[u8],
    ) -> TransportResponse {
        TransportResponse {
            url: Url::parse(url).expect("url"),
            status,
            content_type: content_type.map(ToOwned::to_owned),
            location: None,
            content_length: Some(body.len() as u64),
            body: body.to_vec(),
            bytes_read: body.len() as u64,
            body_truncated: false,
        }
    }

    #[test]
    fn blocks_loopback_hosts() {
        let resolver = FakeResolver::default();
        let transport = FakeTransport::default();
        let result = fetch_with_clients(
            "http://localhost/path",
            REQUEST_TIMEOUT,
            &resolver,
            &transport,
        );
        assert_eq!(result.error.as_deref(), Some("blocked_address"));
        assert!(!result.ok);
    }

    #[test]
    fn follows_redirects_and_preserves_last_url() {
        let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
        let mut responses = HashMap::new();
        responses.insert(
            "http://example.test/start".to_string(),
            Ok(TransportResponse {
                location: Some("/docs".to_string()),
                ..response("http://example.test/start", 302, Some("text/plain"), b"")
            }),
        );
        responses.insert(
            "http://example.test/docs".to_string(),
            Ok(response(
                "http://example.test/docs",
                200,
                Some("text/html"),
                br#"<html><body><main><h1>Docs</h1><p>Hello world.</p></main></body></html>"#,
            )),
        );
        let transport = FakeTransport::new(responses);
        let result = fetch_with_clients(
            "http://example.test/start",
            REQUEST_TIMEOUT,
            &resolver,
            &transport,
        );
        assert!(result.ok);
        assert_eq!(
            result.final_url.as_deref(),
            Some("http://example.test/docs")
        );
        assert_eq!(result.extraction_kind, ExtractionKind::HtmlPrimary);
        assert!(result.content.contains("Hello world."));
    }

    #[test]
    fn html_extraction_strips_obvious_chrome() {
        let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
        let body = br#"
            <html>
              <head>
                <title>Pricing</title>
                <meta name="description" content="Simple plans" />
              </head>
              <body>
                <nav>Top nav</nav>
                <main>
                  <h1>Plans</h1>
                  <p>Starter plan</p>
                  <div class="share-tools">tweet this</div>
                  <pre>cargo install headless</pre>
                </main>
              </body>
            </html>
        "#;
        let transport = FakeTransport::new(HashMap::from([(
            "http://example.test/pricing".to_string(),
            Ok(response(
                "http://example.test/pricing",
                200,
                Some("text/html"),
                body,
            )),
        )]));
        let result = fetch_with_clients(
            "http://example.test/pricing",
            REQUEST_TIMEOUT,
            &resolver,
            &transport,
        );
        assert!(result.ok);
        assert!(result.content.contains("Title: Pricing"));
        assert!(result.content.contains("Starter plan"));
        assert!(result.content.contains("cargo install headless"));
        assert!(!result.content.contains("Top nav"));
        assert!(!result.content.contains("tweet this"));
    }

    #[test]
    fn json_sniffing_works_for_generic_content_type() {
        let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
        let transport = FakeTransport::new(HashMap::from([(
            "http://example.test/data".to_string(),
            Ok(response(
                "http://example.test/data",
                200,
                Some("application/octet-stream"),
                br#"{"hello":"world"}"#,
            )),
        )]));
        let result = fetch_with_clients(
            "http://example.test/data",
            REQUEST_TIMEOUT,
            &resolver,
            &transport,
        );
        assert!(result.ok);
        assert_eq!(result.extraction_kind, ExtractionKind::Json);
        assert_eq!(result.content_type.as_deref(), Some("application/json"));
        assert!(result.content.contains("\"hello\": \"world\""));
    }

    #[test]
    fn binary_summary_uses_content_length_when_present() {
        let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
        let transport = FakeTransport::new(HashMap::from([(
            "http://example.test/file.pdf".to_string(),
            Ok(TransportResponse {
                content_length: Some(182_344),
                ..response(
                    "http://example.test/file.pdf",
                    200,
                    Some("application/pdf"),
                    b"%PDF-1.7",
                )
            }),
        )]));
        let result = fetch_with_clients(
            "http://example.test/file.pdf",
            REQUEST_TIMEOUT,
            &resolver,
            &transport,
        );
        assert!(result.ok);
        assert_eq!(result.extraction_kind, ExtractionKind::BinarySummary);
        assert!(result.content.contains("bytes: 182344"));
    }

    #[test]
    fn low_yield_shell_pages_are_flagged() {
        let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
        let body =
            br#"<html><head><title>App</title></head><body><div id="__next"></div></body></html>"#;
        let transport = FakeTransport::new(HashMap::from([(
            "http://example.test/app".to_string(),
            Ok(response(
                "http://example.test/app",
                200,
                Some("text/html"),
                body,
            )),
        )]));
        let result = fetch_with_clients(
            "http://example.test/app",
            REQUEST_TIMEOUT,
            &resolver,
            &transport,
        );
        assert!(result.warnings.contains(&Warning::LowContentYield));
        assert!(result.warnings.contains(&Warning::PossibleJsRenderedPage));
    }

    #[test]
    fn http_errors_keep_body_snippets_and_mark_error() {
        let resolver = FakeResolver::default().with_mapping("example.test", 80, vec![socket(80)]);
        let body = br#"<html><body><main><h1>Missing</h1><p>Not here.</p></main></body></html>"#;
        let transport = FakeTransport::new(HashMap::from([(
            "http://example.test/missing".to_string(),
            Ok(response(
                "http://example.test/missing",
                404,
                Some("text/html"),
                body,
            )),
        )]));
        let result = fetch_with_clients(
            "http://example.test/missing",
            REQUEST_TIMEOUT,
            &resolver,
            &transport,
        );
        assert_eq!(result.error.as_deref(), Some("http_404"));
        assert_eq!(result.extraction_kind, ExtractionKind::Error);
        assert!(result.content.contains("Missing"));
    }

    #[test]
    fn render_cli_output_shows_header_block() {
        let rendered = render_cli_output(&FetchResult {
            ok: false,
            requested_url: "https://example.com".to_string(),
            final_url: None,
            status: None,
            content_type: None,
            content: String::new(),
            extraction_kind: ExtractionKind::Error,
            warnings: vec![Warning::LowContentYield],
            error: Some("timeout".to_string()),
            truncated: false,
            bytes_read: 0,
        });
        assert!(rendered.contains("URL:             https://example.com"));
        assert!(rendered.contains("Warnings:        LowContentYield"));
        assert!(rendered.contains("Error:           timeout"));
        assert!(rendered.contains("(no content)"));
    }
}
