use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use serde_json::Value;

use super::{ExtractionKind, MAX_CONTENT_CHARS, Warning, html::extract_html};

const MAX_JSON_SNIFF_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub(super) struct ExtractedContent {
    pub(super) kind: ExtractionKind,
    pub(super) content_type: Option<String>,
    pub(super) content: String,
    pub(super) warnings: Vec<Warning>,
    pub(super) truncated: bool,
    pub(super) error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SniffedKind {
    Json,
    Html,
    Text,
    Binary,
}

pub(super) fn extract_content(
    source_url: Option<&str>,
    content_type_header: Option<&str>,
    body: &[u8],
    content_length: Option<u64>,
    body_truncated: bool,
) -> ExtractedContent {
    let kind = sniff_content_kind(content_type_header, body);
    match kind {
        SniffedKind::Json => extract_json(body, body_truncated),
        SniffedKind::Html => {
            let rendered = decode_text_body(content_type_header, body, true);
            extract_html(source_url, content_type_header, &rendered, body_truncated)
        }
        SniffedKind::Text => {
            let rendered = decode_text_body(content_type_header, body, false);
            extract_text(content_type_header, &rendered, body_truncated)
        }
        SniffedKind::Binary => {
            extract_binary(content_type_header, content_length, body, body_truncated)
        }
    }
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
    rendered: &str,
    body_truncated: bool,
) -> ExtractedContent {
    let rendered = normalize_text_body(rendered);
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

fn sniff_content_kind(content_type_header: Option<&str>, body: &[u8]) -> SniffedKind {
    if let Some(content_type) = normalize_content_type(content_type_header) {
        if content_type == "application/json" || content_type.ends_with("+json") {
            return SniffedKind::Json;
        }
        if matches!(content_type.as_str(), "text/html" | "application/xhtml+xml") {
            return SniffedKind::Html;
        }
        if content_type.starts_with("text/") || is_xml_content_type(&content_type) {
            return SniffedKind::Text;
        }
        if looks_like_json(body) {
            return SniffedKind::Json;
        }
        if content_type.starts_with("application/")
            && looks_like_text(body)
            && !has_binary_signature(body)
        {
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

pub(super) fn normalize_content_type(header: Option<&str>) -> Option<String> {
    header.map(|value| {
        value
            .split(';')
            .next()
            .unwrap_or(value)
            .trim()
            .to_ascii_lowercase()
    })
}

fn decode_text_body(
    content_type_header: Option<&str>,
    body: &[u8],
    sniff_html_meta: bool,
) -> String {
    let encoding = detect_encoding(content_type_header, body, sniff_html_meta);
    if encoding == UTF_8 && std::str::from_utf8(body).is_ok() {
        return String::from_utf8_lossy(body).into_owned();
    }
    let (decoded, _, _) = encoding.decode(body);
    decoded.into_owned()
}

fn detect_encoding(
    content_type_header: Option<&str>,
    body: &[u8],
    sniff_html_meta: bool,
) -> &'static Encoding {
    if let Some(label) = charset_from_content_type(content_type_header).or_else(|| {
        sniff_html_meta
            .then(|| charset_from_html_meta(body))
            .flatten()
    }) {
        if let Some(encoding) = Encoding::for_label(label.as_bytes()) {
            return encoding;
        }
    }

    if std::str::from_utf8(body).is_ok() {
        UTF_8
    } else {
        WINDOWS_1252
    }
}

fn charset_from_content_type(header: Option<&str>) -> Option<String> {
    let header = header?;
    let charset = header.split(';').skip(1).find_map(|part| {
        let mut pieces = part.trim().splitn(2, '=');
        let key = pieces.next()?.trim();
        let value = pieces.next()?.trim();
        key.eq_ignore_ascii_case("charset")
            .then_some(value.trim_matches(|ch| matches!(ch, '"' | '\'')))
    })?;
    (!charset.is_empty()).then_some(charset.to_ascii_lowercase())
}

fn charset_from_html_meta(body: &[u8]) -> Option<String> {
    let sample = String::from_utf8_lossy(&body[..body.len().min(4096)]).to_ascii_lowercase();
    let bytes = sample.as_bytes();
    let mut search_from = 0usize;
    while let Some(start) = sample[search_from..].find("<meta") {
        let start = search_from + start;
        let end = sample[start..]
            .find('>')
            .map(|value| start + value)
            .unwrap_or(sample.len());
        let tag = &sample[start..end];
        if let Some(charset) = extract_charset_assignment(tag) {
            return Some(charset);
        }
        search_from = end.min(bytes.len());
    }
    None
}

fn extract_charset_assignment(tag: &str) -> Option<String> {
    if let Some(index) = tag.find("charset=") {
        let value = &tag[(index + "charset=".len())..];
        let charset = value
            .trim_start()
            .trim_matches(|ch: char| matches!(ch, '"' | '\'' | ' ' | '\t'))
            .split(|ch: char| matches!(ch, '"' | '\'' | ';' | ' ' | '\t' | '>'))
            .next()
            .unwrap_or("");
        if !charset.is_empty() {
            return Some(charset.to_ascii_lowercase());
        }
    }
    None
}

fn is_generic_content_type(content_type: &str) -> bool {
    matches!(
        content_type,
        "" | "application/octet-stream" | "binary/octet-stream"
    )
}

fn is_xml_content_type(content_type: &str) -> bool {
    matches!(content_type, "text/xml" | "application/xml") || content_type.ends_with("+xml")
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
    if body.len() > MAX_JSON_SNIFF_BYTES {
        return false;
    }
    serde_json::from_slice::<Value>(body).is_ok()
}

fn looks_like_text(body: &[u8]) -> bool {
    if body.is_empty() {
        return true;
    }
    if body.contains(&0) {
        return false;
    }
    if let Ok(text) = std::str::from_utf8(body) {
        return text
            .chars()
            .all(|ch| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r'));
    }
    let printable = body
        .iter()
        .filter(|byte| matches!(byte, 0x09 | 0x0A | 0x0D | 0x20..=0x7E))
        .count();
    printable * 100 / body.len() >= 85
}

fn has_binary_signature(body: &[u8]) -> bool {
    body.starts_with(b"%PDF-")
        || body.starts_with(b"\x89PNG\r\n\x1a\n")
        || body.starts_with(b"GIF87a")
        || body.starts_with(b"GIF89a")
        || body.starts_with(b"PK\x03\x04")
}

pub(super) fn normalize_inline(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
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

pub(super) fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
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

pub(super) fn warning_list(content_truncated: bool) -> Vec<Warning> {
    if content_truncated {
        vec![Warning::ContentTruncated]
    } else {
        Vec::new()
    }
}

pub(super) fn push_warning(warnings: &mut Vec<Warning>, warning: Warning) {
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}
