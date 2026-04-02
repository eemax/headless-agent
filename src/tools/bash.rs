use std::{
    io::{self, Read},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::{ToolContext, require_string},
};

pub fn bash_spec() -> crate::tools::ToolSpec {
    crate::tools::ToolSpec {
        name: "bash",
        description: "Run a shell command. stdout and stderr are each capped at the configured output limit. Use head/tail/grep in the command to manage large output.",
        parameters: json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        }),
    }
}

pub fn run_bash(context: &ToolContext<'_>, arguments: &Value) -> Result<Value, AppError> {
    let command = require_string(arguments, "command")?;
    let timeout = context.remaining_budget()?;
    let mut process = Command::new(context.shell);
    process.args(context.shell_args);
    process.arg(&command);
    process.current_dir(context.cwd);
    process.stdout(Stdio::piped());
    process.stderr(Stdio::piped());
    #[cfg(unix)]
    process.process_group(0);
    let mut child = process.spawn().map_err(|err| {
        AppError::Shell(format!("failed to spawn shell command `{command}`: {err}"))
    })?;

    let stdout_reader = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Shell("failed to capture shell stdout".to_string()))?;
    let stderr_reader = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Shell("failed to capture shell stderr".to_string()))?;
    let output_limit = context.config.catastrophic_output_bytes;
    let stdout_handle = spawn_reader(stdout_reader, output_limit);
    let stderr_handle = spawn_reader(stderr_reader, output_limit);
    let deadline = Instant::now() + timeout;

    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| AppError::Shell(format!("failed to wait for `{command}`: {err}")))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            kill_child(&mut child, &command)?;
            let _ = child.wait();
            let _ = collect_reader(stdout_handle);
            let _ = collect_reader(stderr_handle);
            return Err(AppError::Timeout(format!(
                "shell command `{command}` timed out after {:?}",
                timeout
            )));
        }
        thread::sleep(Duration::from_millis(10));
    };

    let (stdout_bytes, stdout_truncated) = collect_reader(stdout_handle)?;
    let (stderr_bytes, stderr_truncated) = collect_reader(stderr_handle)?;
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

    let mut result = json!({
        "ok": status.success(),
        "command": command,
        "cwd": context.cwd.display().to_string(),
        "exit_code": status.code(),
        "stdout": stdout,
        "stderr": stderr,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
    });
    if stdout_truncated || stderr_truncated {
        let which = match (stdout_truncated, stderr_truncated) {
            (true, true) => "stdout and stderr were both",
            (true, false) => "stdout was",
            (false, true) => "stderr was",
            _ => unreachable!(),
        };
        result["note"] = json!(format!(
            "{which} truncated at {output_limit} bytes. The command may have produced more output. Pipe through head/tail/grep to get specific sections."
        ));
    }
    Ok(result)
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
        Ok(Err(error)) => Err(AppError::Shell(format!(
            "failed to capture shell output: {error}"
        ))),
        Err(_) => Err(AppError::Shell(
            "shell output reader thread panicked".to_string(),
        )),
    }
}

fn kill_child(child: &mut std::process::Child, command: &str) -> Result<(), AppError> {
    let pid = child.id() as i32;

    // Try graceful shutdown first
    let term_result = unsafe { libc::kill(-pid, libc::SIGTERM) };
    if term_result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        // SIGTERM failed for non-ESRCH reason; fall through to SIGKILL
    } else {
        thread::sleep(Duration::from_millis(500));
        if child.try_wait().ok().flatten().is_some() {
            return Ok(());
        }
    }

    // Force kill
    let kill_result = unsafe { libc::kill(-pid, libc::SIGKILL) };
    if kill_result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(AppError::Shell(format!(
        "failed to kill timed out shell command `{command}`: {error}"
    )))
}
