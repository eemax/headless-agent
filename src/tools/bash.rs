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
        description: "Run a shell command in the effective working directory.",
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
    let stdout_handle = spawn_reader(stdout_reader);
    let stderr_handle = spawn_reader(stderr_reader);
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

    let stdout = String::from_utf8_lossy(&collect_reader(stdout_handle)?).to_string();
    let stderr = String::from_utf8_lossy(&collect_reader(stderr_handle)?).to_string();

    Ok(json!({
        "ok": status.success(),
        "command": command,
        "cwd": context.cwd.display().to_string(),
        "exit_code": status.code(),
        "stdout": stdout,
        "stderr": stderr,
    }))
}

fn spawn_reader<R>(mut reader: R) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer)?;
        Ok(buffer)
    })
}

fn collect_reader(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, AppError> {
    match handle.join() {
        Ok(Ok(buffer)) => Ok(buffer),
        Ok(Err(error)) => Err(AppError::Shell(format!(
            "failed to capture shell output: {error}"
        ))),
        Err(_) => Err(AppError::Shell(
            "shell output reader thread panicked".to_string(),
        )),
    }
}

fn kill_child(child: &mut std::process::Child, command: &str) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
        if result == 0 {
            return Ok(());
        }

        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        Err(AppError::Shell(format!(
            "failed to kill timed out shell command `{command}`: {error}"
        )))
    }

    #[cfg(not(unix))]
    match child.kill() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(AppError::Shell(format!(
            "failed to kill timed out shell command `{command}`: {error}"
        ))),
    }
}
