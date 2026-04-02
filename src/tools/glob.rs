use glob::glob;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, require_string},
};

pub fn glob_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "glob",
        description: "Find files matching a glob pattern.",
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
    let pattern = require_string(arguments, "pattern")?;
    let pattern = if pattern.starts_with('/') {
        pattern
    } else {
        context.cwd.join(pattern).to_string_lossy().to_string()
    };

    let mut matches = Vec::new();
    for entry in
        glob(&pattern).map_err(|err| AppError::Tool(format!("invalid glob pattern: {err}")))?
    {
        let path = entry.map_err(|err| AppError::Tool(format!("glob error: {err}")))?;
        let display = path
            .strip_prefix(context.cwd)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        matches.push(display);
    }
    matches.sort();

    Ok(json!({
        "ok": true,
        "matches": matches,
    }))
}
