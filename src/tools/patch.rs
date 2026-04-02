use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

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
    let _ = context.remaining_budget()?;
    let patch = require_string(arguments, "patch")?;
    let operations = parse_patch(&patch)?;
    let plan = build_execution_plan(context, operations)?;
    commit_execution_plan(context, &plan)?;

    Ok(json!({
        "ok": true,
        "changed": plan.changed,
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

#[derive(Debug)]
struct ExecutionPlan {
    writes: Vec<(PathBuf, String)>,
    deletes: Vec<PathBuf>,
    changed: Vec<String>,
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
            if index < lines.len() - 1
                && let Some(target) = lines[index].strip_prefix("*** Move to: ")
            {
                move_to = Some(target.to_string());
                index += 1;
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

fn build_execution_plan(
    context: &ToolContext<'_>,
    operations: Vec<PatchOp>,
) -> Result<ExecutionPlan, AppError> {
    let mut writes = Vec::new();
    let mut deletes = Vec::new();
    let mut changed = Vec::new();
    let mut touched_paths = HashSet::new();

    for operation in operations {
        let _ = context.remaining_budget()?;
        match operation {
            PatchOp::Add { path, lines } => {
                let path = resolve_path(context.cwd, &path);
                reserve_path(&mut touched_paths, &path)?;
                if path.exists() {
                    return Err(AppError::Tool(format!(
                        "cannot add {}; file already exists",
                        path.display()
                    )));
                }
                changed.push(path.display().to_string());
                let mut content = join_lines(&lines);
                if !content.is_empty() {
                    content.push('\n');
                }
                writes.push((path, content));
            }
            PatchOp::Delete { path } => {
                let path = resolve_path(context.cwd, &path);
                reserve_path(&mut touched_paths, &path)?;
                if !path.exists() {
                    return Err(AppError::Tool(format!(
                        "cannot delete {}; file does not exist",
                        path.display()
                    )));
                }
                changed.push(path.display().to_string());
                deletes.push(path);
            }
            PatchOp::Update {
                path,
                move_to,
                hunks,
            } => {
                let source_path = resolve_path(context.cwd, &path);
                reserve_path(&mut touched_paths, &source_path)?;
                let original = fs::read_to_string(&source_path).map_err(|err| {
                    AppError::Tool(format!("failed to read {}: {err}", source_path.display()))
                })?;
                let updated = apply_hunks(&original, &hunks)?;
                let final_path = move_to
                    .as_deref()
                    .map(|value| resolve_path(context.cwd, value))
                    .unwrap_or_else(|| source_path.clone());

                if final_path != source_path {
                    reserve_path(&mut touched_paths, &final_path)?;
                    if final_path.exists() {
                        return Err(AppError::Tool(format!(
                            "cannot move to {}; destination already exists",
                            final_path.display()
                        )));
                    }
                    changed.push(format!(
                        "{} -> {}",
                        source_path.display(),
                        final_path.display()
                    ));
                    writes.push((final_path, updated));
                    deletes.push(source_path);
                } else {
                    changed.push(source_path.display().to_string());
                    writes.push((source_path, updated));
                }
            }
        }
    }

    Ok(ExecutionPlan {
        writes,
        deletes,
        changed,
    })
}

fn commit_execution_plan(context: &ToolContext<'_>, plan: &ExecutionPlan) -> Result<(), AppError> {
    // Phase 1: Write all content to temporary files (same directory for atomic rename)
    let mut temp_files: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (path, content) in &plan.writes {
        let _ = context.remaining_budget()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = temp_path_for(path);
        if let Err(err) = fs::write(&temp_path, content) {
            cleanup_temps(&temp_files);
            return Err(err.into());
        }
        temp_files.push((temp_path, path.clone()));
    }

    // Phase 2: Rename all temp files to final paths (atomic per file on same fs)
    for (temp_path, final_path) in &temp_files {
        if let Err(err) = fs::rename(temp_path, final_path) {
            cleanup_temps(&temp_files);
            return Err(AppError::Tool(format!(
                "failed to commit patch to {}: {err}",
                final_path.display()
            )));
        }
    }

    // Phase 3: Process deletes
    for path in &plan.deletes {
        let _ = context.remaining_budget()?;
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    name.push_str(".headless_tmp");
    path.with_file_name(name)
}

fn cleanup_temps(temp_files: &[(PathBuf, PathBuf)]) {
    for (temp, _) in temp_files {
        let _ = fs::remove_file(temp);
    }
}

fn reserve_path(paths: &mut HashSet<PathBuf>, path: &Path) -> Result<(), AppError> {
    if paths.insert(path.to_path_buf()) {
        Ok(())
    } else {
        Err(AppError::Tool(format!(
            "patch touches `{}` more than once",
            path.display()
        )))
    }
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
