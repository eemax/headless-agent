use std::fs;

use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, optional_bool, optional_u64, require_string, resolve_path},
};

const DEFAULT_MAX_LINES: usize = 2000;

pub fn read_file_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "read_file",
        description: "Read a UTF-8 text file. Returns up to 2000 lines by default. Use start_line and end_line for specific ranges.",
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
    let content = fs::read_to_string(&path)
        .map_err(|err| AppError::Tool(format!("failed to read file {}: {err}", path.display())))?;

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let explicit_range =
        optional_u64(arguments, "start_line").is_some() || optional_u64(arguments, "end_line").is_some();
    let start_line = optional_u64(arguments, "start_line").unwrap_or(1) as usize;
    let end_line = optional_u64(arguments, "end_line").map(|value| value as usize);

    let (selected, effective_end, truncated) = if let Some(end_line) = end_line {
        let sel: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                let n = i + 1;
                n >= start_line && n <= end_line
            })
            .map(|(_, l)| *l)
            .collect();
        let eff_end = end_line.min(total_lines);
        (sel.join("\n"), eff_end, false)
    } else if explicit_range {
        let sel: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| i + 1 >= start_line)
            .map(|(_, l)| *l)
            .collect();
        (sel.join("\n"), total_lines, false)
    } else {
        let cap = DEFAULT_MAX_LINES;
        let trunc = total_lines > cap;
        let take = if trunc { cap } else { total_lines };
        let sel: Vec<&str> = lines[..take].to_vec();
        (sel.join("\n"), take, trunc)
    };

    let mut result = json!({
        "ok": true,
        "path": path.display().to_string(),
        "content": selected,
        "start_line": start_line,
        "end_line": effective_end,
        "total_lines": total_lines,
    });
    if truncated {
        result["truncated"] = json!(true);
        result["note"] = json!(format!(
            "File has {total_lines} lines, showing first {DEFAULT_MAX_LINES}. Use start_line/end_line to read specific ranges."
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

    let matches = content.matches(&old_text).count();
    if matches != 1 {
        return Err(AppError::Tool(format!(
            "edit_file expected exactly one match in {}, found {matches}",
            path.display()
        )));
    }

    let updated = content.replacen(&old_text, &new_text, 1);
    fs::write(&path, updated)
        .map_err(|err| AppError::Tool(format!("failed to write file {}: {err}", path.display())))?;

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
    fs::write(&path, content)
        .map_err(|err| AppError::Tool(format!("failed to write file {}: {err}", path.display())))?;
    Ok(json!({
        "ok": true,
        "path": path.display().to_string(),
        "bytes": fs::metadata(&path).map(|meta| meta.len()).unwrap_or_default(),
    }))
}
