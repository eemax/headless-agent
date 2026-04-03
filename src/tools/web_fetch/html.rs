use std::sync::OnceLock;

use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

use super::{
    ExtractionKind, MIN_CONTENT_CHARS, Warning,
    content::{
        ExtractedContent, normalize_content_type, normalize_inline, push_warning, truncate_chars,
        warning_list,
    },
    render::{render_root, strip_outer_blank_lines, unmatched_markdown_code_fence},
    sites::{self, SiteExtraction},
};

static SELECTORS: OnceLock<Result<Selectors, String>> = OnceLock::new();
const MAX_SHELL_MARKER_SCAN_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct Selectors {
    title: Selector,
    h1: Selector,
    meta_description: Selector,
    meta_og_description: Selector,
    meta_twitter_description: Selector,
    meta_og_title: Selector,
    meta_twitter_title: Selector,
    meta_author_name: Selector,
    meta_author_property: Selector,
    meta_article_author: Selector,
    meta_published_name: Selector,
    meta_published_property: Selector,
    schema_json: Selector,
    time_datetime: Selector,
    rel_author: Selector,
    itemprop_author: Selector,
    class_author: Selector,
    class_byline: Selector,
    main: Selector,
    article: Selector,
    role_main: Selector,
    id_content: Selector,
    id_main: Selector,
    class_content: Selector,
    body: Selector,
}

#[derive(Debug)]
struct HtmlMetadata {
    title: Option<String>,
    h1: Option<String>,
    description: Option<String>,
    author: Option<String>,
    published: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct SchemaInfo {
    title: Option<String>,
    description: Option<String>,
    author: Option<String>,
    published: Option<String>,
    text: Option<String>,
}

#[derive(Clone)]
struct RootSelection<'a> {
    element: ElementRef<'a>,
    kind: ExtractionKind,
}

pub(super) fn extract_html(
    source_url: Option<&str>,
    content_type_header: Option<&str>,
    html: &str,
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

    let document = Html::parse_document(html);
    let schema = extract_schema_info(&document, selectors);
    let site = sites::extract(source_url, &document);
    let root = select_root(&document, selectors);
    let metadata = extract_metadata(&document, selectors, &root, &schema, site.as_ref());

    let displayed_title = metadata
        .title
        .as_deref()
        .or(metadata.h1.as_deref())
        .map(ToOwned::to_owned);
    let rendered = render_root(root.element, displayed_title.as_deref());
    let dom_body_text = rendered.text;
    let body_visible_text_len = rendered.visible_len;
    let low_signal = is_low_signal_extraction(&dom_body_text, body_visible_text_len, html.len());
    let rescued_body = site
        .as_ref()
        .filter(|value| !value.body.trim().is_empty())
        .map(|value| normalize_schema_text(&value.body))
        .or_else(|| {
            select_schema_fallback(&dom_body_text, body_visible_text_len, low_signal, &schema)
        });
    let used_rescue = rescued_body.is_some();
    let body_text = rescued_body.unwrap_or(dom_body_text);
    let final_visible_text_len = body_text.chars().count();
    let final_low_signal = !used_rescue && low_signal;

    let mut content = String::new();
    append_metadata_line(&mut content, "Title", displayed_title.as_deref());
    append_metadata_line(&mut content, "Description", metadata.description.as_deref());
    append_metadata_line(&mut content, "Author", metadata.author.as_deref());
    append_metadata_line(&mut content, "Published", metadata.published.as_deref());
    if !content.is_empty() && !body_text.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(&body_text);
    let content = strip_outer_blank_lines(&content);
    let (content, content_truncated) =
        truncate_rendered_content(&content, super::MAX_CONTENT_CHARS);

    let mut warnings = Vec::new();
    if final_visible_text_len < MIN_CONTENT_CHARS {
        push_warning(&mut warnings, Warning::LowContentYield);
    }
    if final_low_signal {
        push_warning(&mut warnings, Warning::LowSignalExtraction);
        if html_has_shell_markers(html) {
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

fn extract_metadata(
    document: &Html,
    selectors: &Selectors,
    root: &RootSelection<'_>,
    schema: &SchemaInfo,
    site: Option<&SiteExtraction>,
) -> HtmlMetadata {
    let h1 = document
        .select(&selectors.h1)
        .next()
        .map(|value| normalize_inline(&value.text().collect::<String>()))
        .filter(|value| !value.is_empty());
    let title = site
        .and_then(|value| value.title.clone())
        .or_else(|| {
            document
                .select(&selectors.title)
                .next()
                .map(|value| normalize_inline(&value.text().collect::<String>()))
                .filter(|value| !value.is_empty())
        })
        .or_else(|| meta_content(document, &selectors.meta_og_title))
        .or_else(|| meta_content(document, &selectors.meta_twitter_title))
        .or_else(|| schema.title.clone());
    let description = site
        .and_then(|value| value.description.clone())
        .or_else(|| meta_content(document, &selectors.meta_description))
        .or_else(|| meta_content(document, &selectors.meta_og_description))
        .or_else(|| meta_content(document, &selectors.meta_twitter_description))
        .or_else(|| schema.description.clone());
    let author = site
        .and_then(|value| value.author.clone())
        .or_else(|| schema.author.clone())
        .or_else(|| meta_content(document, &selectors.meta_author_name))
        .or_else(|| meta_content(document, &selectors.meta_author_property))
        .or_else(|| meta_content(document, &selectors.meta_article_author))
        .or_else(|| extract_nearby_author(root.element, selectors));
    let published = site
        .and_then(|value| value.published.clone())
        .or_else(|| schema.published.clone())
        .or_else(|| meta_content(document, &selectors.meta_published_property))
        .or_else(|| meta_content(document, &selectors.meta_published_name))
        .or_else(|| extract_nearby_published(root.element, selectors));

    HtmlMetadata {
        title,
        h1,
        description,
        author,
        published,
    }
}

fn meta_content(document: &Html, selector: &Selector) -> Option<String> {
    document
        .select(selector)
        .next()
        .and_then(|value| value.value().attr("content"))
        .map(normalize_inline)
        .filter(|value| !value.is_empty())
}

fn extract_schema_info(document: &Html, selectors: &Selectors) -> SchemaInfo {
    let mut info = SchemaInfo::default();
    for script in document.select(&selectors.schema_json) {
        let raw = script.text().collect::<String>();
        if raw.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        merge_schema_info(&mut info, &value, 0);
    }
    info
}

fn merge_schema_info(info: &mut SchemaInfo, value: &Value, depth: usize) {
    if depth > 12 {
        return;
    }

    match value {
        Value::Array(items) => {
            for item in items {
                merge_schema_info(info, item, depth + 1);
            }
        }
        Value::Object(map) => {
            if info.text.is_none()
                && let Some(text) = map
                    .get("articleBody")
                    .and_then(json_string)
                    .or_else(|| map.get("text").and_then(json_string))
            {
                let text = normalize_schema_text(&text);
                if is_useful_schema_text(&text) {
                    info.text = Some(text);
                }
            }
            if info.title.is_none() {
                info.title = map
                    .get("headline")
                    .and_then(json_string)
                    .or_else(|| map.get("name").and_then(json_string))
                    .map(|value| normalize_inline(&value))
                    .filter(|value| !value.is_empty());
            }
            if info.description.is_none() {
                info.description = map
                    .get("description")
                    .and_then(json_string)
                    .map(|value| normalize_inline(&value))
                    .filter(|value| !value.is_empty());
            }
            if info.author.is_none() {
                info.author = map.get("author").and_then(extract_schema_author);
            }
            if info.published.is_none() {
                info.published = map
                    .get("datePublished")
                    .and_then(json_string)
                    .or_else(|| map.get("dateCreated").and_then(json_string))
                    .or_else(|| map.get("uploadDate").and_then(json_string))
                    .map(|value| normalize_inline(&value))
                    .filter(|value| !value.is_empty());
            }

            for key in [
                "@graph",
                "mainEntity",
                "mainEntityOfPage",
                "itemListElement",
            ] {
                if let Some(child) = map.get(key) {
                    merge_schema_info(info, child, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn extract_schema_author(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => extract_author_candidate(text),
        Value::Object(map) => map
            .get("name")
            .and_then(json_string)
            .and_then(|value| extract_author_candidate(&value)),
        Value::Array(items) => {
            let mut authors = Vec::new();
            for item in items {
                let Some(author) = extract_schema_author(item) else {
                    continue;
                };
                if !authors.contains(&author) {
                    authors.push(author);
                }
            }
            (!authors.is_empty()).then_some(authors.join(", "))
        }
        _ => None,
    }
}

fn json_string(value: &Value) -> Option<String> {
    value.as_str().map(ToOwned::to_owned)
}

fn normalize_schema_text(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    strip_outer_blank_lines(&normalized)
}

fn is_useful_schema_text(text: &str) -> bool {
    let chars = text.chars().count();
    let words = text.split_whitespace().count();
    chars >= 40 || words >= 8 || text.contains('\n')
}

fn select_schema_fallback(
    body_text: &str,
    visible_len: usize,
    low_signal: bool,
    schema: &SchemaInfo,
) -> Option<String> {
    let schema_text = schema.text.as_deref()?;
    let body_len = body_text.trim().chars().count();
    let schema_len = schema_text.chars().count();

    if body_len == 0 {
        return Some(schema_text.to_string());
    }
    if low_signal && schema_len >= body_len.max(40) {
        return Some(schema_text.to_string());
    }
    if visible_len < (MIN_CONTENT_CHARS / 2)
        && schema_len >= body_len.saturating_mul(2).max(80)
        && schema_len >= body_len.saturating_add(80)
    {
        return Some(schema_text.to_string());
    }
    None
}

fn extract_nearby_author(root: ElementRef<'_>, selectors: &Selectors) -> Option<String> {
    for selector in [
        &selectors.rel_author,
        &selectors.itemprop_author,
        &selectors.class_byline,
        &selectors.class_author,
    ] {
        for element in root.select(selector) {
            let text = normalize_inline(&element.text().collect::<String>());
            if let Some(author) = extract_author_candidate(&text) {
                return Some(author);
            }
        }
    }
    None
}

fn extract_author_candidate(input: &str) -> Option<String> {
    let normalized = normalize_inline(input);
    if normalized.is_empty() {
        return None;
    }

    let mut candidate = normalized
        .strip_prefix("By ")
        .or_else(|| normalized.strip_prefix("by "))
        .unwrap_or(&normalized)
        .trim();

    for separator in ['·', '|', '—', '–'] {
        candidate = candidate
            .split(separator)
            .next()
            .unwrap_or(candidate)
            .trim();
    }

    if let Some(prefix) = candidate.strip_suffix(',') {
        candidate = prefix.trim();
    }

    let words = candidate.split_whitespace().count();
    let lower = candidate.to_ascii_lowercase();
    let has_month = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .any(|value| lower.contains(value));

    if candidate.is_empty()
        || candidate.len() > 80
        || words == 0
        || words > 8
        || lower == "author"
        || lower == "by"
        || candidate.chars().any(|ch| ch.is_ascii_digit())
        || has_month
    {
        return None;
    }

    Some(candidate.to_string())
}

fn extract_nearby_published(root: ElementRef<'_>, selectors: &Selectors) -> Option<String> {
    root.select(&selectors.time_datetime)
        .next()
        .and_then(|value| value.value().attr("datetime"))
        .map(normalize_inline)
        .filter(|value| !value.is_empty())
}

fn truncate_rendered_content(input: &str, limit: usize) -> (String, bool) {
    let (mut output, truncated) = truncate_chars(input, limit);
    if !truncated {
        return (output, false);
    }
    if let Some(fence) = unmatched_markdown_code_fence(&output) {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&fence);
    }
    (output, true)
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
        meta_og_description: parse_selector("meta[property='og:description']")?,
        meta_twitter_description: parse_selector("meta[name='twitter:description']")?,
        meta_og_title: parse_selector("meta[property='og:title']")?,
        meta_twitter_title: parse_selector("meta[name='twitter:title']")?,
        meta_author_name: parse_selector("meta[name='author']")?,
        meta_author_property: parse_selector("meta[property='author']")?,
        meta_article_author: parse_selector(
            "meta[property='article:author'], meta[name='article:author']",
        )?,
        meta_published_name: parse_selector(
            "meta[name='article:published_time'], meta[name='pubdate']",
        )?,
        meta_published_property: parse_selector("meta[property='article:published_time']")?,
        schema_json: parse_selector("script[type='application/ld+json']")?,
        time_datetime: parse_selector("time[datetime]")?,
        rel_author: parse_selector("[rel='author']")?,
        itemprop_author: parse_selector("[itemprop='author']")?,
        class_author: parse_selector(".author")?,
        class_byline: parse_selector(".byline")?,
        main: parse_selector("main")?,
        article: parse_selector("article")?,
        role_main: parse_selector("[role='main']")?,
        id_content: parse_selector("#content")?,
        id_main: parse_selector("#main")?,
        class_content: parse_selector(".content")?,
        body: parse_selector("body")?,
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

fn is_low_signal_extraction(body_text: &str, visible_len: usize, raw_html_len: usize) -> bool {
    if body_text.trim().is_empty() || visible_len == 0 {
        return true;
    }

    let raw_html_len = raw_html_len.max(1);
    let yield_ratio = visible_len as f64 / raw_html_len as f64;

    (visible_len < MIN_CONTENT_CHARS && raw_html_len >= 2_048)
        || (visible_len < 300 && raw_html_len >= 32_768 && yield_ratio < 0.02)
        || (visible_len < 2_000 && raw_html_len >= 500_000 && yield_ratio < 0.005)
}

fn html_has_shell_markers(html: &str) -> bool {
    let sample = &html.as_bytes()[..html.len().min(MAX_SHELL_MARKER_SCAN_BYTES)];
    [
        "__next",
        "id=\"root\"",
        "id='root'",
        "id=\"app\"",
        "id='app'",
        "data-reactroot",
        "__nuxt",
        "ng-version",
    ]
    .iter()
    .any(|marker| contains_ascii_case_insensitive(sample, marker.as_bytes()))
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}
