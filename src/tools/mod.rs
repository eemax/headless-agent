pub mod bash;
pub mod files;
pub mod glob;
pub mod grep;
pub mod patch;

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::{Value, json};

use crate::{
    artifact::store_text_artifact, config::GlobalConfig, error::AppError, types::ToolExecution,
};

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

impl ToolSpec {
    pub fn as_json(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

pub struct ToolContext<'a> {
    pub cwd: &'a Path,
    pub run_dir: &'a Path,
    pub config: &'a GlobalConfig,
    pub plan_mode: bool,
    pub shell: &'a str,
    pub shell_args: &'a [String],
    sequence: AtomicUsize,
}

impl<'a> ToolContext<'a> {
    pub fn new(
        cwd: &'a Path,
        run_dir: &'a Path,
        config: &'a GlobalConfig,
        plan_mode: bool,
        shell: &'a str,
        shell_args: &'a [String],
    ) -> Self {
        Self {
            cwd,
            run_dir,
            config,
            plan_mode,
            shell,
            shell_args,
            sequence: AtomicUsize::new(1),
        }
    }

    pub fn finalize(&self, tool_name: &str, payload: &Value) -> Result<ToolExecution, AppError> {
        let raw = serde_json::to_string(payload)?;
        let index = self.sequence.fetch_add(1, Ordering::Relaxed);
        let stored = store_text_artifact(
            self.run_dir,
            "tool-outputs",
            &format!("{tool_name}-{index:03}.json"),
            &raw,
            self.config.artifact_preview_bytes,
            self.config.catastrophic_output_bytes,
            Some("application/json"),
        )?;
        Ok(ToolExecution {
            content: stored.transcript_text,
            preview: stored.artifact.as_ref().map(|_| stored.preview),
            artifact: stored.artifact,
        })
    }

    pub fn planned(&self, tool_name: &str, arguments: &Value) -> Result<ToolExecution, AppError> {
        self.finalize(
            tool_name,
            &json!({
                "planned": true,
                "tool": tool_name,
                "arguments": arguments,
            }),
        )
    }
}

pub fn builtin_specs(enabled_tools: &[String]) -> Vec<ToolSpec> {
    let mut specs = Vec::new();
    for name in enabled_tools {
        match name.as_str() {
            "read_file" => specs.push(files::read_file_spec()),
            "edit_file" => specs.push(files::edit_file_spec()),
            "write_file" => specs.push(files::write_file_spec()),
            "glob" => specs.push(glob::glob_spec()),
            "grep" => specs.push(grep::grep_spec()),
            "apply_patch" => specs.push(patch::apply_patch_spec()),
            "bash" => specs.push(bash::bash_spec()),
            _ => {}
        }
    }
    specs
}

pub fn execute_tool(
    context: &ToolContext<'_>,
    enabled_tools: &[String],
    name: &str,
    arguments: &Value,
) -> Result<ToolExecution, AppError> {
    if !enabled_tools.iter().any(|value| value == name) {
        return context.finalize(
            name,
            &json!({
                "ok": false,
                "error": format!("tool `{name}` is not enabled for this agent"),
            }),
        );
    }
    if context.plan_mode {
        return context.planned(name, arguments);
    }

    let payload = match name {
        "read_file" => files::read_file(context, arguments)?,
        "edit_file" => files::edit_file(context, arguments)?,
        "write_file" => files::write_file(context, arguments)?,
        "glob" => glob::glob_search(context, arguments)?,
        "grep" => grep::grep_search(context, arguments)?,
        "apply_patch" => patch::apply_patch(context, arguments)?,
        "bash" => bash::run_bash(context, arguments)?,
        _ => {
            return context.finalize(
                name,
                &json!({
                    "ok": false,
                    "error": format!("unknown built-in tool `{name}`"),
                }),
            );
        }
    };
    context.finalize(name, &payload)
}

pub fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub fn require_string(arguments: &Value, key: &str) -> Result<String, AppError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::Tool(format!("tool argument `{key}` must be a string")))
}

pub fn optional_string(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub fn optional_bool(arguments: &Value, key: &str, default: bool) -> bool {
    arguments
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

pub fn optional_u64(arguments: &Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(Value::as_u64)
}
