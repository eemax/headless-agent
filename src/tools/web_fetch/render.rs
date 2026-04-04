use scraper::{ElementRef, Html};
use url::Url;

use super::{NOISY_TAGS, NOISY_TOKEN_SUBSTRINGS, content::normalize_inline};

#[derive(Debug, Clone, PartialEq, Eq)]
enum HtmlBlock {
    Heading {
        level: usize,
        text: String,
    },
    Paragraph(String),
    List {
        ordered: bool,
        items: Vec<HtmlListItem>,
    },
    CodeFence {
        language: Option<String>,
        code: String,
    },
    Blockquote(Vec<HtmlBlock>),
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HtmlListItem {
    blocks: Vec<HtmlBlock>,
}

#[derive(Debug, Clone)]
struct HtmlTableRow {
    cells: Vec<String>,
    all_header: bool,
    in_head: bool,
}

#[derive(Debug, Clone)]
enum InlineToken {
    Text(String),
    Code(String),
    Link { text: String, url: String },
    Break,
}

#[derive(Debug, Clone)]
pub(super) struct RenderedBody {
    pub(super) text: String,
    pub(super) visible_len: usize,
}

#[derive(Clone, Copy)]
struct RenderContext<'a> {
    base_url: Option<&'a Url>,
}

pub(super) fn render_root(
    element: ElementRef<'_>,
    duplicate_title: Option<&str>,
    base_url: Option<&Url>,
) -> RenderedBody {
    let mut blocks = prune_noise_blocks(collect_blocks_from_children(
        element,
        RenderContext { base_url },
    ));
    if let Some(title) = duplicate_title {
        suppress_duplicate_title_heading(&mut blocks, title);
    }
    let visible_len = visible_text_len(&blocks);
    let text = render_blocks(&blocks);
    RenderedBody { text, visible_len }
}

pub(super) fn render_element_blocks(element: ElementRef<'_>, base_url: Option<&Url>) -> String {
    let blocks = prune_noise_blocks(collect_blocks_from_children(
        element,
        RenderContext { base_url },
    ));
    render_blocks(&blocks)
}

pub(super) fn render_html_fragment(html: &str, base_url: Option<&Url>) -> String {
    let document = Html::parse_fragment(html);
    render_element_blocks(document.root_element(), base_url)
}

pub(super) fn render_markdown_inline_code(code: &str) -> String {
    let delimiter = backtick_delimiter(code, 1);
    let needs_padding = code.starts_with('`')
        || code.ends_with('`')
        || code.starts_with(' ')
        || code.ends_with(' ');
    if needs_padding {
        format!("{delimiter} {code} {delimiter}")
    } else {
        format!("{delimiter}{code}{delimiter}")
    }
}

fn render_markdown_link(text: &str, url: &str) -> String {
    format!("[{}](<{url}>)", escape_markdown_link_text(text))
}

fn escape_markdown_link_text(input: &str) -> String {
    let mut output = String::new();
    for ch in input.chars() {
        if matches!(ch, '\\' | '[' | ']') {
            output.push('\\');
        }
        output.push(ch);
    }
    output
}

pub(super) fn render_markdown_code_block(language: Option<&str>, code: &str) -> String {
    let delimiter = backtick_delimiter(code, 3);
    let mut rendered = delimiter.clone();
    if let Some(language) = language {
        rendered.push_str(language);
    }
    rendered.push('\n');
    rendered.push_str(code);
    if !code.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str(&delimiter);
    rendered
}

pub(super) fn unmatched_markdown_code_fence(input: &str) -> Option<String> {
    let mut open_fence: Option<String> = None;
    for line in input.lines() {
        let trimmed = line.trim_start();
        if let Some(fence) = &open_fence {
            if is_closing_code_fence(trimmed, fence) {
                open_fence = None;
            }
            continue;
        }

        if let Some(fence) = opening_code_fence(trimmed) {
            open_fence = Some(fence.to_string());
        }
    }
    open_fence
}

pub(super) fn strip_outer_blank_lines(input: &str) -> String {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

fn collect_blocks_from_children(
    container: ElementRef<'_>,
    context: RenderContext<'_>,
) -> Vec<HtmlBlock> {
    let mut blocks = Vec::new();
    let mut pending_inline = Vec::new();

    for child in container.children() {
        if let Some(text) = child.value().as_text() {
            pending_inline.push(InlineToken::Text(text.to_string()));
            continue;
        }

        let Some(element) = ElementRef::wrap(child) else {
            continue;
        };
        if is_noisy_element(&element) || is_code_chrome_element(&element) {
            continue;
        }

        let tag = element.value().name();
        if tag == "br" {
            pending_inline.push(InlineToken::Break);
            continue;
        }
        if tag == "code" && is_preformatted_code(&element) {
            flush_pending_inline(&mut pending_inline, &mut blocks);
            if let Some(block) = collect_code_block(&element) {
                blocks.push(block);
            }
            continue;
        }
        if is_inline_element(tag) {
            collect_inline_tokens_from_element(&element, &mut pending_inline, context);
            continue;
        }

        flush_pending_inline(&mut pending_inline, &mut blocks);
        match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let text = render_inline_content(&element, context);
                if !text.is_empty() {
                    blocks.push(HtmlBlock::Heading {
                        level: heading_level(tag),
                        text,
                    });
                }
            }
            "p" | "figcaption" => {
                let text = render_inline_content(&element, context);
                if !text.is_empty() {
                    blocks.push(HtmlBlock::Paragraph(text));
                }
            }
            "ul" | "ol" => {
                let items = collect_list_items(&element, context);
                if !items.is_empty() {
                    blocks.push(HtmlBlock::List {
                        ordered: tag == "ol",
                        items,
                    });
                }
            }
            "blockquote" => {
                let mut quote_blocks = collect_blocks_from_children(element, context);
                if quote_blocks.is_empty() {
                    let text = render_inline_content(&element, context);
                    if !text.is_empty() {
                        quote_blocks.push(HtmlBlock::Paragraph(text));
                    }
                }
                if !quote_blocks.is_empty() {
                    blocks.push(HtmlBlock::Blockquote(quote_blocks));
                }
            }
            "pre" => {
                if let Some(block) = collect_code_block(&element) {
                    blocks.push(block);
                }
            }
            "table" => blocks.extend(collect_table_blocks(&element, context)),
            _ => {
                if has_meaningful_block_children(&element) {
                    blocks.extend(collect_blocks_from_children(element, context));
                } else {
                    let text = render_inline_content(&element, context);
                    if !text.is_empty() {
                        blocks.push(HtmlBlock::Paragraph(text));
                    }
                }
            }
        }
    }

    flush_pending_inline(&mut pending_inline, &mut blocks);
    blocks
}

fn prune_noise_blocks(blocks: Vec<HtmlBlock>) -> Vec<HtmlBlock> {
    blocks.into_iter().filter_map(prune_noise_block).collect()
}

fn prune_noise_block(block: HtmlBlock) -> Option<HtmlBlock> {
    match block {
        HtmlBlock::Heading { level, text } => {
            (!is_ui_crumb(&text)).then_some(HtmlBlock::Heading { level, text })
        }
        HtmlBlock::Paragraph(text) => (!is_ui_crumb(&text)).then_some(HtmlBlock::Paragraph(text)),
        HtmlBlock::List { ordered, items } => {
            let items = items
                .into_iter()
                .filter_map(prune_noise_item)
                .collect::<Vec<_>>();
            (!items.is_empty()).then_some(HtmlBlock::List { ordered, items })
        }
        HtmlBlock::Blockquote(blocks) => {
            let blocks = prune_noise_blocks(blocks);
            (!blocks.is_empty()).then_some(HtmlBlock::Blockquote(blocks))
        }
        other => Some(other),
    }
}

fn prune_noise_item(item: HtmlListItem) -> Option<HtmlListItem> {
    let blocks = prune_noise_blocks(item.blocks);
    (!blocks.is_empty()).then_some(HtmlListItem { blocks })
}

fn is_ui_crumb(text: &str) -> bool {
    let normalized = normalize_inline(text).to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "[edit]" | "toggle" | "expand description" | "source"
    )
}

fn flush_pending_inline(tokens: &mut Vec<InlineToken>, blocks: &mut Vec<HtmlBlock>) {
    if tokens.is_empty() {
        return;
    }
    let rendered = render_inline_tokens(tokens);
    tokens.clear();
    if !rendered.is_empty() {
        blocks.push(HtmlBlock::Paragraph(rendered));
    }
}

fn collect_inline_tokens_from_element(
    element: &ElementRef<'_>,
    tokens: &mut Vec<InlineToken>,
    context: RenderContext<'_>,
) {
    if is_noisy_element(element) || is_code_chrome_element(element) {
        return;
    }

    let tag = element.value().name();
    match tag {
        "br" => tokens.push(InlineToken::Break),
        "code" if !is_preformatted_code(element) => {
            let code = normalize_inline_code(&extract_code_text(element));
            if !code.is_empty() {
                tokens.push(InlineToken::Code(code));
            }
        }
        "a" => {
            let text = normalize_inline(&render_inline_content(element, context));
            if text.is_empty() {
                return;
            }
            if let Some(url) = resolve_anchor_href(element, context.base_url) {
                tokens.push(InlineToken::Link { text, url });
            } else {
                tokens.push(InlineToken::Text(text));
            }
        }
        "img" => {
            if let Some(alt) = element.value().attr("alt") {
                let text = normalize_inline(alt);
                if !text.is_empty() {
                    tokens.push(InlineToken::Text(text));
                }
            }
        }
        _ => {
            for child in element.children() {
                if let Some(text) = child.value().as_text() {
                    tokens.push(InlineToken::Text(text.to_string()));
                } else if let Some(child_element) = ElementRef::wrap(child) {
                    collect_inline_tokens_from_element(&child_element, tokens, context);
                }
            }
        }
    }
}

fn render_inline_content(element: &ElementRef<'_>, context: RenderContext<'_>) -> String {
    let mut tokens = Vec::new();
    for child in element.children() {
        if let Some(text) = child.value().as_text() {
            tokens.push(InlineToken::Text(text.to_string()));
        } else if let Some(child_element) = ElementRef::wrap(child) {
            collect_inline_tokens_from_element(&child_element, &mut tokens, context);
        }
    }
    render_inline_tokens(&tokens)
}

fn render_inline_tokens(tokens: &[InlineToken]) -> String {
    let mut output = String::new();
    let mut pending_space = false;
    for token in tokens {
        match token {
            InlineToken::Text(raw) => {
                for ch in raw.chars() {
                    match ch {
                        '\n' | '\r' | '\t' | ' ' | '\u{00A0}' => pending_space = true,
                        _ => {
                            if pending_space && !output.is_empty() && !output.ends_with('\n') {
                                output.push(' ');
                            }
                            output.push(ch);
                            pending_space = false;
                        }
                    }
                }
            }
            InlineToken::Code(code) => {
                if pending_space && !output.is_empty() && !output.ends_with('\n') {
                    output.push(' ');
                }
                output.push_str(&render_markdown_inline_code(code));
                pending_space = false;
            }
            InlineToken::Link { text, url } => {
                if pending_space && !output.is_empty() && !output.ends_with('\n') {
                    output.push(' ');
                }
                output.push_str(&render_markdown_link(text, url));
                pending_space = false;
            }
            InlineToken::Break => {
                trim_trailing_spaces(&mut output);
                if !output.ends_with('\n') {
                    output.push('\n');
                }
                pending_space = false;
            }
        }
    }

    strip_outer_blank_lines(&output)
}

fn trim_trailing_spaces(value: &mut String) {
    while value.ends_with(' ') {
        value.pop();
    }
}

fn resolve_anchor_href(element: &ElementRef<'_>, base_url: Option<&Url>) -> Option<String> {
    let href = element.value().attr("href")?.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }

    if let Ok(url) = Url::parse(href) {
        return matches!(url.scheme(), "http" | "https").then(|| url.to_string());
    }

    let base_url = base_url?;
    let resolved = base_url.join(href).ok()?;
    matches!(resolved.scheme(), "http" | "https").then(|| resolved.to_string())
}

fn normalize_inline_code(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn backtick_delimiter(input: &str, minimum_len: usize) -> String {
    "`".repeat(longest_backtick_run(input).max(minimum_len.saturating_sub(1)) + 1)
}

fn longest_backtick_run(input: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for ch in input.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn opening_code_fence(line: &str) -> Option<&str> {
    let count = line.chars().take_while(|ch| *ch == '`').count();
    (count >= 3).then_some(&line[..count])
}

fn is_closing_code_fence(line: &str, fence: &str) -> bool {
    let trimmed = line.trim();
    let count = trimmed.chars().take_while(|ch| *ch == '`').count();
    count >= fence.len() && trimmed.chars().all(|ch| ch == '`')
}

fn has_meaningful_block_children(element: &ElementRef<'_>) -> bool {
    element.child_elements().any(|child| {
        if is_noisy_element(&child) || is_code_chrome_element(&child) {
            return false;
        }
        let tag = child.value().name();
        (tag == "code" && is_preformatted_code(&child)) || (!is_inline_element(tag) && tag != "br")
    })
}

fn is_inline_element(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "abbr"
            | "b"
            | "cite"
            | "data"
            | "del"
            | "em"
            | "i"
            | "ins"
            | "kbd"
            | "label"
            | "mark"
            | "q"
            | "s"
            | "samp"
            | "small"
            | "span"
            | "strong"
            | "sub"
            | "sup"
            | "time"
            | "u"
            | "var"
    )
}

fn heading_level(tag: &str) -> usize {
    tag.strip_prefix('h')
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 6)
}

fn collect_list_items(list: &ElementRef<'_>, context: RenderContext<'_>) -> Vec<HtmlListItem> {
    list.child_elements()
        .filter(|element| element.value().name() == "li")
        .filter_map(|item| {
            let blocks = collect_blocks_from_children(item, context);
            if blocks.is_empty() {
                None
            } else {
                Some(HtmlListItem { blocks })
            }
        })
        .collect()
}

fn collect_code_block(element: &ElementRef<'_>) -> Option<HtmlBlock> {
    let code = extract_code_text(element);
    if code.is_empty() {
        return None;
    }
    Some(HtmlBlock::CodeFence {
        language: detect_code_language(element),
        code,
    })
}

fn extract_code_text(element: &ElementRef<'_>) -> String {
    let mut code = if let Some(lines) = extract_code_lines(element) {
        lines.join("\n")
    } else {
        extract_code_text_fragment(element)
    };
    code = code
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\u{00A0}', " ");
    strip_code_surrounding_blank_lines(&code)
}

fn strip_code_surrounding_blank_lines(input: &str) -> String {
    let lines = input.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

fn extract_code_lines(element: &ElementRef<'_>) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    for descendant in element.descendent_elements() {
        if !is_code_line_element(&descendant) || has_code_line_ancestor(&descendant) {
            continue;
        }
        let line = extract_code_text_fragment(&descendant)
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .trim_end_matches('\n')
            .to_string();
        lines.push(line);
    }

    (!lines.is_empty()).then_some(lines)
}

fn has_code_line_ancestor(element: &ElementRef<'_>) -> bool {
    for ancestor in element.ancestors().skip(1).filter_map(ElementRef::wrap) {
        if is_code_line_element(&ancestor) {
            return true;
        }
    }
    false
}

fn is_code_line_element(element: &ElementRef<'_>) -> bool {
    if element.value().attr("data-line").is_some() {
        return true;
    }
    if element_attr_contains(element, "class", "line-number")
        || element_attr_contains(element, "id", "line-number")
        || element_attr_contains(element, "class", "gutter")
        || element_attr_contains(element, "id", "gutter")
        || element_has_token(element, "lnt")
        || element_has_token(element, "linenos")
    {
        return false;
    }
    element_has_token(element, "line")
}

fn extract_code_text_fragment(element: &ElementRef<'_>) -> String {
    let mut output = String::new();
    for child in element.children() {
        if let Some(text) = child.value().as_text() {
            output.push_str(text.as_ref());
            continue;
        }

        let Some(child_element) = ElementRef::wrap(child) else {
            continue;
        };
        if is_noisy_element(&child_element) || is_code_chrome_element(&child_element) {
            continue;
        }

        match child_element.value().name() {
            "br" => output.push('\n'),
            _ => output.push_str(&extract_code_text_fragment(&child_element)),
        }
    }
    output
}

fn is_preformatted_code(element: &ElementRef<'_>) -> bool {
    element.value().name() == "code"
        && element
            .value()
            .attr("style")
            .map(|value| {
                let lower = value.to_ascii_lowercase();
                lower.contains("white-space") && lower.contains("pre")
            })
            .unwrap_or(false)
}

fn detect_code_language(element: &ElementRef<'_>) -> Option<String> {
    for candidate in element.descendent_elements() {
        if let Some(language) = candidate
            .value()
            .attr("data-lang")
            .or(candidate.value().attr("data-language"))
            .or(candidate.value().attr("language"))
            .and_then(normalize_language_hint)
        {
            return Some(language);
        }
        if let Some(class_name) = candidate.value().attr("class") {
            for part in class_name.split_whitespace() {
                let lower = part.to_ascii_lowercase();
                if let Some(language) = lower
                    .strip_prefix("language-")
                    .or_else(|| lower.strip_prefix("lang-"))
                    .and_then(normalize_language_hint)
                {
                    return Some(language);
                }
            }
        }
    }
    None
}

fn normalize_language_hint(value: &str) -> Option<String> {
    let trimmed = value
        .trim_matches(|ch: char| {
            !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '#' | '-' | '_'))
        })
        .to_ascii_lowercase();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn is_code_chrome_element(element: &ElementRef<'_>) -> bool {
    let tag = element.value().name();
    if matches!(tag, "button" | "style" | "svg" | "clipboard-copy") {
        return true;
    }
    ["class", "id"].into_iter().any(|attr| {
        [
            "toolbar",
            "gutter",
            "line-number",
            "rouge-gutter",
            "copy-button",
            "clipboard-button",
        ]
        .iter()
        .any(|needle| element_attr_contains(element, attr, needle))
    }) || element_has_token(element, "lnt")
}

fn element_attr_contains(element: &ElementRef<'_>, attr: &str, needle: &str) -> bool {
    element
        .value()
        .attr(attr)
        .map(|value| value.to_ascii_lowercase().contains(needle))
        .unwrap_or(false)
}

fn element_has_token(element: &ElementRef<'_>, needle: &str) -> bool {
    ["class", "id"].into_iter().any(|attr| {
        element
            .value()
            .attr(attr)
            .map(|value| {
                value
                    .split(|ch: char| !ch.is_ascii_alphanumeric())
                    .filter(|token| !token.is_empty())
                    .any(|token| token.eq_ignore_ascii_case(needle))
            })
            .unwrap_or(false)
    })
}

fn collect_table_blocks(table: &ElementRef<'_>, context: RenderContext<'_>) -> Vec<HtmlBlock> {
    let Some(rows) = collect_table_rows(table, context) else {
        return fallback_table_blocks(table, context);
    };
    if rows.is_empty() {
        return Vec::new();
    }

    let width = rows[0].cells.len();
    if width == 0 || rows.iter().any(|row| row.cells.len() != width) {
        return fallback_table_blocks(table, context);
    }

    let mut header_index = None;
    if let Some(index) = rows.iter().position(|row| row.in_head) {
        header_index = Some(index);
    } else if rows.first().map(|row| row.all_header).unwrap_or(false) {
        header_index = Some(0);
    }

    let Some(header_index) = header_index else {
        return fallback_table_blocks(table, context);
    };

    let headers = rows[header_index].cells.clone();
    let body_rows = rows
        .into_iter()
        .enumerate()
        .filter_map(|(index, row)| (index != header_index).then_some(row.cells))
        .collect::<Vec<_>>();

    vec![HtmlBlock::Table {
        headers,
        rows: body_rows,
    }]
}

fn collect_table_rows(
    table: &ElementRef<'_>,
    context: RenderContext<'_>,
) -> Option<Vec<HtmlTableRow>> {
    let mut rows = Vec::new();

    for child in table.child_elements() {
        match child.value().name() {
            "thead" | "tbody" | "tfoot" => {
                let in_head = child.value().name() == "thead";
                for row in child
                    .child_elements()
                    .filter(|row| row.value().name() == "tr")
                {
                    rows.push(collect_table_row(&row, in_head, context)?);
                }
            }
            "tr" => rows.push(collect_table_row(&child, false, context)?),
            _ => {}
        }
    }

    Some(rows)
}

fn collect_table_row(
    row: &ElementRef<'_>,
    in_head: bool,
    context: RenderContext<'_>,
) -> Option<HtmlTableRow> {
    let cells = row
        .child_elements()
        .filter(|cell| matches!(cell.value().name(), "td" | "th"))
        .collect::<Vec<_>>();
    if cells.is_empty() {
        return None;
    }
    if cells.iter().any(|cell| {
        cell.value().attr("rowspan").is_some() || cell.value().attr("colspan").is_some()
    }) {
        return None;
    }

    Some(HtmlTableRow {
        cells: cells
            .iter()
            .map(|cell| render_inline_content(cell, context))
            .collect(),
        all_header: cells.iter().all(|cell| cell.value().name() == "th"),
        in_head,
    })
}

fn fallback_table_blocks(table: &ElementRef<'_>, context: RenderContext<'_>) -> Vec<HtmlBlock> {
    collect_blocks_from_children(*table, context)
}

fn suppress_duplicate_title_heading(blocks: &mut Vec<HtmlBlock>, title: &str) {
    let Some(HtmlBlock::Heading { text, .. }) = blocks.first() else {
        return;
    };
    if normalize_title_match(text) == normalize_title_match(title) {
        blocks.remove(0);
    }
}

fn normalize_title_match(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn visible_text_len(blocks: &[HtmlBlock]) -> usize {
    blocks.iter().map(visible_text_len_block).sum()
}

fn visible_text_len_block(block: &HtmlBlock) -> usize {
    match block {
        HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph(text) => text.chars().count(),
        HtmlBlock::List { items, .. } => items
            .iter()
            .map(|item| visible_text_len(&item.blocks))
            .sum(),
        HtmlBlock::CodeFence { code, .. } => code.chars().count(),
        HtmlBlock::Blockquote(blocks) => visible_text_len(blocks),
        HtmlBlock::Table { headers, rows } => {
            headers
                .iter()
                .map(|cell| cell.chars().count())
                .sum::<usize>()
                + rows
                    .iter()
                    .flat_map(|row| row.iter())
                    .map(|cell| cell.chars().count())
                    .sum::<usize>()
        }
    }
}

fn render_blocks(blocks: &[HtmlBlock]) -> String {
    let parts = blocks
        .iter()
        .map(render_block)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    strip_outer_blank_lines(&parts.join("\n\n"))
}

fn render_block(block: &HtmlBlock) -> String {
    match block {
        HtmlBlock::Heading { level, text } => format!("{} {}", "#".repeat(*level), text),
        HtmlBlock::Paragraph(text) => text.clone(),
        HtmlBlock::List { ordered, items } => render_list(*ordered, items, 0),
        HtmlBlock::CodeFence { language, code } => {
            render_markdown_code_block(language.as_deref(), code)
        }
        HtmlBlock::Blockquote(blocks) => {
            let rendered = render_blocks(blocks);
            rendered
                .lines()
                .map(|line| {
                    if line.is_empty() {
                        ">".to_string()
                    } else {
                        format!("> {line}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        HtmlBlock::Table { headers, rows } => render_table(headers, rows),
    }
}

fn render_list(ordered: bool, items: &[HtmlListItem], depth: usize) -> String {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| render_list_item(ordered, index, item, depth))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_list_item(ordered: bool, index: usize, item: &HtmlListItem, depth: usize) -> String {
    let marker = if ordered {
        format!("{}. ", index + 1)
    } else {
        "- ".to_string()
    };
    let prefix = format!("{}{}", " ".repeat(depth * 4), marker);
    let continuation = " ".repeat((depth + 1) * 4);
    let mut lines = Vec::new();
    let mut started = false;

    for block in &item.blocks {
        match block {
            HtmlBlock::List { ordered, items } => {
                if !started {
                    lines.push(prefix.trim_end().to_string());
                    started = true;
                }
                lines.extend(
                    render_list(*ordered, items, depth + 1)
                        .lines()
                        .map(ToOwned::to_owned),
                );
            }
            _ => {
                let rendered = render_block(block);
                if rendered.is_empty() {
                    continue;
                }
                let block_lines = rendered.lines().collect::<Vec<_>>();
                if block_lines.is_empty() {
                    continue;
                }
                if !started {
                    lines.push(format!("{prefix}{}", block_lines[0]));
                    for line in &block_lines[1..] {
                        lines.push(format!("{continuation}{line}"));
                    }
                    started = true;
                } else {
                    for line in block_lines {
                        lines.push(format!("{continuation}{line}"));
                    }
                }
            }
        }
    }

    if !started {
        return prefix.trim_end().to_string();
    }
    lines.join("\n")
}

fn render_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut lines = Vec::new();
    lines.push(render_table_row(headers));
    lines.push(format!(
        "| {} |",
        headers
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    for row in rows {
        lines.push(render_table_row(row));
    }
    lines.join("\n")
}

fn render_table_row(cells: &[String]) -> String {
    format!(
        "| {} |",
        cells
            .iter()
            .map(|cell| cell.replace('\n', " ").replace('|', "\\|"))
            .collect::<Vec<_>>()
            .join(" | ")
    )
}

fn is_noisy_element(element: &ElementRef<'_>) -> bool {
    let name = element.value().name();
    if NOISY_TAGS.contains(&name) {
        return true;
    }

    if element.value().attr("hidden").is_some() {
        return true;
    }

    if let Some(value) = element.value().attr("aria-hidden")
        && value.eq_ignore_ascii_case("true")
    {
        return true;
    }

    if let Some(value) = element.value().attr("role") {
        let value = value.to_ascii_lowercase();
        if matches!(value.as_str(), "navigation" | "banner" | "complementary") {
            return true;
        }
    }

    if let Some(value) = element.value().attr("style")
        && style_hides_element(value)
    {
        return true;
    }

    if has_hidden_utility_class(element) {
        return true;
    }

    for attr in ["class", "id"] {
        if let Some(value) = element.value().attr(attr)
            && has_noisy_attribute_token(value)
        {
            return true;
        }
    }

    false
}

fn has_hidden_utility_class(element: &ElementRef<'_>) -> bool {
    element
        .value()
        .attr("class")
        .map(|value| {
            value.split_whitespace().any(|token| {
                token.eq_ignore_ascii_case("hidden")
                    || token.eq_ignore_ascii_case("invisible")
                    || token.to_ascii_lowercase().ends_with(":hidden")
                    || token.to_ascii_lowercase().ends_with(":invisible")
            })
        })
        .unwrap_or(false)
}

fn style_hides_element(style: &str) -> bool {
    style.split(';').any(|declaration| {
        let mut parts = declaration.splitn(2, ':');
        let property = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let value = parts
            .next()
            .unwrap_or("")
            .chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        matches!(
            (property.as_str(), value.as_str()),
            ("display", "none")
                | ("display", "none!important")
                | ("visibility", "hidden")
                | ("visibility", "hidden!important")
                | ("opacity", "0")
                | ("opacity", "0!important")
        )
    })
}

fn has_noisy_attribute_token(value: &str) -> bool {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .any(token_matches_noisy_pattern)
}

fn token_matches_noisy_pattern(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    NOISY_TOKEN_SUBSTRINGS.iter().any(|pattern| {
        if lower == *pattern {
            return true;
        }

        if *pattern == "share" {
            return lower.len() > pattern.len() && lower.ends_with(pattern);
        }

        (lower.len() > pattern.len() && lower.starts_with(pattern))
            || (lower.len() > pattern.len() && lower.ends_with(pattern))
    })
}
