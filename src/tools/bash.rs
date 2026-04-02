use std::process::Command;

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
    let mut process = Command::new(context.shell);
    process.args(context.shell_args);
    process.arg(&command);
    process.current_dir(context.cwd);
    let output = process.output().map_err(|err| {
        AppError::Shell(format!("failed to spawn shell command `{command}`: {err}"))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    Ok(json!({
        "ok": output.status.success(),
        "command": command,
        "cwd": context.cwd.display().to_string(),
        "exit_code": output.status.code(),
        "stdout": stdout,
        "stderr": stderr,
    }))
}
