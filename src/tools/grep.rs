use std::fs;

use regex::Regex;
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::{
    error::AppError,
    tools::{ToolContext, optional_string, require_string, resolve_path},
};

pub fn grep_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "grep",
        description: "Search text files with regex semantics.",
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
    let pattern = require_string(arguments, "pattern")?;
    let path = optional_string(arguments, "path").unwrap_or_else(|| ".".to_string());
    let root = resolve_path(context.cwd, &path);
    let regex = Regex::new(&pattern)
        .map_err(|err| AppError::Tool(format!("invalid regex `{pattern}`: {err}")))?;

    let mut matches = Vec::new();
    for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let raw = match fs::read(entry.path()) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let content = String::from_utf8_lossy(&raw);
        for (line_number, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                let file_path = entry
                    .path()
                    .strip_prefix(context.cwd)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .to_string();
                matches.push(json!({
                    "path": file_path,
                    "line": line_number + 1,
                    "text": line,
                }));
            }
        }
    }

    Ok(json!({
        "ok": true,
        "matches": matches,
    }))
}
