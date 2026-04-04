use std::sync::OnceLock;

use scraper::{ElementRef, Html, Selector};
use serde_json::Value;
use url::Url;

use super::SiteExtraction;
use crate::tools::web_fetch::{
    content::normalize_inline,
    render::{
        render_element_blocks, render_html_fragment, render_markdown_code_block,
        strip_outer_blank_lines,
    },
};

static SELECTORS: OnceLock<Result<Selectors, String>> = OnceLock::new();

#[derive(Debug)]
struct Selectors {
    embedded_data: Selector,
    issue_container: Selector,
    issue_body: Selector,
    issue_author: Selector,
    issue_comment_container: Selector,
    pr_body: Selector,
    pr_author: Selector,
    pr_discussion_container: Selector,
    visible_discussion_body: Selector,
    visible_discussion_author: Selector,
    relative_time: Selector,
    release_section: Selector,
    release_title: Selector,
    release_author: Selector,
    release_body: Selector,
    release_asset: Selector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GithubRoute {
    RepoOverview,
    Tree,
    Blob,
    Issue { number: u64 },
    Pull { number: u64 },
    Releases,
    ReleaseLatest,
    ReleaseTag,
}

#[derive(Debug, Default, Clone)]
struct DiscussionContent {
    body: String,
    author: Option<String>,
    published: Option<String>,
    comments: Vec<DiscussionEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct DiscussionEntry {
    author: Option<String>,
    published: Option<String>,
    body: String,
}

pub(super) fn extract(source_url: Option<&str>, document: &Html) -> Option<SiteExtraction> {
    let source_url = source_url?;
    let parsed_url = Url::parse(source_url).ok()?;
    let route = classify_route(source_url)?;
    let selectors = selectors().ok()?;

    match route {
        GithubRoute::RepoOverview => extract_repo_overview(document, selectors, &parsed_url),
        GithubRoute::Tree => extract_tree(document, selectors, &parsed_url),
        GithubRoute::Blob => extract_blob(document, selectors, &parsed_url),
        GithubRoute::Issue { number } => {
            extract_issue_or_pull(document, selectors, &parsed_url, number, true)
        }
        GithubRoute::Pull { number } => {
            extract_issue_or_pull(document, selectors, &parsed_url, number, false)
        }
        GithubRoute::Releases | GithubRoute::ReleaseLatest => {
            extract_release(document, selectors, &parsed_url, ReleaseScope::FirstSection)
        }
        GithubRoute::ReleaseTag => {
            extract_release(document, selectors, &parsed_url, ReleaseScope::WholeDocument)
        }
    }
}

fn extract_repo_overview(
    document: &Html,
    selectors: &Selectors,
    source_url: &Url,
) -> Option<SiteExtraction> {
    let payload = embedded_payload(document, selectors)?;
    let items = get_path(&payload, &["payload", "codeViewRepoRoute", "tree", "items"])
        .and_then(Value::as_array);
    let entries = items.map(|items| render_tree_entries("Top-level entries", items));

    let readme = get_path(
        &payload,
        &["payload", "codeViewRepoRoute", "overview", "overviewFiles"],
    )
    .and_then(Value::as_array)
    .and_then(|files| select_primary_overview_file(files))
    .and_then(extract_rich_text_field)
    .map(|html| render_html_fragment(&html, Some(source_url)))
    .filter(|text| !text.trim().is_empty());

    let body = join_sections([entries, readme]);
    (!body.is_empty()).then_some(SiteExtraction {
        body,
        ..SiteExtraction::default()
    })
}

fn extract_tree(document: &Html, selectors: &Selectors, source_url: &Url) -> Option<SiteExtraction> {
    let payload = embedded_payload(document, selectors)?;
    let items = get_path(&payload, &["payload", "codeViewTreeRoute", "tree", "items"])
        .and_then(Value::as_array);
    let entries = items.map(|items| render_tree_entries("Directory entries", items));
    let readme = get_path(
        &payload,
        &["payload", "codeViewTreeRoute", "tree", "readme"],
    )
    .and_then(|value| extract_rich_text_value(value, Some(source_url)))
    .filter(|text| !text.trim().is_empty());

    let body = join_sections([entries, readme]);
    (!body.is_empty()).then_some(SiteExtraction {
        body,
        ..SiteExtraction::default()
    })
}

fn extract_blob(document: &Html, selectors: &Selectors, source_url: &Url) -> Option<SiteExtraction> {
    let payload = embedded_payload(document, selectors)?;
    let rendered = get_path(&payload, &["payload", "codeViewBlobRoute", "richText"])
        .and_then(Value::as_str)
        .map(|html| render_html_fragment(html, Some(source_url)))
        .filter(|text| !text.trim().is_empty());

    if let Some(body) = rendered {
        return Some(SiteExtraction {
            body,
            ..SiteExtraction::default()
        });
    }

    let raw_lines = get_path(
        &payload,
        &["payload", "codeViewBlobLayoutRoute.StyledBlob", "rawLines"],
    )
    .and_then(Value::as_array)
    .map(|lines| {
        lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    })
    .filter(|text| !text.is_empty())?;
    let language = get_path(
        &payload,
        &["payload", "codeViewBlobLayoutRoute", "blob", "language"],
    )
    .and_then(Value::as_str)
    .map(|value| value.trim().to_ascii_lowercase())
    .filter(|value| !value.is_empty());
    let body = render_markdown_code_block(language.as_deref(), &raw_lines);

    Some(SiteExtraction {
        body,
        ..SiteExtraction::default()
    })
}

fn extract_issue_or_pull(
    document: &Html,
    selectors: &Selectors,
    source_url: &Url,
    route_number: u64,
    is_issue: bool,
) -> Option<SiteExtraction> {
    let embedded = extract_embedded_discussion(document, selectors, source_url, route_number);
    let primary_body = if !embedded.body.is_empty() {
        embedded.body.clone()
    } else if is_issue {
        extract_issue_visible_body(document, selectors, source_url)
    } else {
        extract_pr_visible_body(document, selectors, source_url)
    };
    let discussion = if !embedded.comments.is_empty() {
        embedded.comments.clone()
    } else if is_issue {
        extract_issue_visible_discussion(document, selectors, source_url)
    } else {
        extract_pr_visible_discussion(document, selectors, source_url)
    };
    let body = join_sections([
        (!primary_body.trim().is_empty()).then_some(primary_body),
        (!discussion.is_empty()).then_some(render_discussion_section(&discussion)),
    ]);
    let author = embedded.author.or_else(|| {
        if is_issue {
            extract_issue_visible_author(document, selectors)
        } else {
            extract_pr_visible_author(document, selectors)
        }
    });
    let published = embedded
        .published
        .or_else(|| extract_visible_published(document, selectors));

    if body.trim().is_empty() && author.is_none() && published.is_none() {
        None
    } else {
        Some(SiteExtraction {
            author,
            published,
            body,
            ..SiteExtraction::default()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseScope {
    FirstSection,
    WholeDocument,
}

fn extract_release(
    document: &Html,
    selectors: &Selectors,
    source_url: &Url,
    scope: ReleaseScope,
) -> Option<SiteExtraction> {
    let section = match scope {
        ReleaseScope::FirstSection => document.select(&selectors.release_section).next(),
        ReleaseScope::WholeDocument => None,
    };

    let title = select_text(section, document, &selectors.release_title);
    let author = select_text(section, document, &selectors.release_author);
    let published = select_datetime(section, document, &selectors.relative_time);
    let body = select_element(section, document, &selectors.release_body)
        .map(|element| render_element_blocks(element, Some(source_url)))
        .filter(|text| !text.trim().is_empty());
    let assets = collect_release_assets(section, document, selectors);

    let body = join_sections([
        body,
        (!assets.is_empty()).then_some(render_asset_list(&assets)),
    ]);

    if body.is_empty() && title.is_none() && author.is_none() && published.is_none() {
        None
    } else {
        Some(SiteExtraction {
            title,
            author,
            published,
            body,
            ..SiteExtraction::default()
        })
    }
}

fn collect_release_assets(
    section: Option<ElementRef<'_>>,
    document: &Html,
    selectors: &Selectors,
) -> Vec<String> {
    let mut assets = Vec::new();

    match section {
        Some(section) => {
            for asset in section.select(&selectors.release_asset) {
                push_release_asset(&mut assets, asset);
            }
        }
        None => {
            for asset in document.select(&selectors.release_asset) {
                push_release_asset(&mut assets, asset);
            }
        }
    }

    assets
}

fn push_release_asset(assets: &mut Vec<String>, asset: ElementRef<'_>) {
    let label = normalize_inline(&asset.text().collect::<String>());
    let label = if label.is_empty() {
        asset
            .value()
            .attr("href")
            .and_then(|href| href.rsplit('/').next())
            .map(normalize_inline)
            .unwrap_or_default()
    } else {
        label
    };

    if !label.is_empty() && !assets.contains(&label) {
        assets.push(label);
    }
}

fn render_asset_list(assets: &[String]) -> String {
    let mut body = String::from("## Assets\n");
    for asset in assets {
        body.push_str("- ");
        body.push_str(asset);
        body.push('\n');
    }
    strip_outer_blank_lines(&body)
}

fn render_tree_entries(heading: &str, items: &[Value]) -> String {
    let mut body = String::new();
    body.push_str("## ");
    body.push_str(heading);
    body.push('\n');

    for item in items {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let content_type = item
            .get("contentType")
            .and_then(Value::as_str)
            .unwrap_or("");
        body.push_str("- ");
        body.push_str(name);
        if content_type.eq_ignore_ascii_case("directory") {
            body.push('/');
        } else if !content_type.is_empty() && !content_type.eq_ignore_ascii_case("file") {
            body.push_str(" (");
            body.push_str(content_type);
            body.push(')');
        }
        body.push('\n');
    }

    strip_outer_blank_lines(&body)
}

fn select_primary_overview_file(files: &[Value]) -> Option<&Value> {
    files
        .iter()
        .find(|file| {
            file.get("preferredFileType")
                .and_then(Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("readme"))
                .unwrap_or(false)
        })
        .or_else(|| {
            files.iter().find(|file| {
                file.get("tabName")
                    .and_then(Value::as_str)
                    .map(|value| value.eq_ignore_ascii_case("readme"))
                    .unwrap_or(false)
            })
        })
        .or_else(|| {
            files.iter().find(|file| {
                file.get("displayName")
                    .and_then(Value::as_str)
                    .map(|value| value.to_ascii_lowercase().starts_with("readme"))
                    .unwrap_or(false)
            })
        })
        .or_else(|| {
            files
                .iter()
                .find(|file| extract_rich_text_field(file).is_some())
        })
}

fn extract_rich_text_value(value: &Value, base_url: Option<&Url>) -> Option<String> {
    if let Some(html) = value.as_str() {
        let rendered = render_html_fragment(html, base_url);
        return (!rendered.trim().is_empty()).then_some(rendered);
    }

    extract_rich_text_field(value)
        .map(|html| render_html_fragment(&html, base_url))
        .filter(|text| !text.trim().is_empty())
}

fn extract_rich_text_field(value: &Value) -> Option<String> {
    for key in ["richText", "html", "bodyHtml", "markup"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            let text = text.to_string();
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn extract_embedded_discussion(
    document: &Html,
    selectors: &Selectors,
    source_url: &Url,
    route_number: u64,
) -> DiscussionContent {
    let Some(payload) = embedded_payload(document, selectors) else {
        return DiscussionContent::default();
    };
    find_discussion_record(&payload, source_url, route_number).unwrap_or_default()
}

fn find_discussion_record(
    payload: &Value,
    source_url: &Url,
    route_number: u64,
) -> Option<DiscussionContent> {
    let queries = get_path(payload, &["payload", "preloadedQueries"])?.as_array()?;
    queries.iter().find_map(|query| {
        let issue = get_path(query, &["result", "data", "repository", "issue"])?;
        extract_repository_issue_discussion(issue, source_url, route_number)
    })
}

fn extract_repository_issue_discussion(
    issue: &Value,
    source_url: &Url,
    route_number: u64,
) -> Option<DiscussionContent> {
    let map = issue.as_object()?;
    if map.get("number").and_then(Value::as_u64)? != route_number {
        return None;
    }

    let body = extract_discussion_body(issue, source_url).unwrap_or_default();
    let comments = extract_timeline_discussion_entries(issue, source_url);
    if body.is_empty() && comments.is_empty() {
        return None;
    }

    Some(DiscussionContent {
        body,
        author: map.get("author").and_then(extract_discussion_author),
        published: map
            .get("createdAt")
            .and_then(Value::as_str)
            .map(normalize_inline)
            .filter(|value| !value.is_empty()),
        comments,
    })
}

fn extract_discussion_author(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get("login")
            .and_then(Value::as_str)
            .or_else(|| map.get("name").and_then(Value::as_str))
            .map(normalize_inline)
            .filter(|value| !value.is_empty()),
        Value::String(text) => {
            let text = normalize_inline(text);
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn extract_discussion_body(value: &Value, source_url: &Url) -> Option<String> {
    for key in ["bodyHTML", "bodyHtml", "html", "markup"] {
        if let Some(html) = value.get(key).and_then(Value::as_str) {
            let rendered = render_html_fragment(html, Some(source_url));
            if !rendered.trim().is_empty() {
                return Some(rendered);
            }
        }
    }
    for key in ["bodyText", "body"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            let normalized = normalize_multiline(text);
            if !normalized.is_empty() {
                return Some(normalized);
            }
        }
    }
    None
}

fn extract_timeline_discussion_entries(issue: &Value, source_url: &Url) -> Vec<DiscussionEntry> {
    let mut entries = Vec::new();
    for key in ["frontTimelineItems", "backTimelineItems"] {
        let Some(connection) = issue.get(key) else {
            continue;
        };
        for node in connection_nodes(connection) {
            collect_timeline_entries(node, source_url, 0, &mut entries);
        }
    }

    entries.sort_by(|left, right| {
        left.published
            .cmp(&right.published)
            .then_with(|| left.author.cmp(&right.author))
            .then_with(|| left.body.cmp(&right.body))
    });
    entries.dedup_by(|left, right| {
        left.author == right.author && left.published == right.published && left.body == right.body
    });
    entries
}

fn collect_timeline_entries(
    node: &Value,
    source_url: &Url,
    depth: usize,
    entries: &mut Vec<DiscussionEntry>,
) {
    if depth > 4 {
        return;
    }

    if let Some(entry) = extract_timeline_entry(node, source_url) {
        entries.push(entry);
    }
    for key in ["comments", "reviewComments", "reviewThreads", "threads"] {
        let Some(children) = node.get(key) else {
            continue;
        };
        for child in connection_nodes(children) {
            collect_timeline_entries(child, source_url, depth + 1, entries);
        }
    }
}

fn extract_timeline_entry(node: &Value, source_url: &Url) -> Option<DiscussionEntry> {
    let typename = node.get("__typename").and_then(Value::as_str)?;
    if !matches!(
        typename,
        "IssueComment" | "PullRequestReview" | "PullRequestReviewComment"
    ) {
        return None;
    }
    if node
        .get("isHidden")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || node.get("minimizedReason").is_some_and(|value| !value.is_null())
    {
        return None;
    }

    let body = extract_discussion_body(node, source_url)?;
    let body = normalize_multiline(&body);
    if body.is_empty() {
        return None;
    }

    Some(DiscussionEntry {
        author: node.get("author").and_then(extract_discussion_author),
        published: node
            .get("createdAt")
            .and_then(Value::as_str)
            .map(normalize_inline)
            .filter(|value| !value.is_empty()),
        body,
    })
}

fn connection_nodes(value: &Value) -> Vec<&Value> {
    if let Some(nodes) = value.get("nodes").and_then(Value::as_array) {
        return nodes.iter().collect();
    }
    if let Some(edges) = value.get("edges").and_then(Value::as_array) {
        return edges.iter().filter_map(|edge| edge.get("node")).collect();
    }
    Vec::new()
}

fn render_discussion_section(entries: &[DiscussionEntry]) -> String {
    let mut body = String::from("## Discussion");
    for entry in entries {
        body.push_str("\n\n### ");
        body.push_str(entry.author.as_deref().unwrap_or("Unknown"));
        if let Some(published) = entry.published.as_deref() {
            body.push_str("\nPublished: ");
            body.push_str(published);
        }
        body.push_str("\n\n");
        body.push_str(entry.body.trim());
    }
    strip_outer_blank_lines(&body)
}

fn extract_issue_visible_body(document: &Html, selectors: &Selectors, source_url: &Url) -> String {
    document
        .select(&selectors.issue_body)
        .next()
        .map(|element| render_element_blocks(element, Some(source_url)))
        .unwrap_or_default()
}

fn extract_pr_visible_body(document: &Html, selectors: &Selectors, source_url: &Url) -> String {
    document
        .select(&selectors.pr_body)
        .next()
        .map(|element| render_element_blocks(element, Some(source_url)))
        .unwrap_or_default()
}

fn extract_issue_visible_discussion(
    document: &Html,
    selectors: &Selectors,
    source_url: &Url,
) -> Vec<DiscussionEntry> {
    document
        .select(&selectors.issue_comment_container)
        .filter_map(|container| extract_visible_discussion_entry(container, selectors, source_url))
        .collect()
}

fn extract_pr_visible_discussion(
    document: &Html,
    selectors: &Selectors,
    source_url: &Url,
) -> Vec<DiscussionEntry> {
    let main_body_id = document.select(&selectors.pr_body).next().map(|body| body.id());
    document
        .select(&selectors.pr_discussion_container)
        .filter(|container| {
            let Some(main_body_id) = main_body_id else {
                return true;
            };
            container
                .select(&selectors.pr_body)
                .next()
                .map(|body| body.id() != main_body_id)
                .unwrap_or(true)
        })
        .filter_map(|container| extract_visible_discussion_entry(container, selectors, source_url))
        .collect()
}

fn extract_visible_discussion_entry(
    container: ElementRef<'_>,
    selectors: &Selectors,
    source_url: &Url,
) -> Option<DiscussionEntry> {
    let body = container
        .select(&selectors.visible_discussion_body)
        .next()
        .map(|element| render_element_blocks(element, Some(source_url)))
        .filter(|text| !text.trim().is_empty())?;

    Some(DiscussionEntry {
        author: container
            .select(&selectors.visible_discussion_author)
            .next()
            .map(|value| normalize_inline(&value.text().collect::<String>()))
            .filter(|value| !value.is_empty()),
        published: container
            .select(&selectors.relative_time)
            .next()
            .and_then(|value| value.value().attr("datetime"))
            .map(normalize_inline)
            .filter(|value| !value.is_empty()),
        body: normalize_multiline(&body),
    })
}

fn extract_issue_visible_author(document: &Html, selectors: &Selectors) -> Option<String> {
    document
        .select(&selectors.issue_container)
        .next()
        .and_then(|container| container.select(&selectors.issue_author).next())
        .map(|value| normalize_inline(&value.text().collect::<String>()))
        .filter(|value| !value.is_empty())
}

fn extract_pr_visible_author(document: &Html, selectors: &Selectors) -> Option<String> {
    document
        .select(&selectors.pr_author)
        .next()
        .map(|value| normalize_inline(&value.text().collect::<String>()))
        .filter(|value| !value.is_empty())
}

fn extract_visible_published(document: &Html, selectors: &Selectors) -> Option<String> {
    document
        .select(&selectors.relative_time)
        .next()
        .and_then(|value| value.value().attr("datetime"))
        .map(normalize_inline)
        .filter(|value| !value.is_empty())
}

fn embedded_payload(document: &Html, selectors: &Selectors) -> Option<Value> {
    document
        .select(&selectors.embedded_data)
        .find_map(|script| {
            let raw = script.text().collect::<String>();
            if raw.trim().is_empty() {
                None
            } else {
                serde_json::from_str::<Value>(&raw).ok()
            }
        })
}

fn classify_route(url: &str) -> Option<GithubRoute> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if !matches!(host, "github.com" | "www.github.com") {
        return None;
    }

    let segments = parsed.path_segments()?.collect::<Vec<_>>();
    if segments.len() < 2 {
        return None;
    }

    match segments.get(2).copied() {
        None => Some(GithubRoute::RepoOverview),
        Some("tree") if segments.len() >= 4 => Some(GithubRoute::Tree),
        Some("blob") if segments.len() >= 4 => Some(GithubRoute::Blob),
        Some("issues") => parse_route_number(&segments).map(|number| GithubRoute::Issue { number }),
        Some("pull") => parse_route_number(&segments).map(|number| GithubRoute::Pull { number }),
        Some("releases") => match segments.get(3).copied() {
            None => Some(GithubRoute::Releases),
            Some("latest") => Some(GithubRoute::ReleaseLatest),
            Some("tag") if segments.len() >= 5 => Some(GithubRoute::ReleaseTag),
            _ => None,
        },
        _ => None,
    }
}

fn parse_route_number(segments: &[&str]) -> Option<u64> {
    let segment = segments.get(3)?;
    segment
        .chars()
        .all(|ch| ch.is_ascii_digit())
        .then(|| segment.parse::<u64>().ok())
        .flatten()
}

fn select_text(
    section: Option<ElementRef<'_>>,
    document: &Html,
    selector: &Selector,
) -> Option<String> {
    select_element(section, document, selector)
        .map(|value| normalize_inline(&value.text().collect::<String>()))
        .filter(|value| !value.is_empty())
}

fn select_datetime(
    section: Option<ElementRef<'_>>,
    document: &Html,
    selector: &Selector,
) -> Option<String> {
    select_element(section, document, selector)
        .and_then(|value| value.value().attr("datetime"))
        .map(normalize_inline)
        .filter(|value| !value.is_empty())
}

fn select_element<'a>(
    section: Option<ElementRef<'a>>,
    document: &'a Html,
    selector: &Selector,
) -> Option<ElementRef<'a>> {
    match section {
        Some(section) => section.select(selector).next(),
        None => document.select(selector).next(),
    }
}

fn get_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn join_sections<const N: usize>(sections: [Option<String>; N]) -> String {
    sections
        .into_iter()
        .flatten()
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn normalize_multiline(input: &str) -> String {
    strip_outer_blank_lines(&input.replace("\r\n", "\n").replace('\r', "\n"))
}

fn selectors() -> Result<&'static Selectors, String> {
    SELECTORS
        .get_or_init(build_selectors)
        .as_ref()
        .map_err(Clone::clone)
}

fn build_selectors() -> Result<Selectors, String> {
    Ok(Selectors {
        embedded_data: parse_selector("script[data-target='react-app.embeddedData']")?,
        issue_container: parse_selector("[data-testid='issue-viewer-issue-container']")?,
        issue_body: parse_selector("[data-testid='issue-body-viewer'] .markdown-body")?,
        issue_author: parse_selector("a[data-testid='issue-body-header-author']")?,
        issue_comment_container: parse_selector("[data-wrapper-timeline-id] .react-issue-comment")?,
        pr_body: parse_selector(".comment-body.markdown-body")?,
        pr_author: parse_selector(".gh-header-meta .author, .timeline-comment .author")?,
        pr_discussion_container: parse_selector(".timeline-comment, .review-comment")?,
        visible_discussion_body: parse_selector(".markdown-body, .comment-body.markdown-body")?,
        visible_discussion_author: parse_selector(
            ".ActivityHeader-module__AuthorLink--iofTU, a[data-testid='avatar-link'], \
             a[data-testid='issue-body-header-author'], .author, a[href^='/'][data-hovercard-url*='/users/']",
        )?,
        relative_time: parse_selector("relative-time")?,
        release_section: parse_selector("section[aria-labelledby]")?,
        release_title: parse_selector("a[href*='/releases/tag/']")?,
        release_author: parse_selector(
            "a.color-fg-muted.wb-break-all, a.text-bold.color-fg-muted, a[href^='/apps/']",
        )?,
        release_body: parse_selector("[data-test-selector='body-content']")?,
        release_asset: parse_selector("a[href*='/releases/download/']")?,
    })
}

fn parse_selector(input: &str) -> Result<Selector, String> {
    Selector::parse(input).map_err(|_| format!("failed to parse selector `{input}`"))
}
