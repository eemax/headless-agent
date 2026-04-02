use std::{sync::Arc, sync::atomic::AtomicBool, time::Duration};

use serde_json::json;

use crate::{
    agent_def::LoadedAgent,
    artifact::store_text_artifact,
    config::GlobalConfig,
    error::AppError,
    provider::openrouter::{ChatRequest, OpenRouterClient},
    session::SessionStore,
    tools::{RunControl, ToolContext, builtin_specs, execute_tool, tool_behavior},
    types::{
        LoopTermination, MessageRole, PromptMessage, RunArtifacts, RunOutcome, RunResult,
        ToolCallRecord, ToolExecution, TranscriptRecord,
    },
};

#[derive(Debug, Clone)]
pub struct AgentRunContext {
    pub session_id: String,
    pub run_id: String,
    pub run_dir: std::path::PathBuf,
    pub session_revision: u64,
    pub model: String,
    pub cwd: std::path::PathBuf,
    pub effort: crate::types::Effort,
    pub plan_mode: bool,
    pub prompt_messages: Vec<PromptMessage>,
    pub agent: LoadedAgent,
    pub config: GlobalConfig,
    pub session_store: SessionStore,
    pub api_key: String,
    pub interrupted: Arc<AtomicBool>,
}

const STEP_CAP: usize = 24;
const TOOL_RETRY_CAP: usize = 2;

pub fn run_agent_loop(context: AgentRunContext) -> Result<RunOutcome, AppError> {
    let base_url = context
        .agent
        .def
        .base_url
        .clone()
        .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
    let timeout = Duration::from_secs(context.agent.timeout_seconds()?);
    let max_output_tokens = context.agent.def.max_output_tokens.unwrap_or(12_000);
    let client = OpenRouterClient::new(base_url, context.api_key.clone(), timeout);
    let tool_specs = builtin_specs(&context.agent.def.enabled_tools);
    let run_control = RunControl::new(
        context.session_store.clone(),
        context.session_id.clone(),
        context.session_revision,
        timeout,
        context.interrupted.clone(),
    );
    let tool_context = ToolContext::new(
        &context.cwd,
        &context.run_dir,
        &context.config,
        context.plan_mode,
        &context.config.shell,
        &context.config.shell_args,
        &run_control,
    );
    let mut prompt_messages = context.prompt_messages.clone();
    let mut records = Vec::new();
    let mut artifacts = RunArtifacts::default();
    let mut total_prompt_tokens: usize = 0;
    let mut total_completion_tokens: usize = 0;

    for _step in 0..STEP_CAP {
        if let Err(err) = run_control.remaining_budget() {
            return Ok(partial_outcome(
                records,
                artifacts,
                LoopTermination::Timeout(err.to_string()),
                run_control,
                total_prompt_tokens,
                total_completion_tokens,
            ));
        }
        let response = match client.send_chat(ChatRequest {
            session_id: &context.session_id,
            model: &context.model,
            effort: context.effort,
            messages: &prompt_messages,
            tools: &tool_specs,
            max_output_tokens,
        }) {
            Ok(response) => response,
            Err(AppError::Timeout(msg)) => {
                return Ok(partial_outcome(
                    records,
                    artifacts,
                    LoopTermination::Timeout(msg),
                    run_control,
                    total_prompt_tokens,
                    total_completion_tokens,
                ));
            }
            Err(err) if !records.is_empty() => {
                return Ok(partial_outcome(
                    records,
                    artifacts,
                    LoopTermination::Error(err.to_string()),
                    run_control,
                    total_prompt_tokens,
                    total_completion_tokens,
                ));
            }
            Err(err) => return Err(err),
        };
        if let Some(usage) = &response.usage {
            total_prompt_tokens += usage.prompt_tokens;
            total_completion_tokens += usage.completion_tokens;
        }
        if let Err(err) = run_control.remaining_budget() {
            return Ok(partial_outcome(
                records,
                artifacts,
                LoopTermination::Timeout(err.to_string()),
                run_control,
                total_prompt_tokens,
                total_completion_tokens,
            ));
        }

        let (assistant_content, assistant_artifact) =
            assistant_content_to_record(&context, _step, response.content.as_deref())?;
        if let Some(ref artifact) = assistant_artifact {
            artifacts.paths.push(artifact.clone());
        }

        let assistant_record = TranscriptRecord {
            v: 1,
            ts: crate::session::now_rfc3339()?,
            run_id: context.run_id.clone(),
            role: MessageRole::Assistant,
            content: assistant_content,
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: assistant_artifact,
            tool_calls: (!response.tool_calls.is_empty()).then_some(response.tool_calls.clone()),
        };
        records.push(assistant_record.clone());
        prompt_messages.push(PromptMessage {
            role: MessageRole::Assistant,
            content: assistant_record.content.clone(),
            name: None,
            tool_call_id: None,
            tool_calls: assistant_record.tool_calls.clone().unwrap_or_default(),
        });

        if response.tool_calls.is_empty() {
            return Ok(RunOutcome {
                result: RunResult {
                    final_text: response.content.unwrap_or_default(),
                    records,
                    artifacts,
                    termination: LoopTermination::Complete,
                    total_prompt_tokens,
                    total_completion_tokens,
                },
                execution_guard: run_control.into_execution_guard(),
            });
        }

        for tool_call in response.tool_calls {
            let execution = match execute_with_retry(
                &tool_context,
                &context.agent.def.enabled_tools,
                &tool_call,
            ) {
                Ok(exec) => exec,
                Err(AppError::Timeout(msg)) => {
                    return Ok(partial_outcome(
                        records,
                        artifacts,
                        LoopTermination::Timeout(msg),
                        run_control,
                        total_prompt_tokens,
                        total_completion_tokens,
                    ));
                }
                Err(err) => {
                    return Ok(partial_outcome(
                        records,
                        artifacts,
                        LoopTermination::Error(err.to_string()),
                        run_control,
                        total_prompt_tokens,
                        total_completion_tokens,
                    ));
                }
            };
            if let Some(artifact) = &execution.artifact {
                artifacts.paths.push(artifact.clone());
            }
            let tool_record = TranscriptRecord {
                v: 1,
                ts: crate::session::now_rfc3339()?,
                run_id: context.run_id.clone(),
                role: MessageRole::Tool,
                content: Some(execution.content.clone()),
                name: Some(tool_call.name.clone()),
                tool_call_id: Some(tool_call.id.clone()),
                preview: execution.preview.clone(),
                artifact: execution.artifact.clone(),
                tool_calls: None,
            };
            records.push(tool_record.clone());
            prompt_messages.push(PromptMessage {
                role: MessageRole::Tool,
                content: tool_record.content.clone(),
                name: tool_record.name.clone(),
                tool_call_id: tool_record.tool_call_id.clone(),
                tool_calls: Vec::new(),
            });
        }
    }

    Ok(partial_outcome(
        records,
        artifacts,
        LoopTermination::StepCapExceeded,
        run_control,
        total_prompt_tokens,
        total_completion_tokens,
    ))
}

fn partial_outcome(
    records: Vec<TranscriptRecord>,
    artifacts: RunArtifacts,
    termination: LoopTermination,
    run_control: RunControl,
    prompt_tokens: usize,
    completion_tokens: usize,
) -> RunOutcome {
    let final_text = records
        .iter()
        .rev()
        .find_map(|r| {
            if r.role == MessageRole::Assistant {
                r.content.clone()
            } else {
                None
            }
        })
        .unwrap_or_default();
    RunOutcome {
        result: RunResult {
            final_text,
            records,
            artifacts,
            termination,
            total_prompt_tokens: prompt_tokens,
            total_completion_tokens: completion_tokens,
        },
        execution_guard: run_control.into_execution_guard(),
    }
}

fn execute_with_retry(
    tool_context: &ToolContext<'_>,
    enabled_tools: &[String],
    tool_call: &ToolCallRecord,
) -> Result<ToolExecution, AppError> {
    let mut attempts = 0;
    let retryable = tool_behavior(&tool_call.name)
        .map(|behavior| behavior.retryable)
        .unwrap_or(false);
    loop {
        attempts += 1;
        match execute_tool(
            tool_context,
            enabled_tools,
            &tool_call.name,
            &tool_call.arguments,
        ) {
            Ok(result) => return Ok(result),
            Err(AppError::Tool(_)) | Err(AppError::Shell(_))
                if retryable && attempts <= TOOL_RETRY_CAP =>
            {
                continue;
            }
            Err(AppError::Tool(error)) | Err(AppError::Shell(error)) => {
                return tool_context.finalize(
                    &tool_call.name,
                    &json!({
                        "ok": false,
                        "error": error,
                        "attempts": attempts,
                    }),
                );
            }
            Err(error) => return Err(error),
        }
    }
}

fn assistant_content_to_record(
    context: &AgentRunContext,
    step: usize,
    content: Option<&str>,
) -> Result<(Option<String>, Option<crate::types::ArtifactRef>), AppError> {
    let Some(content) = content else {
        return Ok((None, None));
    };
    if content.len() <= context.config.catastrophic_output_bytes {
        return Ok((Some(content.to_string()), None));
    }

    let stored = store_text_artifact(
        &context.run_dir,
        "assistant",
        &format!("assistant-step-{:02}.txt", step + 1),
        content,
        context.config.artifact_preview_bytes,
        context.config.catastrophic_output_bytes,
        Some("text/plain"),
    )?;
    Ok((Some(stored.transcript_text), stored.artifact))
}
