use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use tempfile::NamedTempFile;
use ulid::Ulid;

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
    backup_targets: Vec<PathBuf>,
    changed: Vec<String>,
}

#[derive(Debug)]
struct StagedWrite {
    temp: NamedTempFile,
    final_path: PathBuf,
}

#[derive(Debug)]
struct BackupEntry {
    original_path: PathBuf,
    backup_path: PathBuf,
}

#[derive(Debug)]
struct PreparedExecution {
    created_dirs: Vec<PathBuf>,
    staged_writes: Vec<StagedWrite>,
    backups: Vec<BackupEntry>,
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
    let mut backup_targets = Vec::new();
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
                backup_targets.push(path);
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
                    backup_targets.push(source_path);
                } else {
                    changed.push(source_path.display().to_string());
                    writes.push((source_path.clone(), updated));
                    backup_targets.push(source_path);
                }
            }
        }
    }

    Ok(ExecutionPlan {
        writes,
        backup_targets,
        changed,
    })
}

fn commit_execution_plan(context: &ToolContext<'_>, plan: &ExecutionPlan) -> Result<(), AppError> {
    let prepared = prepare_execution_plan(context, plan)?;
    commit_prepared_execution(prepared)
}

fn prepare_execution_plan(
    context: &ToolContext<'_>,
    plan: &ExecutionPlan,
) -> Result<PreparedExecution, AppError> {
    // Phase 1: Stage content into random-named temp files in the destination directories.
    let mut created_dirs = Vec::new();
    let mut created_dir_set = HashSet::new();
    let mut staged_writes = Vec::new();
    let mut backups = Vec::new();
    let staged = (|| -> Result<(), AppError> {
        for (path, content) in &plan.writes {
            let _ = context.remaining_budget()?;
            let parent = path.parent().unwrap_or(Path::new("."));
            ensure_parent_dirs(parent, &mut created_dirs, &mut created_dir_set)?;
            let mut temp = tempfile::Builder::new()
                .prefix(".headless_")
                .tempfile_in(parent)
                .map_err(|err| {
                    AppError::Tool(format!(
                        "failed to create temp file in {}: {err}",
                        parent.display()
                    ))
                })?;
            std::io::Write::write_all(&mut temp, content.as_bytes())?;
            staged_writes.push(StagedWrite {
                temp,
                final_path: path.clone(),
            });
        }

        for path in &plan.backup_targets {
            let _ = context.remaining_budget()?;
            backups.push(BackupEntry {
                original_path: path.clone(),
                backup_path: unique_backup_path(path)?,
            });
        }
        Ok(())
    })();
    if let Err(error) = staged {
        drop(staged_writes);
        cleanup_created_dirs(&created_dirs);
        return Err(error);
    }

    Ok(PreparedExecution {
        created_dirs,
        staged_writes,
        backups,
    })
}

fn commit_prepared_execution(prepared: PreparedExecution) -> Result<(), AppError> {
    let PreparedExecution {
        created_dirs,
        staged_writes,
        backups,
    } = prepared;

    // Once commit starts, do not consult the run budget again. We either finish
    // atomically or roll back to the original state.
    let mut moved_backups = Vec::new();
    for backup in backups {
        if let Err(err) = fs::rename(&backup.original_path, &backup.backup_path) {
            drop(staged_writes);
            rollback_transaction(&[], &moved_backups);
            cleanup_created_dirs(&created_dirs);
            return Err(AppError::Tool(format!(
                "failed to back up patch target {}: {err}",
                backup.original_path.display()
            )));
        }
        moved_backups.push(backup);
    }

    // Phase 2: Rename staged temp files into place.
    let mut persisted_paths = Vec::new();
    let mut staged_writes = staged_writes.into_iter();
    while let Some(staged) = staged_writes.next() {
        let final_path = staged.final_path;
        match staged.temp.persist(&final_path) {
            Ok(_) => persisted_paths.push(final_path),
            Err(err) => {
                drop(staged_writes);
                rollback_transaction(&persisted_paths, &moved_backups);
                cleanup_created_dirs(&created_dirs);
                return Err(AppError::Tool(format!(
                    "failed to commit patch to {}: {err}",
                    final_path.display()
                )));
            }
        }
    }

    // Phase 3: Finalize deletes by dropping their backups.
    for backup in &moved_backups {
        if let Err(err) = remove_path_if_exists(&backup.backup_path) {
            eprintln!(
                "warning: failed to remove patch backup {}: {err}",
                backup.backup_path.display()
            );
        }
    }

    Ok(())
}

fn ensure_parent_dirs(
    parent: &Path,
    created_dirs: &mut Vec<PathBuf>,
    created_dir_set: &mut HashSet<PathBuf>,
) -> Result<(), AppError> {
    let mut missing = Vec::new();
    let mut cursor = parent;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        let Some(next) = cursor.parent() else {
            break;
        };
        cursor = next;
    }
    if missing.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent)?;
    missing.reverse();
    for dir in missing {
        if created_dir_set.insert(dir.clone()) {
            created_dirs.push(dir);
        }
    }
    Ok(())
}

fn unique_backup_path(path: &Path) -> Result<PathBuf, AppError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    for _ in 0..16 {
        let candidate = parent.join(format!(".headless_backup_{}", Ulid::new()));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(AppError::Tool(format!(
        "failed to allocate backup path for {}",
        path.display()
    )))
}

fn rollback_transaction(final_paths: &[PathBuf], backups: &[BackupEntry]) {
    for path in final_paths.iter().rev() {
        let _ = remove_path_if_exists(path);
    }
    for backup in backups.iter().rev() {
        let _ = remove_path_if_exists(&backup.original_path);
        let _ = fs::rename(&backup.backup_path, &backup.original_path);
    }
}

fn cleanup_created_dirs(created_dirs: &[PathBuf]) {
    for dir in created_dirs.iter().rev() {
        let _ = fs::remove_dir(dir);
    }
}

fn remove_path_if_exists(path: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir(path)?;
            Ok(())
        }
        Ok(_) => {
            fs::remove_file(path)?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{Arc, atomic::AtomicBool},
        thread,
        time::Duration,
    };

    use tempfile::TempDir;

    use super::*;
    use crate::{config::GlobalConfig, session::SessionStore, tools::RunControl};

    #[test]
    fn commit_phase_can_finish_after_run_budget_expires() {
        let temp = TempDir::new().expect("tempdir");
        let cwd = temp.path();
        let run_dir = cwd.join("run");
        fs::create_dir_all(&run_dir).expect("run dir");
        fs::write(cwd.join("existing.txt"), "before\n").expect("existing file");

        let config = test_config(cwd);
        let store = SessionStore::new(&config);
        store.ensure_root().expect("ensure sessions");
        let session = store.create_session().expect("create session");
        let run_control = RunControl::new(
            store,
            session.session_id,
            session.revision,
            Duration::from_millis(50),
            Arc::new(AtomicBool::new(false)),
        );
        let context = ToolContext::new(
            cwd,
            &run_dir,
            &config,
            false,
            &config.shell,
            &config.shell_args,
            &run_control,
        );
        let plan = ExecutionPlan {
            writes: vec![(cwd.join("existing.txt"), "after\n".to_string())],
            backup_targets: vec![cwd.join("existing.txt")],
            changed: Vec::new(),
        };

        let prepared = prepare_execution_plan(&context, &plan).expect("staged patch");
        thread::sleep(Duration::from_millis(75));
        assert!(
            context.remaining_budget().is_err(),
            "run budget should be exhausted"
        );

        commit_prepared_execution(prepared).expect("commit after timeout");

        assert_eq!(
            fs::read_to_string(cwd.join("existing.txt")).expect("patched file"),
            "after\n"
        );
        assert_no_patch_artifacts(cwd);
    }

    fn test_config(cwd: &Path) -> GlobalConfig {
        GlobalConfig {
            sessions_dir: cwd.join("sessions"),
            shell: "/bin/bash".to_string(),
            shell_args: vec!["-lc".to_string()],
            max_stdin_bytes: 1024,
            artifact_preview_bytes: 256,
            catastrophic_output_bytes: 4096,
            api_key: None,
            api_key_env: None,
            source_path: None,
        }
    }

    fn assert_no_patch_artifacts(dir: &Path) {
        let leftovers = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("dir entry").file_name())
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| name.starts_with(".headless_"))
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "unexpected patch artifacts in {}: {leftovers:?}",
            dir.display()
        );
    }
}
