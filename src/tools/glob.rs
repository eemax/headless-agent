use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use ignore::{DirEntry, WalkBuilder};
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, require_string},
};

const RESULT_LIMIT: usize = 10_000;

pub fn glob_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "glob",
        description: "Find files matching a glob pattern. Returns up to 10000 results. Searches hidden files, respects ignore files, and excludes .git.",
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
    let pattern = resolve_pattern(context.cwd, &pattern);
    let search_root = search_root(Path::new(&pattern));
    let matcher = GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map_err(|err| AppError::Tool(format!("invalid glob pattern: {err}")))?
        .compile_matcher();

    if !search_root.exists() {
        return Ok(json!({
            "ok": true,
            "matches": [],
            "truncated": false,
        }));
    }

    let mut matches = Vec::new();
    let mut truncated = false;
    let mut builder = WalkBuilder::new(&search_root);
    builder.current_dir(context.cwd);
    builder.hidden(false);
    builder.parents(true);
    builder.ignore(true);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.require_git(false);
    builder.sort_by_file_path(|left, right| left.cmp(right));
    builder.filter_entry(not_git_entry);

    for entry in builder.build() {
        let _ = context.remaining_budget()?;
        let entry = entry.map_err(|err| AppError::Tool(format!("glob error: {err}")))?;
        let path = entry.path();
        if !matcher.is_match(path) {
            continue;
        }
        let display = path
            .strip_prefix(context.cwd)
            .unwrap_or(path)
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

fn resolve_pattern(cwd: &Path, pattern: &str) -> String {
    if Path::new(pattern).is_absolute() {
        pattern.to_string()
    } else {
        cwd.join(pattern).to_string_lossy().to_string()
    }
}

fn search_root(pattern: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for component in pattern.components() {
        let text = component.as_os_str().to_string_lossy();
        if component_has_glob_meta(&text) {
            break;
        }
        root.push(component.as_os_str());
    }

    if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    }
}

fn component_has_glob_meta(component: &str) -> bool {
    component
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | '{'))
}

fn not_git_entry(entry: &DirEntry) -> bool {
    entry.file_name().to_str() != Some(".git")
}
