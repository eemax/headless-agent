use std::path::{Component, Path, PathBuf};

use globset::GlobBuilder;
use ignore::{DirEntry, WalkBuilder};
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, require_string},
};

const RESULT_LIMIT: usize = 10_000;

#[derive(Debug)]
struct GlobPlan {
    matcher_pattern: String,
    walk_root: PathBuf,
    subtree_root: PathBuf,
    match_relative_to_cwd: bool,
    max_depth: Option<usize>,
}

pub fn glob_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "glob",
        description: "Find files matching a glob pattern. Returns up to 10000 results. Searches hidden files, respects repo and global ignore files, and excludes .git.",
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
    let pattern = normalize_pattern(&require_string(arguments, "pattern")?);
    let plan = build_plan(context.cwd, &pattern);
    let matcher = GlobBuilder::new(&plan.matcher_pattern)
        .literal_separator(true)
        .build()
        .map_err(|err| AppError::Tool(format!("invalid glob pattern: {err}")))?
        .compile_matcher();

    if !plan.subtree_root.exists() {
        return Ok(json!({
            "ok": true,
            "matches": [],
            "truncated": false,
        }));
    }
    if !plan.match_relative_to_cwd && subtree_root_is_excluded(context.cwd, &plan.subtree_root) {
        return Ok(json!({
            "ok": true,
            "matches": [],
            "truncated": false,
        }));
    }

    let mut matches = Vec::new();
    let mut truncated = false;
    let subtree_root = plan.subtree_root.clone();
    let mut builder = WalkBuilder::new(&plan.walk_root);
    configure_walk_builder(&mut builder, context.cwd);
    builder.max_depth(plan.max_depth);
    builder.sort_by_file_path(|left, right| left.cmp(right));
    builder.filter_entry(move |entry| should_visit_entry(entry, &subtree_root));

    for entry in builder.build() {
        let _ = context.remaining_budget()?;
        let entry = entry.map_err(|err| AppError::Tool(format!("glob error: {err}")))?;
        let path = entry.path();
        let candidate = match_candidate_path(path, context.cwd, plan.match_relative_to_cwd);
        if !matcher.is_match(candidate) {
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

fn build_plan(cwd: &Path, pattern: &str) -> GlobPlan {
    let pattern_path = Path::new(pattern);
    let match_relative_to_cwd = !pattern_path.is_absolute();
    let base_root = if match_relative_to_cwd {
        cwd.to_path_buf()
    } else {
        absolute_base_root(pattern_path)
    };
    let subtree_root = search_root(&base_root, pattern_path);
    let remaining_components = remaining_components(pattern_path);
    let walk_root = if match_relative_to_cwd {
        base_root.clone()
    } else {
        subtree_root.clone()
    };
    let max_depth = if pattern.contains("**") {
        None
    } else if match_relative_to_cwd {
        Some(path_depth(&base_root, &subtree_root) + remaining_components)
    } else {
        Some(remaining_components)
    };

    GlobPlan {
        matcher_pattern: pattern.to_string(),
        walk_root,
        subtree_root,
        match_relative_to_cwd,
        max_depth,
    }
}

fn absolute_base_root(pattern: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for component in pattern.components() {
        root.push(component.as_os_str());
        if matches!(component, Component::RootDir) {
            break;
        }
    }
    root
}

fn search_root(base_root: &Path, pattern: &Path) -> PathBuf {
    let mut root = base_root.to_path_buf();
    for component in pattern.components() {
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::CurDir
        ) {
            continue;
        }
        let text = component.as_os_str().to_string_lossy();
        if component_has_glob_meta(&text) {
            break;
        }
        root.push(component.as_os_str());
    }
    root
}

fn component_has_glob_meta(component: &str) -> bool {
    component
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | '{'))
}

fn remaining_components(pattern: &Path) -> usize {
    let mut found_meta = false;
    let mut remaining = 0usize;

    for component in pattern.components() {
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::CurDir
        ) {
            continue;
        }
        let has_meta = component_has_glob_meta(&component.as_os_str().to_string_lossy());
        if found_meta {
            remaining += 1;
            continue;
        }
        if has_meta {
            found_meta = true;
            remaining = 1;
        }
    }

    remaining
}

fn path_depth(base: &Path, path: &Path) -> usize {
    path.strip_prefix(base)
        .map(|value| value.components().count())
        .unwrap_or(0)
}

fn match_candidate_path<'a>(path: &'a Path, cwd: &'a Path, relative: bool) -> &'a Path {
    if relative {
        path.strip_prefix(cwd).unwrap_or(path)
    } else {
        path
    }
}

fn should_visit_entry(entry: &DirEntry, subtree_root: &Path) -> bool {
    !path_has_component(entry.path(), ".git") && is_in_subtree(entry.path(), subtree_root)
}

fn configure_walk_builder(builder: &mut WalkBuilder, cwd: &Path) {
    builder.current_dir(cwd);
    builder.hidden(false);
    builder.parents(true);
    builder.ignore(true);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.require_git(false);
}

fn normalize_pattern(pattern: &str) -> String {
    let pattern_path = Path::new(pattern);
    if pattern_path.is_absolute() {
        return pattern.to_string();
    }

    let mut components = pattern_path.components().peekable();
    while matches!(components.peek(), Some(Component::CurDir)) {
        components.next();
    }

    let mut normalized = PathBuf::new();
    for component in components {
        normalized.push(component.as_os_str());
    }

    if normalized.as_os_str().is_empty() {
        ".".to_string()
    } else {
        normalized.to_string_lossy().to_string()
    }
}

fn path_has_component(path: &Path, name: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == name)
}

fn subtree_root_is_excluded(cwd: &Path, subtree_root: &Path) -> bool {
    if path_has_component(subtree_root, ".git") {
        return true;
    }
    let Some(parent) = subtree_root.parent() else {
        return false;
    };
    if parent == subtree_root {
        return false;
    }

    let target = subtree_root.to_path_buf();
    let filter_target = target.clone();
    let mut builder = WalkBuilder::new(parent);
    configure_walk_builder(&mut builder, cwd);
    builder.max_depth(Some(1));
    builder.filter_entry(move |entry| filter_target.starts_with(entry.path()));

    let mut saw_error = false;
    for entry in builder.build() {
        match entry {
            Ok(entry) if entry.path() == target => return false,
            Ok(_) => {}
            Err(_) => saw_error = true,
        }
    }
    !saw_error
}

fn is_in_subtree(path: &Path, subtree_root: &Path) -> bool {
    path.starts_with(subtree_root) || subtree_root.starts_with(path)
}
