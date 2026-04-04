use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use serde::de::IgnoredAny;
use serde_json::Value;

use super::{ExtractionKind, MAX_CONTENT_CHARS, Warning, html::extract_html};

const MAX_JSON_SNIFF_BYTES: usize = 256 * 1024;
const MAX_JSON_PRETTY_BYTES: usize = 2 * 1024 * 1024;

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
        SniffedKind::Json => extract_json(content_type_header, body, body_truncated),
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

fn strip_utf8_bom(body: &[u8]) -> &[u8] {
    body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body)
}

fn extract_json(
    content_type_header: Option<&str>,
    body: &[u8],
    body_truncated: bool,
) -> ExtractedContent {
    let content_type = normalize_content_type(content_type_header)
        .or_else(|| Some("application/json".to_string()));
    let explicit_json = content_type
        .as_deref()
        .map(|value| value == "application/json" || value.ends_with("+json"))
        .unwrap_or(false);
    let body = strip_utf8_bom(body);
    if !body_truncated
        && body.len() <= MAX_JSON_PRETTY_BYTES
        && let Ok(value) = serde_json::from_slice::<Value>(body)
    {
        let rendered = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
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

    let byte_limit = MAX_CONTENT_CHARS.saturating_mul(4);
    let sample = &body[..body.len().min(byte_limit)];
    let rendered = String::from_utf8_lossy(sample);
    let (content, content_truncated) = truncate_chars(&rendered, MAX_CONTENT_CHARS);
    let truncated = body_truncated || content_truncated || body.len() > byte_limit;
    let oversized_explicit_json_is_valid = explicit_json
        && !body_truncated
        && body.len() > MAX_JSON_PRETTY_BYTES
        && parses_as_json(body);
    let mut warnings = Vec::new();
    if truncated {
        push_warning(&mut warnings, Warning::ContentTruncated);
    }
    ExtractedContent {
        kind: ExtractionKind::Json,
        content_type,
        content,
        warnings,
        truncated,
        error: if !body_truncated
            && (body.len() <= MAX_JSON_PRETTY_BYTES
                || explicit_json && !oversized_explicit_json_is_valid)
        {
            Some("decode_error")
        } else {
            None
        },
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
    if encoding == UTF_8
        && let Ok(valid) = std::str::from_utf8(body)
    {
        return valid.to_string();
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
    }) && let Some(encoding) = Encoding::for_label(label.as_bytes())
    {
        return encoding;
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
            .split(['"', '\'', ';', ' ', '\t', '>'])
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
        || sample.contains("<html")
        || sample.contains("<body")
        || sample.contains("<main")
        || sample.contains("<article")
}

fn looks_like_json(body: &[u8]) -> bool {
    let body = strip_utf8_bom(body);
    let first = body.iter().find(|b| !b.is_ascii_whitespace());
    if !matches!(first, Some(b'{') | Some(b'[')) {
        return false;
    }
    if body.len() > MAX_JSON_SNIFF_BYTES {
        return false;
    }
    parses_as_json(body)
}

fn parses_as_json(body: &[u8]) -> bool {
    serde_json::from_slice::<IgnoredAny>(body).is_ok()
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
    let mut output = String::new();
    for word in input.split_whitespace() {
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(word);
    }
    output
}

fn normalize_text_body(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

pub(super) fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
    let mut char_indices = input.char_indices();
    if let Some((byte_offset, _)) = char_indices.nth(limit) {
        (input[..byte_offset].to_string(), true)
    } else {
        (input.to_string(), false)
    }
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
