use std::path::Path;

use glob::glob;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{IGNORED_DIRS, ToolContext, require_string},
};

const RESULT_LIMIT: usize = 10_000;

pub fn glob_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "glob",
        description: "Find files matching a glob pattern. Returns up to 10000 results. Skips .git, node_modules, target directories.",
        parameters: json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string" }
            },
            "required": ["pattern"]
        }),
    }
}

pub fn glob_search(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let _ = context.remaining_budget()?;
    let pattern = require_string(arguments, "pattern")?;
    let pattern = if pattern.starts_with('/') {
        pattern
    } else {
        context.cwd.join(pattern).to_string_lossy().to_string()
    };

    let mut matches = Vec::new();
    let mut truncated = false;
    for entry in
        glob(&pattern).map_err(|err| AppError::Tool(format!("invalid glob pattern: {err}")))?
    {
        let _ = context.remaining_budget()?;
        let path = entry.map_err(|err| AppError::Tool(format!("glob error: {err}")))?;
        if path_contains_ignored_segment(&path) {
            continue;
        }
        let display = path
            .strip_prefix(context.cwd)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        matches.push(display);
        if matches.len() >= RESULT_LIMIT {
            truncated = true;
            break;
        }
    }
    matches.sort();

    let mut result = json!({
        "ok": true,
        "matches": matches,
        "truncated": truncated,
    });
    if truncated {
        result["result_limit"] = json!(RESULT_LIMIT);
        result["note"] = json!(format!(
            "Result limit reached ({RESULT_LIMIT} paths). Use a more specific pattern to narrow results."
        ));
    }
    Ok(result)
}

fn path_contains_ignored_segment(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| IGNORED_DIRS.contains(&s))
            .unwrap_or(false)
    })
}
