pub mod bash;
pub mod files;
pub mod glob;
pub mod grep;
pub mod patch;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    artifact::store_text_artifact,
    config::GlobalConfig,
    error::AppError,
    session::{SessionExecutionGuard, SessionStore},
    types::ToolExecution,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAccess {
    ReadOnly,
    Mutating,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolBehavior {
    pub access: ToolAccess,
    pub retryable: bool,
}

pub struct RunControl {
    session_store: SessionStore,
    session_id: String,
    session_revision: u64,
    timeout: Duration,
    deadline: Instant,
    execution_guard: RefCell<Option<SessionExecutionGuard>>,
    interrupted: Arc<AtomicBool>,
}

pub struct ToolContext<'a> {
    pub cwd: &'a Path,
    pub run_dir: &'a Path,
    pub config: &'a GlobalConfig,
    pub plan_mode: bool,
    pub shell: &'a str,
    pub shell_args: &'a [String],
    pub run_control: &'a RunControl,
    sequence: AtomicUsize,
}

impl RunControl {
    pub fn new(
        session_store: SessionStore,
        session_id: String,
        session_revision: u64,
        timeout: Duration,
        interrupted: Arc<AtomicBool>,
    ) -> Self {
        Self {
            session_store,
            session_id,
            session_revision,
            timeout,
            deadline: Instant::now() + timeout,
            execution_guard: RefCell::new(None),
            interrupted,
        }
    }

    pub fn remaining_budget(&self) -> Result<Duration, AppError> {
        self.check_interrupted()?;
        let now = Instant::now();
        if now >= self.deadline {
            return Err(AppError::Timeout(format!(
                "run exceeded configured timeout of {:?}",
                self.timeout
            )));
        }
        Ok(self.deadline.saturating_duration_since(now))
    }

    pub fn check_interrupted(&self) -> Result<(), AppError> {
        if self.interrupted.load(Ordering::Relaxed) {
            return Err(AppError::Runtime("interrupted by signal".to_string()));
        }
        Ok(())
    }

    pub fn ensure_mutating_access(&self) -> Result<(), AppError> {
        let _ = self.remaining_budget()?;
        if self.execution_guard.borrow().is_some() {
            return Ok(());
        }
        let guard = self
            .session_store
            .acquire_execution_lock(&self.session_id, self.session_revision)?;
        self.execution_guard.replace(Some(guard));
        Ok(())
    }

    pub fn into_execution_guard(self) -> Option<SessionExecutionGuard> {
        self.execution_guard.into_inner()
    }
}

impl<'a> ToolContext<'a> {
    pub fn new(
        cwd: &'a Path,
        run_dir: &'a Path,
        config: &'a GlobalConfig,
        plan_mode: bool,
        shell: &'a str,
        shell_args: &'a [String],
        run_control: &'a RunControl,
    ) -> Self {
        Self {
            cwd,
            run_dir,
            config,
            plan_mode,
            shell,
            shell_args,
            run_control,
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

    pub fn remaining_budget(&self) -> Result<Duration, AppError> {
        self.run_control.remaining_budget()
    }

    pub fn check_interrupted(&self) -> Result<(), AppError> {
        self.run_control.check_interrupted()
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
    let behavior = tool_behavior(name);
    let _ = context.remaining_budget()?;
    if context.plan_mode {
        return context.planned(name, arguments);
    }
    if matches!(
        behavior,
        Some(ToolBehavior {
            access: ToolAccess::Mutating,
            ..
        })
    ) {
        context.run_control.ensure_mutating_access()?;
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

pub fn tool_behavior(name: &str) -> Option<ToolBehavior> {
    match name {
        "read_file" | "glob" | "grep" => Some(ToolBehavior {
            access: ToolAccess::ReadOnly,
            retryable: true,
        }),
        "edit_file" | "write_file" | "apply_patch" | "bash" => Some(ToolBehavior {
            access: ToolAccess::Mutating,
            retryable: false,
        }),
        _ => None,
    }
}

pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".hg",
    ".svn",
    "__pycache__",
    "dist",
    "build",
];

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
