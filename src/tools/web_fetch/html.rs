use std::sync::OnceLock;

use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use url::Url;

use super::{
    ExtractionKind, MIN_CONTENT_CHARS, MIN_LOW_SIGNAL_CHARS, MIN_PRIMARY_ROOT_CHARS, Warning,
    content::{
        ExtractedContent, normalize_content_type, normalize_inline, push_warning, truncate_chars,
        warning_list,
    },
    render::{RenderedBody, render_root, strip_outer_blank_lines, unmatched_markdown_code_fence},
    sites::{self, SiteExtraction},
};

static SELECTORS: OnceLock<Result<Selectors, String>> = OnceLock::new();
const MAX_SHELL_MARKER_SCAN_BYTES: usize = 64 * 1024;
const NAV_HEADING_PHRASES: &[&str] = &[
    "related articles",
    "related posts",
    "related stories",
    "recommended articles",
    "recommended reading",
    "popular articles",
    "popular posts",
    "trending stories",
    "more articles",
    "more stories",
    "you may also like",
    "you might also like",
    "also read",
    "further reading",
    "most read",
    "top stories",
];

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
    source: RootSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSource {
    Main,
    Article,
    RoleMain,
    IdContent,
    IdMain,
    ClassContent,
    Body,
}

#[derive(Clone)]
struct RootCandidate<'a> {
    root: RootSelection<'a>,
    rendered: RenderedBody,
    score: f64,
    low_signal: bool,
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
    let parsed_source_url = source_url.and_then(|value| Url::parse(value).ok());
    let schema = extract_schema_info(&document, selectors);
    let site = sites::extract(source_url, &document);
    let root = select_best_root(&document, selectors, parsed_source_url.as_ref());
    let metadata = extract_metadata(&document, selectors, &root, &schema, site.as_ref());

    let displayed_title = metadata
        .title
        .as_deref()
        .or(metadata.h1.as_deref())
        .map(ToOwned::to_owned);
    let rendered = render_root(
        root.element,
        displayed_title.as_deref(),
        parsed_source_url.as_ref(),
    );
    let dom_body_text = rendered.text;
    let body_visible_text_len = rendered.visible_len;
    let low_signal = is_low_signal_extraction(
        &dom_body_text,
        body_visible_text_len,
        root.element.html().len(),
    );
    let rescued_site_body = site
        .as_ref()
        .filter(|value| !value.body.trim().is_empty())
        .map(|value| normalize_schema_text(&value.body));
    let rescued_body = rescued_site_body.clone().or_else(|| {
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
        kind: if rescued_site_body.is_some() {
            ExtractionKind::HtmlPrimary
        } else {
            root.kind
        },
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

fn select_best_root<'a>(
    document: &'a Html,
    selectors: &'a Selectors,
    base_url: Option<&Url>,
) -> RootSelection<'a> {
    let mut candidates = collect_root_candidates(document, selectors, base_url);
    if candidates.is_empty() {
        return RootSelection {
            element: document
                .select(&selectors.body)
                .next()
                .or_else(|| document.root_element().select(&selectors.body).next())
                .unwrap_or_else(|| document.root_element()),
            kind: ExtractionKind::HtmlFallback,
            source: RootSource::Body,
        };
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.rendered.visible_len.cmp(&left.rendered.visible_len))
    });

    let best_candidate = candidates
        .iter()
        .find(|candidate| !candidate.low_signal)
        .unwrap_or(&candidates[0]);

    if matches!(best_candidate.root.source, RootSource::Body)
        && let Some(primary_candidate) = candidates.iter().find(|candidate| {
            !candidate.low_signal
                && !matches!(candidate.root.source, RootSource::Body)
                && candidate.rendered.visible_len >= MIN_PRIMARY_ROOT_CHARS
        })
    {
        return primary_candidate.root.clone();
    }

    best_candidate.root.clone()
}

fn collect_root_candidates<'a>(
    document: &'a Html,
    selectors: &'a Selectors,
    base_url: Option<&Url>,
) -> Vec<RootCandidate<'a>> {
    let mut roots = Vec::new();
    let mut seen = Vec::new();

    for (selector, source) in [
        (&selectors.main, RootSource::Main),
        (&selectors.article, RootSource::Article),
        (&selectors.role_main, RootSource::RoleMain),
        (&selectors.id_content, RootSource::IdContent),
        (&selectors.id_main, RootSource::IdMain),
        (&selectors.class_content, RootSource::ClassContent),
    ] {
        for element in document.select(selector) {
            let element_id = element.id();
            if seen.contains(&element_id) {
                continue;
            }
            seen.push(element_id);
            roots.push(RootSelection {
                element,
                kind: ExtractionKind::HtmlPrimary,
                source,
            });
        }
    }

    if let Some(element) = document
        .select(&selectors.body)
        .next()
        .or_else(|| document.root_element().select(&selectors.body).next())
    {
        if !seen.contains(&element.id()) {
            roots.push(RootSelection {
                element,
                kind: ExtractionKind::HtmlFallback,
                source: RootSource::Body,
            });
        }
    } else {
        roots.push(RootSelection {
            element: document.root_element(),
            kind: ExtractionKind::HtmlFallback,
            source: RootSource::Body,
        });
    }

    let mut candidates = roots
        .into_iter()
        .map(|root| evaluate_root_candidate(root, base_url))
        .collect::<Vec<_>>();
    dedupe_nested_candidates(&mut candidates);
    candidates
}

fn evaluate_root_candidate<'a>(
    root: RootSelection<'a>,
    base_url: Option<&Url>,
) -> RootCandidate<'a> {
    let rendered = render_root(root.element, None, base_url);
    let raw_html_len = root.element.html().len();
    let low_signal = is_low_signal_extraction(&rendered.text, rendered.visible_len, raw_html_len);
    let visible_len = rendered.visible_len.max(1) as f64;
    let raw_html_len = raw_html_len.max(1) as f64;
    let yield_ratio = visible_len / raw_html_len;
    let mut paragraph_count = 0usize;
    let mut list_item_count = 0usize;
    let mut code_block_count = 0usize;
    let mut table_count = 0usize;
    let mut link_text_len = 0usize;
    let mut nav_heading_count = 0usize;
    for element in root.element.descendent_elements() {
        match element.value().name() {
            "p" | "figcaption" => paragraph_count += 1,
            "li" => list_item_count += 1,
            "pre" => code_block_count += 1,
            "table" => table_count += 1,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let heading_text = normalize_inline(&element.text().collect::<String>());
                if heading_matches_nav_phrase(&heading_text) {
                    nav_heading_count += 1;
                }
            }
            "a" => {
                link_text_len += normalize_inline(&element.text().collect::<String>())
                    .chars()
                    .count();
            }
            _ => {}
        }
    }
    let comma_count = rendered.text.chars().filter(|&ch| ch == ',').count() as f64;
    let link_density = (link_text_len as f64 / visible_len).clamp(0.0, 1.0);
    let noise_penalty = noisy_token_penalty(root.element);
    let body_penalty = if matches!(root.source, RootSource::Body) {
        120.0
    } else {
        0.0
    };
    let low_signal_penalty = if low_signal { 140.0 } else { 0.0 };
    let nav_heading_penalty = nav_heading_count as f64 * 90.0;
    let content_score = visible_len
        + (paragraph_count as f64 * 18.0)
        + (list_item_count as f64 * 8.0)
        + (code_block_count as f64 * 30.0)
        + (table_count as f64 * 20.0)
        + (yield_ratio * 220.0)
        + (comma_count * 1.5);
    let link_multiplier = 1.0 - (link_density * 0.85);
    let score = (content_score * link_multiplier)
        - noise_penalty
        - body_penalty
        - low_signal_penalty
        - nav_heading_penalty;

    RootCandidate {
        root,
        rendered,
        score,
        low_signal,
    }
}

fn dedupe_nested_candidates(candidates: &mut Vec<RootCandidate<'_>>) {
    let mut keep = vec![true; candidates.len()];
    for outer_index in 0..candidates.len() {
        if !keep[outer_index] {
            continue;
        }
        if matches!(candidates[outer_index].root.source, RootSource::Body) {
            continue;
        }
        for inner_index in 0..candidates.len() {
            if outer_index == inner_index || !keep[inner_index] {
                continue;
            }
            if matches!(candidates[inner_index].root.source, RootSource::Body) {
                continue;
            }
            let outer = &candidates[outer_index];
            let inner = &candidates[inner_index];
            if is_same_or_ancestor(outer.root.element, inner.root.element)
                && inner.rendered.visible_len.saturating_mul(100)
                    >= outer.rendered.visible_len.saturating_mul(85)
            {
                keep[outer_index] = false;
                break;
            }
        }
    }

    let mut index = 0usize;
    candidates.retain(|_| {
        let retained = keep[index];
        index += 1;
        retained
    });
}

fn is_same_or_ancestor(ancestor: ElementRef<'_>, descendant: ElementRef<'_>) -> bool {
    descendant
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|element| element.id() == ancestor.id())
}

fn noisy_token_penalty(element: ElementRef<'_>) -> f64 {
    let mut penalty = 0.0;
    for attr in ["class", "id"] {
        if let Some(value) = element.value().attr(attr)
            && super::has_noisy_attribute_token(value)
        {
            penalty += 35.0;
        }
    }
    penalty
}

fn heading_matches_nav_phrase(heading_text: &str) -> bool {
    let heading_tokens = ascii_word_tokens(heading_text);
    if heading_tokens.is_empty() {
        return false;
    }

    NAV_HEADING_PHRASES.iter().any(|phrase| {
        let phrase_tokens = ascii_word_tokens(phrase);
        !phrase_tokens.is_empty()
            && phrase_tokens.len() <= heading_tokens.len()
            && heading_tokens.windows(phrase_tokens.len()).any(|window| {
                window
                    .iter()
                    .zip(&phrase_tokens)
                    .all(|(heading_token, phrase_token)| heading_token == phrase_token)
            })
    })
}

fn ascii_word_tokens(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
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
    if raw_html_len < 2_048 {
        return false;
    }
    // Need at least 200 chars or 2% of the HTML size, whichever is larger.
    visible_len < MIN_LOW_SIGNAL_CHARS.max(raw_html_len / 50)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_score(html: &str, selector: &str, source: RootSource) -> f64 {
        let document = Html::parse_document(html);
        let selector = Selector::parse(selector).expect("selector");
        let element = document.select(&selector).next().expect("element");

        evaluate_root_candidate(
            RootSelection {
                element,
                kind: ExtractionKind::HtmlPrimary,
                source,
            },
            None,
        )
        .score
    }

    fn noisy_penalty(html: &str, selector: &str) -> f64 {
        let document = Html::parse_document(html);
        let selector = Selector::parse(selector).expect("selector");
        let element = document.select(&selector).next().expect("element");
        noisy_token_penalty(element)
    }

    #[test]
    fn link_density_scaling_prefers_clean_article_over_large_link_hub() {
        let link_cluster =
            "<a href='/story'>Platform launch guide and release notes</a> ".repeat(70);
        let html = format!(
            r#"
            <html>
              <body>
                <div id="content"><p>{link_cluster}</p></div>
                <article>
                  <p>This article explains how the worker pool initializes, how retries behave, and how operators should verify rollout health during deployment.</p>
                  <p>It also covers failure handling, metrics, incident response, migration sequencing, cache invalidation, and compatibility expectations for older clients.</p>
                  <p>Operators can use it to validate deploy order, compare health signals between regions, confirm alert routing, and rehearse rollback steps before customer traffic shifts.</p>
                </article>
              </body>
            </html>
            "#
        );

        let link_hub_score = candidate_score(&html, "#content", RootSource::IdContent);
        let article_score = candidate_score(&html, "article", RootSource::Article);

        assert!(
            article_score > link_hub_score,
            "expected clean article to outrank link hub, article={article_score}, link_hub={link_hub_score}"
        );
    }

    #[test]
    fn comma_density_breaks_ties_in_favor_of_prose() {
        let html = r#"
            <html>
              <body>
                <article id="prose">
                  <p>Alpha, beta, gamma, delta, epsilon, zeta, eta, theta, iota, kappa.</p>
                  <p>Lambda, mu, nu, xi, omicron, pi, rho, sigma, tau, upsilon.</p>
                </article>
                <article id="listy">
                  <p>Alpha beta gamma delta epsilon zeta eta theta iota kappa.</p>
                  <p>Lambda mu nu xi omicron pi rho sigma tau upsilon.</p>
                </article>
              </body>
            </html>
        "#;

        let prose_score = candidate_score(html, "#prose", RootSource::Article);
        let listy_score = candidate_score(html, "#listy", RootSource::Article);

        assert!(
            prose_score > listy_score,
            "expected prose candidate to score higher, prose={prose_score}, listy={listy_score}"
        );
    }

    #[test]
    fn navigation_headings_reduce_candidate_score() {
        let html = r#"
            <html>
              <body>
                <section id="nav-heading">
                  <h2>Related Articles</h2>
                  <p><a href="/one">Compiler pipeline guide</a>, release notes, migration checklist, rollback drill, and incident guide.</p>
                  <p><a href="/two">Storage tuning handbook</a>, cache policy update, schema notes, and deployment sequencing.</p>
                </section>
                <section id="neutral-heading">
                  <h2>Implementation Notes</h2>
                  <p><a href="/one">Compiler pipeline guide</a>, release notes, migration checklist, rollback drill, and incident guide.</p>
                  <p><a href="/two">Storage tuning handbook</a>, cache policy update, schema notes, and deployment sequencing.</p>
                </section>
              </body>
            </html>
        "#;

        let nav_score = candidate_score(html, "#nav-heading", RootSource::ClassContent);
        let neutral_score = candidate_score(html, "#neutral-heading", RootSource::ClassContent);

        assert!(
            neutral_score > nav_score,
            "expected navigation heading to be penalized, neutral={neutral_score}, nav={nav_score}"
        );
    }

    #[test]
    fn newsletter_roots_receive_a_noisy_token_penalty() {
        let html = r#"
            <html>
              <body>
                <section class="newsletter-widget">
                  <p>Subscribe for updates and release notes.</p>
                </section>
              </body>
            </html>
        "#;

        assert_eq!(noisy_penalty(html, ".newsletter-widget"), 35.0);
    }

    #[test]
    fn compound_noisy_tokens_count_once_per_attribute() {
        let html = r#"
            <html>
              <body>
                <section class="login-form-banner">
                  <p>Account onboarding steps.</p>
                </section>
              </body>
            </html>
        "#;

        assert_eq!(noisy_penalty(html, "section"), 35.0);
    }

    #[test]
    fn nav_heading_matching_requires_clear_recommendation_phrases() {
        assert!(heading_matches_nav_phrase("Related Articles"));
        assert!(heading_matches_nav_phrase("You may also like"));
        assert!(!heading_matches_nav_phrase("Unrelated Work"));
        assert!(!heading_matches_nav_phrase("Recommended Configuration"));
    }
}
