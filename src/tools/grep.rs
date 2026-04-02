use std::fs;

use regex::Regex;
use serde_json::{Value, json};
use walkdir::{DirEntry, WalkDir};

use crate::{
    error::AppError,
    tools::{IGNORED_DIRS, ToolContext, optional_string, require_string, resolve_path},
};

const MATCH_LIMIT: usize = 1000;

pub fn grep_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "grep",
        description: "Search text files with regex semantics. Returns up to 1000 matches. Skips .git, node_modules, target directories. If truncated, narrow with a more specific pattern or search a subdirectory.",
        parameters: json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" },
                "path": { "type": "string" }
            },
            "required": ["pattern"]
        }),
    }
}

pub fn grep_search(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let _ = context.remaining_budget()?;
    let pattern = require_string(arguments, "pattern")?;
    let path = optional_string(arguments, "path").unwrap_or_else(|| ".".to_string());
    let root = resolve_path(context.cwd, &path);
    let regex = Regex::new(&pattern)
        .map_err(|err| AppError::Tool(format!("invalid regex `{pattern}`: {err}")))?;

    let mut matches = Vec::new();
    let mut truncated = false;
    let mut files_scanned: usize = 0;
    let mut last_file_scanned = String::new();
    'walk: for entry in WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| !is_ignored_dir(e))
        .filter_map(Result::ok)
    {
        let _ = context.remaining_budget()?;
        if !entry.file_type().is_file() {
            continue;
        }
        files_scanned += 1;
        last_file_scanned = entry
            .path()
            .strip_prefix(context.cwd)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .to_string();
        let raw = match fs::read(entry.path()) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let content = String::from_utf8_lossy(&raw);
        for (line_number, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(json!({
                    "path": &last_file_scanned,
                    "line": line_number + 1,
                    "text": line,
                }));
                if matches.len() >= MATCH_LIMIT {
                    truncated = true;
                    break 'walk;
                }
            }
        }
    }

    let mut result = json!({
        "ok": true,
        "matches": matches,
        "files_scanned": files_scanned,
        "truncated": truncated,
    });
    if truncated {
        result["match_limit"] = json!(MATCH_LIMIT);
        result["last_file_scanned"] = json!(last_file_scanned);
        result["note"] = json!(format!(
            "Result limit reached ({MATCH_LIMIT} matches after scanning {files_scanned} files). Narrow with a more specific pattern or search a subdirectory via the path argument."
        ));
    }
    Ok(result)
}

fn is_ignored_dir(entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    entry
        .file_name()
        .to_str()
        .map(|name| IGNORED_DIRS.contains(&name))
        .unwrap_or(false)
}
