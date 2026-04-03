use std::sync::OnceLock;

use scraper::{ElementRef, Html, Selector};

use super::{
    ExtractionKind, MIN_CONTENT_CHARS, NOISY_TAGS, NOISY_TOKEN_SUBSTRINGS, Warning,
    content::{
        ExtractedContent, normalize_content_type, normalize_inline, push_warning, truncate_chars,
        warning_list,
    },
};

static SELECTORS: OnceLock<Result<Selectors, String>> = OnceLock::new();

#[derive(Debug)]
struct Selectors {
    title: Selector,
    h1: Selector,
    meta_description: Selector,
    meta_og_description: Selector,
    meta_twitter_description: Selector,
    meta_og_title: Selector,
    meta_twitter_title: Selector,
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
}

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
    Break,
}

#[derive(Clone)]
struct RootSelection<'a> {
    element: ElementRef<'a>,
    kind: ExtractionKind,
}

pub(super) fn extract_html(
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
    let metadata = extract_metadata(&document, selectors);

    let displayed_title = metadata
        .title
        .as_deref()
        .or(metadata.h1.as_deref())
        .map(ToOwned::to_owned);
    let mut blocks = prune_noise_blocks(collect_blocks_from_children(root.element));
    if let Some(title) = displayed_title.as_deref() {
        suppress_duplicate_title_heading(&mut blocks, title);
    }
    let body_visible_text_len = visible_text_len(&blocks);
    let body_text = render_blocks(&blocks);
    let low_signal = is_low_signal_extraction(&body_text, body_visible_text_len, html.len());

    let mut content = String::new();
    append_metadata_line(&mut content, "Title", displayed_title.as_deref());
    append_metadata_line(&mut content, "Description", metadata.description.as_deref());
    if !content.is_empty() && !body_text.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(&body_text);
    let content = strip_outer_blank_lines(&content);
    let (content, content_truncated) =
        truncate_rendered_content(&content, super::MAX_CONTENT_CHARS);

    let mut warnings = Vec::new();
    if body_visible_text_len < MIN_CONTENT_CHARS {
        push_warning(&mut warnings, Warning::LowContentYield);
    }
    if low_signal {
        push_warning(&mut warnings, Warning::LowSignalExtraction);
        if html_has_shell_markers(&html) {
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

fn extract_metadata(document: &Html, selectors: &Selectors) -> HtmlMetadata {
    let h1 = document
        .select(&selectors.h1)
        .next()
        .map(|value| normalize_inline(&value.text().collect::<String>()))
        .filter(|value| !value.is_empty());
    let title = document
        .select(&selectors.title)
        .next()
        .map(|value| normalize_inline(&value.text().collect::<String>()))
        .filter(|value| !value.is_empty())
        .or_else(|| meta_content(document, &selectors.meta_og_title))
        .or_else(|| meta_content(document, &selectors.meta_twitter_title));
    let description = meta_content(document, &selectors.meta_description)
        .or_else(|| meta_content(document, &selectors.meta_og_description))
        .or_else(|| meta_content(document, &selectors.meta_twitter_description));

    HtmlMetadata {
        title,
        h1,
        description,
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

fn collect_blocks_from_children(container: ElementRef<'_>) -> Vec<HtmlBlock> {
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
            collect_inline_tokens_from_element(&element, &mut pending_inline);
            continue;
        }

        flush_pending_inline(&mut pending_inline, &mut blocks);
        match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let text = render_inline_content(&element);
                if !text.is_empty() {
                    blocks.push(HtmlBlock::Heading {
                        level: heading_level(tag),
                        text,
                    });
                }
            }
            "p" | "figcaption" => {
                let text = render_inline_content(&element);
                if !text.is_empty() {
                    blocks.push(HtmlBlock::Paragraph(text));
                }
            }
            "ul" | "ol" => {
                let items = collect_list_items(&element);
                if !items.is_empty() {
                    blocks.push(HtmlBlock::List {
                        ordered: tag == "ol",
                        items,
                    });
                }
            }
            "blockquote" => {
                let mut quote_blocks = collect_blocks_from_children(element);
                if quote_blocks.is_empty() {
                    let text = render_inline_content(&element);
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
            "table" => blocks.extend(collect_table_blocks(&element)),
            _ => {
                if has_meaningful_block_children(&element) {
                    blocks.extend(collect_blocks_from_children(element));
                } else {
                    let text = render_inline_content(&element);
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
        "[edit]" | "toggle" | "expand description"
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

fn collect_inline_tokens_from_element(element: &ElementRef<'_>, tokens: &mut Vec<InlineToken>) {
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
        _ => {
            for child in element.children() {
                if let Some(text) = child.value().as_text() {
                    tokens.push(InlineToken::Text(text.to_string()));
                } else if let Some(child_element) = ElementRef::wrap(child) {
                    collect_inline_tokens_from_element(&child_element, tokens);
                }
            }
        }
    }
}

fn render_inline_content(element: &ElementRef<'_>) -> String {
    let mut tokens = Vec::new();
    for child in element.children() {
        if let Some(text) = child.value().as_text() {
            tokens.push(InlineToken::Text(text.to_string()));
        } else if let Some(child_element) = ElementRef::wrap(child) {
            collect_inline_tokens_from_element(&child_element, &mut tokens);
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
                output.push('`');
                output.push_str(code);
                output.push('`');
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

fn normalize_inline_code(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_meaningful_block_children(element: &ElementRef<'_>) -> bool {
    element.child_elements().any(|child| {
        if is_noisy_element(&child) || is_code_chrome_element(&child) {
            return false;
        }
        let tag = child.value().name();
        tag == "code" && is_preformatted_code(&child) || !is_inline_element(tag) && tag != "br"
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

fn collect_list_items(list: &ElementRef<'_>) -> Vec<HtmlListItem> {
    list.child_elements()
        .filter(|element| element.value().name() == "li")
        .filter_map(|item| {
            let blocks = collect_blocks_from_children(item);
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
    if matches!(tag, "button" | "style" | "svg") {
        return true;
    }
    ["class", "id"].into_iter().any(|attr| {
        [
            "copy",
            "clipboard",
            "toolbar",
            "gutter",
            "line-number",
            "rouge-gutter",
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

fn collect_table_blocks(table: &ElementRef<'_>) -> Vec<HtmlBlock> {
    let Some(rows) = collect_table_rows(table) else {
        return fallback_table_blocks(table);
    };
    if rows.is_empty() {
        return Vec::new();
    }

    let width = rows[0].cells.len();
    if width == 0 || rows.iter().any(|row| row.cells.len() != width) {
        return fallback_table_blocks(table);
    }

    let mut header_index = None;
    if let Some(index) = rows.iter().position(|row| row.in_head) {
        header_index = Some(index);
    } else if rows.first().map(|row| row.all_header).unwrap_or(false) {
        header_index = Some(0);
    }

    let Some(header_index) = header_index else {
        return fallback_table_blocks(table);
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

fn collect_table_rows(table: &ElementRef<'_>) -> Option<Vec<HtmlTableRow>> {
    let mut rows = Vec::new();

    for child in table.child_elements() {
        match child.value().name() {
            "thead" | "tbody" | "tfoot" => {
                let in_head = child.value().name() == "thead";
                for row in child
                    .child_elements()
                    .filter(|row| row.value().name() == "tr")
                {
                    rows.push(collect_table_row(&row, in_head)?);
                }
            }
            "tr" => rows.push(collect_table_row(&child, false)?),
            _ => {}
        }
    }

    Some(rows)
}

fn collect_table_row(row: &ElementRef<'_>, in_head: bool) -> Option<HtmlTableRow> {
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
        cells: cells.iter().map(render_inline_content).collect(),
        all_header: cells.iter().all(|cell| cell.value().name() == "th"),
        in_head,
    })
}

fn fallback_table_blocks(table: &ElementRef<'_>) -> Vec<HtmlBlock> {
    let mut blocks = Vec::new();
    for row in table
        .descendent_elements()
        .filter(|row| row.value().name() == "tr")
    {
        let cells = row
            .child_elements()
            .filter(|cell| matches!(cell.value().name(), "td" | "th"))
            .map(|cell| render_inline_content(&cell))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if !cells.is_empty() {
            blocks.push(HtmlBlock::Paragraph(cells.join(" | ")));
        }
    }
    blocks
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
            let mut rendered = String::from("```");
            if let Some(language) = language {
                rendered.push_str(language);
            }
            rendered.push('\n');
            rendered.push_str(code);
            rendered.push_str("\n```");
            rendered
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

fn strip_outer_blank_lines(input: &str) -> String {
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

fn truncate_rendered_content(input: &str, limit: usize) -> (String, bool) {
    let (mut output, truncated) = truncate_chars(input, limit);
    if !truncated {
        return (output, false);
    }
    if has_unclosed_code_fence(&output) {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("```");
    }
    (output, true)
}

fn has_unclosed_code_fence(input: &str) -> bool {
    input
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
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
    let lower = html.to_ascii_lowercase();
    lower.contains("__next")
        || lower.contains("id=\"root\"")
        || lower.contains("id='root'")
        || lower.contains("id=\"app\"")
        || lower.contains("id='app'")
        || lower.contains("data-reactroot")
        || lower.contains("__nuxt")
        || lower.contains("ng-version")
}
