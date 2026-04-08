use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::Path,
};

use serde_json::{Value, json};
use tempfile::NamedTempFile;

use crate::{
    error::AppError,
    tools::{ToolContext, optional_bool, require_string, resolve_path},
};

const DEFAULT_MAX_LINES: usize = 2000;
const MAX_READ_LINE_BYTES: usize = 1024 * 1024;

pub fn read_file_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "read_file",
        description: "Read a UTF-8 text file. Returns up to 2000 lines when end_line is omitted. start_line and end_line are 1-indexed positive integers.",
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "start_line": { "type": "integer" },
                "end_line": { "type": "integer" }
            },
            "required": ["path"]
        }),
    }
}

pub fn edit_file_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "edit_file",
        description: "Apply one exact text replacement inside a UTF-8 file.",
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_text": { "type": "string" },
                "new_text": { "type": "string" }
            },
            "required": ["path", "old_text", "new_text"]
        }),
    }
}

pub fn write_file_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "write_file",
        description: "Create or replace a UTF-8 text file.",
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
                "create_parents": { "type": "boolean" }
            },
            "required": ["path", "content"]
        }),
    }
}

pub fn read_file(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let _ = context.remaining_budget()?;
    let path = require_string(arguments, "path")?;
    let path = resolve_path(context.cwd, &path);
    let start_line = optional_line_number(arguments, "start_line")?.unwrap_or(1);
    let end_line = optional_line_number(arguments, "end_line")?;
    if let Some(end_line) = end_line
        && end_line < start_line
    {
        return Err(AppError::Tool(
            "tool argument `end_line` must be greater than or equal to `start_line`".to_string(),
        ));
    }
    let file = File::open(&path)
        .map_err(|err| AppError::Tool(format!("failed to read file {}: {err}", path.display())))?;
    let mut reader = BufReader::new(file);
    let mut selected = String::new();
    let mut total_lines = 0usize;
    let mut included_lines = 0usize;
    let mut truncated = false;
    let mut total_lines_known = true;
    let mut total_lines_lower_bound = None;

    while let Some(line) = read_utf8_line_limited(&mut reader, &path)? {
        let _ = context.remaining_budget()?;
        total_lines += 1;

        let include = if let Some(end_line) = end_line {
            total_lines >= start_line && total_lines <= end_line
        } else {
            total_lines >= start_line && included_lines < DEFAULT_MAX_LINES
        };
        if include {
            append_line(&mut selected, &line);
            included_lines += 1;
        }

        if let Some(end_line) = end_line {
            if total_lines > end_line {
                total_lines_known = false;
                total_lines_lower_bound = Some(total_lines);
                break;
            }
        } else if total_lines >= start_line.saturating_add(DEFAULT_MAX_LINES) {
            truncated = true;
            total_lines_known = false;
            total_lines_lower_bound = Some(total_lines);
            break;
        }
    }

    let effective_end = if let Some(end_line) = end_line {
        end_line.min(total_lines)
    } else if included_lines > 0 {
        start_line + included_lines - 1
    } else {
        total_lines
    };

    let mut result = json!({
        "ok": true,
        "path": path.display().to_string(),
        "content": selected,
        "start_line": start_line,
        "end_line": effective_end,
    });
    if total_lines_known {
        result["total_lines"] = json!(total_lines);
    } else if let Some(lower_bound) = total_lines_lower_bound {
        result["total_lines_lower_bound"] = json!(lower_bound);
    }
    if truncated {
        result["truncated"] = json!(true);
        result["note"] = json!(format!(
            "File has more than {} lines from line {} onward; showing {} lines. Use start_line/end_line to read specific ranges.",
            DEFAULT_MAX_LINES, start_line, DEFAULT_MAX_LINES
        ));
    }
    Ok(result)
}

pub fn edit_file(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let _ = context.remaining_budget()?;
    let path = require_string(arguments, "path")?;
    let old_text = require_string(arguments, "old_text")?;
    let new_text = require_string(arguments, "new_text")?;
    let path = resolve_path(context.cwd, &path);
    let content = fs::read_to_string(&path)
        .map_err(|err| AppError::Tool(format!("failed to read file {}: {err}", path.display())))?;

    if old_text.is_empty() {
        return Err(AppError::Tool(
            "tool argument `old_text` must not be empty".to_string(),
        ));
    }

    let mut match_starts = find_match_starts(&content, &old_text);
    if match_starts.len() != 1 {
        return Err(AppError::Tool(format!(
            "edit_file expected exactly one match in {}, found {}",
            path.display(),
            match_starts.len()
        )));
    }

    let start = match_starts.pop().expect("single match start");
    let end = start + old_text.len();
    let mut updated = String::with_capacity(content.len() - old_text.len() + new_text.len());
    updated.push_str(&content[..start]);
    updated.push_str(&new_text);
    updated.push_str(&content[end..]);
    write_text_atomic(&path, &updated)?;

    Ok(json!({
        "ok": true,
        "path": path.display().to_string(),
        "replaced": true,
    }))
}

pub fn write_file(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let _ = context.remaining_budget()?;
    let path = require_string(arguments, "path")?;
    let content = require_string(arguments, "content")?;
    let create_parents = optional_bool(arguments, "create_parents", false);
    let path = resolve_path(context.cwd, &path);
    ensure_parent_dir(&path, create_parents)?;
    write_text_atomic(&path, &content)?;
    Ok(json!({
        "ok": true,
        "path": path.display().to_string(),
        "bytes": fs::metadata(&path).map(|meta| meta.len()).unwrap_or_default(),
    }))
}

fn optional_line_number(arguments: &Value, key: &str) -> Result<Option<usize>, AppError> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    let Some(raw) = value.as_u64() else {
        return Err(AppError::Tool(format!(
            "tool argument `{key}` must be a positive integer"
        )));
    };
    if raw == 0 {
        return Err(AppError::Tool(format!(
            "tool argument `{key}` must be a positive integer"
        )));
    }
    Ok(Some(raw as usize))
}

fn read_utf8_line_limited<R: BufRead>(
    reader: &mut R,
    path: &Path,
) -> Result<Option<String>, AppError> {
    let mut bytes = Vec::new();

    loop {
        let available = reader.fill_buf().map_err(|err| {
            AppError::Tool(format!("failed to read file {}: {err}", path.display()))
        })?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }

        let newline_index = available.iter().position(|byte| *byte == b'\n');
        let take = newline_index
            .map(|index| index + 1)
            .unwrap_or(available.len());
        if bytes.len() + take > MAX_READ_LINE_BYTES {
            return Err(AppError::Tool(format!(
                "failed to read file {}: line exceeds {} bytes",
                path.display(),
                MAX_READ_LINE_BYTES
            )));
        }
        bytes.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline_index.is_some() {
            break;
        }
    }

    let mut line = String::from_utf8(bytes).map_err(|_| {
        AppError::Tool(format!(
            "failed to read file {}: not valid UTF-8",
            path.display()
        ))
    })?;
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(Some(line))
}

fn append_line(selected: &mut String, line: &str) {
    if !selected.is_empty() {
        selected.push('\n');
    }
    selected.push_str(line);
}

fn find_match_starts(content: &str, needle: &str) -> Vec<usize> {
    content
        .char_indices()
        .map(|(index, _)| index)
        .filter(|index| content[*index..].starts_with(needle))
        .take(2)
        .collect()
}

fn ensure_parent_dir(path: &Path, create_parents: bool) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        if create_parents {
            fs::create_dir_all(parent).map_err(|err| {
                AppError::Tool(format!(
                    "failed to create parent directories for {}: {err}",
                    path.display()
                ))
            })?;
        } else if !parent.exists() {
            return Err(AppError::Tool(format!(
                "parent directory does not exist for {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn write_text_atomic(path: &Path, content: &str) -> Result<(), AppError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let permissions = match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return Err(AppError::Tool(format!(
                "failed to write file {}: path is a directory",
                path.display()
            )));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(AppError::Tool(format!(
                "failed to write file {}: {err}",
                path.display()
            )));
        }
    };

    let mut temp = NamedTempFile::new_in(parent).map_err(|err| {
        AppError::Tool(format!(
            "failed to create temp file in {}: {err}",
            parent.display()
        ))
    })?;
    if let Some(permissions) = permissions {
        temp.as_file().set_permissions(permissions).map_err(|err| {
            AppError::Tool(format!("failed to write file {}: {err}", path.display()))
        })?;
    }
    temp.write_all(content.as_bytes())
        .map_err(|err| AppError::Tool(format!("failed to write file {}: {err}", path.display())))?;
    temp.persist(path).map_err(|err| {
        AppError::Tool(format!(
            "failed to write file {}: {}",
            path.display(),
            err.error
        ))
    })?;
    Ok(())
}
