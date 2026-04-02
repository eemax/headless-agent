use std::time::Duration;

use serde_json::json;

use crate::{
    agent_def::LoadedAgent,
    artifact::store_text_artifact,
    config::GlobalConfig,
    error::AppError,
    provider::openrouter::OpenRouterClient,
    tools::{ToolContext, builtin_specs, execute_tool},
    types::{
        MessageRole, PromptMessage, RunArtifacts, RunResult, ToolCallRecord, ToolExecution,
        TranscriptRecord,
    },
};

#[derive(Debug, Clone)]
pub struct AgentRunContext {
    pub session_id: String,
    pub run_id: String,
    pub run_dir: std::path::PathBuf,
    pub model: String,
    pub cwd: std::path::PathBuf,
    pub effort: crate::types::Effort,
    pub plan_mode: bool,
    pub prompt_messages: Vec<PromptMessage>,
    pub agent: LoadedAgent,
    pub config: GlobalConfig,
    pub api_key: String,
}

const STEP_CAP: usize = 24;
const TOOL_RETRY_CAP: usize = 2;

pub fn run_agent_loop(context: AgentRunContext) -> Result<RunResult, AppError> {
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
    let tool_context = ToolContext::new(
        &context.cwd,
        &context.run_dir,
        &context.config,
        context.plan_mode,
        &context.config.shell,
        &context.config.shell_args,
    );
    let mut prompt_messages = context.prompt_messages.clone();
    let mut records = Vec::new();
    let mut artifacts = RunArtifacts::default();

    for step in 0..STEP_CAP {
        let response = client.send_chat(
            &context.session_id,
            &context.model,
            context.effort,
            &prompt_messages,
            &tool_specs,
            max_output_tokens,
        )?;

        let (assistant_content, assistant_artifact) =
            assistant_content_to_record(&context, step, response.content.as_deref())?;
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
            return Ok(RunResult {
                final_text: response.content.unwrap_or_default(),
                records,
                artifacts,
            });
        }

        for tool_call in response.tool_calls {
            let execution =
                execute_with_retry(&tool_context, &context.agent.def.enabled_tools, &tool_call);
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

    Err(AppError::Runtime(
        "agent loop exceeded the step cap of 24 iterations".to_string(),
    ))
}

fn execute_with_retry(
    tool_context: &ToolContext<'_>,
    enabled_tools: &[String],
    tool_call: &ToolCallRecord,
) -> ToolExecution {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match execute_tool(
            tool_context,
            enabled_tools,
            &tool_call.name,
            &tool_call.arguments,
        ) {
            Ok(result) => return result,
            Err(error) if attempts <= TOOL_RETRY_CAP => continue,
            Err(error) => {
                return tool_context
                    .finalize(
                        &tool_call.name,
                        &json!({
                            "ok": false,
                            "error": error.to_string(),
                            "attempts": attempts,
                        }),
                    )
                    .unwrap_or_else(|finalize_error| ToolExecution {
                        content: json!({
                            "ok": false,
                            "error": finalize_error.to_string(),
                        })
                        .to_string(),
                        preview: None,
                        artifact: None,
                    });
            }
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
    if content.as_bytes().len() <= context.config.catastrophic_output_bytes {
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
