use std::{
    io::{self, BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use base64::Engine;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, optional_string, require_string, resolve_path},
};

const MATCH_LIMIT: usize = 1000;

pub fn grep_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "grep",
        description: "Search text files with ripgrep regex semantics. Returns up to 1000 matches. Searches hidden files, respects repo and global ignore files, and excludes .git. If truncated, narrow with a more specific pattern or search a subdirectory.",
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
    if !root.exists() {
        return Err(AppError::Tool(format!(
            "search path does not exist: {}",
            root.display()
        )));
    }
    if search_root_is_excluded(context.cwd, &root) {
        return Ok(json!({
            "ok": true,
            "matches": [],
            "files_scanned": 0,
            "truncated": false,
        }));
    }
    let timeout = context.remaining_budget()?;

    let mut process = Command::new("rg");
    process.current_dir(context.cwd);
    process.arg("--json");
    process.arg("--hidden");
    process.arg("-g");
    process.arg("!.git");
    process.arg("-e");
    process.arg(&pattern);
    process.arg("--");
    process.arg(&root);
    process.stdout(Stdio::piped());
    process.stderr(Stdio::piped());

    let mut child = process.spawn().map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            AppError::Tool("ripgrep (rg) is required but not found in PATH".to_string())
        } else {
            AppError::Tool(format!(
                "failed to spawn ripgrep for pattern `{pattern}`: {err}"
            ))
        }
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Tool("failed to capture ripgrep stdout".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Tool("failed to capture ripgrep stderr".to_string()))?;

    let limit_reached = Arc::new(AtomicBool::new(false));
    let stdout_handle = spawn_rg_parser(
        stdout,
        context.cwd.to_path_buf(),
        Arc::clone(&limit_reached),
    );
    let stderr_handle = spawn_reader(stderr, context.config.catastrophic_output_bytes);
    let deadline = Instant::now() + timeout;

    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| AppError::Tool(format!("failed to wait for ripgrep: {err}")))?
        {
            break status;
        }
        if let Err(err) = context.check_interrupted() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = collect_rg_parser(stdout_handle);
            let _ = collect_reader(stderr_handle);
            return Err(err);
        }
        if limit_reached.load(Ordering::Acquire) {
            let _ = child.kill();
            break child
                .wait()
                .map_err(|err| AppError::Tool(format!("failed to wait for ripgrep: {err}")))?;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = collect_rg_parser(stdout_handle);
            let _ = collect_reader(stderr_handle);
            return Err(AppError::Timeout(format!(
                "ripgrep search timed out after {:?}",
                timeout
            )));
        }
        thread::sleep(Duration::from_millis(10));
    };

    let parsed = collect_rg_parser(stdout_handle)?;
    let (stderr_bytes, _) = collect_reader(stderr_handle)?;
    let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
    let partial = status.code() == Some(2)
        && parsed.files_scanned > 0
        && stderr_has_only_nonfatal_path_errors(&stderr);

    if !status.success()
        && status.code() != Some(1)
        && !partial
        && !limit_reached.load(Ordering::Acquire)
    {
        let message = if stderr.is_empty() {
            if status.code() == Some(2) {
                "search path could not be read".to_string()
            } else {
                format!("ripgrep exited with status {status}")
            }
        } else {
            stderr
        };
        return Err(AppError::Tool(format!("ripgrep search failed: {message}")));
    }

    let mut result = json!({
        "ok": true,
        "matches": parsed.matches,
        "files_scanned": parsed.files_scanned,
        "truncated": parsed.total_matches > MATCH_LIMIT,
    });
    if partial {
        result["partial"] = json!(true);
        result["warning"] = json!("Some files could not be searched; results may be incomplete.");
    }
    if parsed.total_matches > MATCH_LIMIT {
        result["match_limit"] = json!(MATCH_LIMIT);
        result["last_file_scanned"] = json!(parsed.last_file_scanned);
        result["note"] = json!(format!(
            "Result limit reached ({MATCH_LIMIT} matches after scanning {} files). Narrow with a more specific pattern or search a subdirectory via the path argument.",
            parsed.files_scanned
        ));
    }
    Ok(result)
}

fn stderr_has_only_nonfatal_path_errors(stderr: &str) -> bool {
    !stderr.is_empty()
        && stderr
            .lines()
            .all(|line| line.starts_with("rg: ") && line.contains("(os error "))
}

fn search_root_is_excluded(cwd: &Path, root: &Path) -> bool {
    if path_has_component(root, ".git") {
        return true;
    }
    if root == cwd {
        return false;
    }

    let Ok(relative) = root.strip_prefix(cwd) else {
        return false;
    };
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }

    let depth = relative.components().count();
    if depth == 0 {
        return false;
    }

    let target = root.to_path_buf();
    let filter_target = target.clone();
    let mut builder = WalkBuilder::new(cwd);
    configure_ignore_walk(&mut builder, cwd);
    builder.max_depth(Some(depth));
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

fn configure_ignore_walk(builder: &mut WalkBuilder, cwd: &Path) {
    builder.current_dir(cwd);
    builder.hidden(false);
    builder.parents(true);
    builder.ignore(true);
    builder.git_ignore(true);
    builder.git_global(true);
    builder.git_exclude(true);
    builder.require_git(false);
}

fn path_has_component(path: &Path, name: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == name)
}

#[derive(Debug)]
struct ParsedRgOutput {
    matches: Vec<Value>,
    total_matches: usize,
    files_scanned: usize,
    last_file_scanned: String,
}

#[derive(Debug, Deserialize)]
struct RgMessage {
    #[serde(rename = "type")]
    kind: String,
    data: Value,
}

fn spawn_rg_parser<R>(
    reader: R,
    cwd: PathBuf,
    limit_reached: Arc<AtomicBool>,
) -> thread::JoinHandle<Result<ParsedRgOutput, AppError>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || parse_rg_output(reader, &cwd, &limit_reached))
}

fn parse_rg_output<R>(
    reader: R,
    cwd: &Path,
    limit_reached: &AtomicBool,
) -> Result<ParsedRgOutput, AppError>
where
    R: Read,
{
    let reader = BufReader::new(reader);
    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    let mut files_scanned = 0usize;
    let mut last_file_scanned = String::new();

    for line in reader.lines() {
        let line =
            line.map_err(|err| AppError::Tool(format!("failed to read ripgrep output: {err}")))?;
        if line.trim().is_empty() {
            continue;
        }
        let message: RgMessage = serde_json::from_str(&line)
            .map_err(|err| AppError::Tool(format!("failed to parse ripgrep output: {err}")))?;

        if let Some(path) = extract_path(&message.data, cwd)? {
            last_file_scanned = path;
        }

        match message.kind.as_str() {
            "begin" => {
                files_scanned += 1;
            }
            "match" => {
                total_matches += 1;
                if total_matches > MATCH_LIMIT {
                    limit_reached.store(true, Ordering::Release);
                    break;
                }

                let path = last_file_scanned.clone();
                let line_number = message
                    .data
                    .get("line_number")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        AppError::Tool("ripgrep output did not include a line number".to_string())
                    })?;
                let text = extract_line_text(&message.data)?;

                matches.push(json!({
                    "path": path,
                    "line": line_number,
                    "text": text,
                }));
            }
            "summary" => {
                files_scanned = message
                    .data
                    .get("stats")
                    .and_then(|stats| stats.get("searches"))
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize;
            }
            _ => {}
        }
    }

    Ok(ParsedRgOutput {
        matches,
        total_matches,
        files_scanned,
        last_file_scanned,
    })
}

fn extract_path(data: &Value, cwd: &Path) -> Result<Option<String>, AppError> {
    let Some(path) = decode_text_or_bytes(data.get("path"))? else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    Ok(Some(
        path.strip_prefix(cwd)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string(),
    ))
}

fn extract_line_text(data: &Value) -> Result<String, AppError> {
    let line = decode_text_or_bytes(data.get("lines"))?
        .ok_or_else(|| AppError::Tool("ripgrep output did not include match text".to_string()))?;
    Ok(line.lines().next().unwrap_or_default().to_string())
}

fn decode_text_or_bytes(value: Option<&Value>) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Ok(Some(text.to_string()));
    }
    if let Some(bytes) = value.get("bytes").and_then(Value::as_str) {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(bytes)
            .map_err(|err| {
                AppError::Tool(format!("failed to decode ripgrep bytes field: {err}"))
            })?;
        return Ok(Some(String::from_utf8_lossy(&decoded).to_string()));
    }
    Ok(None)
}

fn spawn_reader<R>(reader: R, limit: usize) -> thread::JoinHandle<io::Result<(Vec<u8>, bool)>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = reader;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut truncated = false;

        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                break;
            }

            let remaining = limit.saturating_sub(buffer.len());
            let keep = remaining.min(read);
            if keep > 0 {
                buffer.extend_from_slice(&chunk[..keep]);
            }
            if read > keep {
                truncated = true;
            }
        }

        Ok((buffer, truncated))
    })
}

fn collect_reader(
    handle: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, bool), AppError> {
    match handle.join() {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => Err(AppError::Tool(format!(
            "failed to capture ripgrep stderr: {error}"
        ))),
        Err(_) => Err(AppError::Tool(
            "ripgrep stderr reader thread panicked".to_string(),
        )),
    }
}

fn collect_rg_parser(
    handle: thread::JoinHandle<Result<ParsedRgOutput, AppError>>,
) -> Result<ParsedRgOutput, AppError> {
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(AppError::Tool("ripgrep parser thread panicked".to_string())),
    }
}
