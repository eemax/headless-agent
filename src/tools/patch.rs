use std::fs;

use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, require_string, resolve_path},
};

pub fn apply_patch_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "apply_patch",
        description: "Apply a structured multi-file patch with explicit file operations.",
        parameters: json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string" }
            },
            "required": ["patch"]
        }),
    }
}

pub fn apply_patch(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let patch = require_string(arguments, "patch")?;
    let operations = parse_patch(&patch)?;
    let mut changed = Vec::new();
    for operation in operations {
        match operation {
            PatchOp::Add { path, lines } => {
                let path = resolve_path(context.cwd, &path);
                if path.exists() {
                    return Err(AppError::Tool(format!(
                        "cannot add {}; file already exists",
                        path.display()
                    )));
                }
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, join_lines(&lines))?;
                changed.push(path.display().to_string());
            }
            PatchOp::Delete { path } => {
                let path = resolve_path(context.cwd, &path);
                if !path.exists() {
                    return Err(AppError::Tool(format!(
                        "cannot delete {}; file does not exist",
                        path.display()
                    )));
                }
                fs::remove_file(&path)?;
                changed.push(path.display().to_string());
            }
            PatchOp::Update {
                path,
                move_to,
                hunks,
            } => {
                let path = resolve_path(context.cwd, &path);
                let original = fs::read_to_string(&path).map_err(|err| {
                    AppError::Tool(format!("failed to read {}: {err}", path.display()))
                })?;
                let updated = apply_hunks(&original, &hunks)?;
                let final_path = move_to
                    .as_deref()
                    .map(|value| resolve_path(context.cwd, value))
                    .unwrap_or_else(|| path.clone());
                if let Some(parent) = final_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&final_path, updated)?;
                if let Some(move_to) = move_to {
                    if final_path != path {
                        fs::remove_file(&path)?;
                        changed.push(format!("{} -> {}", path.display(), move_to));
                    }
                } else {
                    changed.push(final_path.display().to_string());
                }
            }
        }
    }

    Ok(json!({
        "ok": true,
        "changed": changed,
    }))
}

#[derive(Debug)]
enum PatchOp {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<PatchHunk>,
    },
}

#[derive(Debug)]
struct PatchHunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
}

fn parse_patch(input: &str) -> Result<Vec<PatchOp>, AppError> {
    let lines: Vec<&str> = input.lines().collect();
    if lines.first().copied() != Some("*** Begin Patch") {
        return Err(AppError::Tool(
            "patch must start with `*** Begin Patch`".to_string(),
        ));
    }
    if lines.last().copied() != Some("*** End Patch") {
        return Err(AppError::Tool(
            "patch must end with `*** End Patch`".to_string(),
        ));
    }

    let mut operations = Vec::new();
    let mut index = 1;
    while index < lines.len() - 1 {
        let line = lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut content = Vec::new();
            while index < lines.len() - 1 && !lines[index].starts_with("*** ") {
                let added = lines[index].strip_prefix('+').ok_or_else(|| {
                    AppError::Tool("add file hunks must use `+` lines".to_string())
                })?;
                content.push(added.to_string());
                index += 1;
            }
            operations.push(PatchOp::Add {
                path: path.to_string(),
                lines: content,
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            operations.push(PatchOp::Delete {
                path: path.to_string(),
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut move_to = None;
            if index < lines.len() - 1 {
                if let Some(target) = lines[index].strip_prefix("*** Move to: ") {
                    move_to = Some(target.to_string());
                    index += 1;
                }
            }
            let mut change_lines = Vec::new();
            while index < lines.len() - 1 && !lines[index].starts_with("*** ") {
                if lines[index] != "*** End of File" {
                    change_lines.push(lines[index].to_string());
                }
                index += 1;
            }
            operations.push(PatchOp::Update {
                path: path.to_string(),
                move_to,
                hunks: build_hunks(&change_lines)?,
            });
            continue;
        }
        return Err(AppError::Tool(format!("unrecognized patch line `{line}`")));
    }
    Ok(operations)
}

fn build_hunks(lines: &[String]) -> Result<Vec<PatchHunk>, AppError> {
    let mut hunks = Vec::new();
    let mut current = Vec::new();

    for line in lines {
        if line.starts_with("@@") {
            if !current.is_empty() {
                hunks.push(change_lines_to_hunk(&current)?);
                current.clear();
            }
            continue;
        }
        current.push(line.clone());
    }

    if !current.is_empty() {
        hunks.push(change_lines_to_hunk(&current)?);
    }
    if hunks.is_empty() {
        return Err(AppError::Tool(
            "update patch did not contain any hunks".to_string(),
        ));
    }
    Ok(hunks)
}

fn change_lines_to_hunk(lines: &[String]) -> Result<PatchHunk, AppError> {
    let mut old_lines = Vec::new();
    let mut new_lines = Vec::new();
    for line in lines {
        let (prefix, text) = line.split_at(1);
        match prefix {
            " " => {
                old_lines.push(text.to_string());
                new_lines.push(text.to_string());
            }
            "-" => old_lines.push(text.to_string()),
            "+" => new_lines.push(text.to_string()),
            _ => {
                return Err(AppError::Tool(format!(
                    "invalid patch change line `{line}`"
                )));
            }
        }
    }
    Ok(PatchHunk {
        old_lines,
        new_lines,
    })
}

fn apply_hunks(original: &str, hunks: &[PatchHunk]) -> Result<String, AppError> {
    let had_trailing_newline = original.ends_with('\n');
    let original_lines: Vec<String> = original.lines().map(ToOwned::to_owned).collect();
    let mut result = Vec::new();
    let mut cursor = 0;

    for hunk in hunks {
        let start = find_subsequence(&original_lines[cursor..], &hunk.old_lines)
            .map(|offset| offset + cursor)
            .ok_or_else(|| {
                AppError::Tool("patch context did not match file contents".to_string())
            })?;
        result.extend(original_lines[cursor..start].iter().cloned());
        result.extend(hunk.new_lines.iter().cloned());
        cursor = start + hunk.old_lines.len();
    }

    result.extend(original_lines[cursor..].iter().cloned());
    let mut output = join_lines(&result);
    if had_trailing_newline && !output.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

fn find_subsequence(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn join_lines(lines: &[String]) -> String {
    lines.join("\n")
}
