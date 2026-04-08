use crate::{
    agent_def::LoadedAgent,
    error::AppError,
    prompt_def::LoadedPrompt,
    role_def::LoadedRole,
    types::{MessageRole, PromptMessage, TranscriptRecord},
};

#[derive(Debug, Clone)]
pub struct PromptAssembly {
    pub messages: Vec<PromptMessage>,
    pub estimated_tokens: usize,
    pub current_user_message: String,
}

pub fn assemble_prompt(
    agent: &LoadedAgent,
    role: Option<&LoadedRole>,
    prompt: Option<&LoadedPrompt>,
    history: &[TranscriptRecord],
    user_message: &str,
    stdin: Option<&str>,
) -> Result<PromptAssembly, AppError> {
    let mut messages = Vec::new();
    let system_prompt = build_system_prompt(agent, role);

    messages.push(PromptMessage {
        role: MessageRole::System,
        content: Some(system_prompt),
        name: None,
        tool_call_id: None,
        tool_calls: Vec::new(),
        reasoning: None,
        reasoning_details: None,
    });

    // Session history is replay-oriented rather than audit-oriented:
    // only persisted user prompts and completed assistant replies are sent back.
    for record in history {
        match record.role {
            MessageRole::User => {
                messages.push(PromptMessage {
                    role: record.role,
                    content: record.content_for_prompt(),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                    reasoning: None,
                    reasoning_details: None,
                });
            }
            MessageRole::Assistant if record.tool_calls.as_ref().is_none_or(|tc| tc.is_empty()) => {
                messages.push(PromptMessage {
                    role: record.role,
                    content: record.content_for_prompt(),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                    reasoning: None,
                    reasoning_details: None,
                });
            }
            _ => {}
        }
    }

    let current_message = build_current_user_message(prompt, user_message);

    messages.push(PromptMessage {
        role: MessageRole::User,
        content: Some(current_message.clone()),
        name: None,
        tool_call_id: None,
        tool_calls: Vec::new(),
        reasoning: None,
        reasoning_details: None,
    });

    if let Some(stdin) = stdin {
        messages.push(PromptMessage {
            role: MessageRole::User,
            content: Some(stdin.to_string()),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            reasoning: None,
            reasoning_details: None,
        });
    }

    let estimated_tokens = estimate_tokens_rough(&messages);
    Ok(PromptAssembly {
        messages,
        estimated_tokens,
        current_user_message: current_message,
    })
}

fn build_system_prompt(agent: &LoadedAgent, role: Option<&LoadedRole>) -> String {
    let mut parts = Vec::new();
    if let Some(system_prompt) = role.and_then(|value| value.system_prompt.as_deref()) {
        let trimmed = system_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed);
        }
    }
    let base = agent.system_prompt.trim();
    if !base.is_empty() {
        parts.push(base);
    }
    parts.join("\n\n")
}

fn build_current_user_message(prompt: Option<&LoadedPrompt>, user_message: &str) -> String {
    let mut current_message = String::new();
    if let Some(prefix) = prompt
        .map(|value| value.prompt.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        current_message.push_str(prefix);
        if !prefix.ends_with('\n') {
            current_message.push('\n');
        }
    }
    current_message.push_str(user_message);
    current_message
}

/// Pre-flight rough estimate based on character count (chars / 4).
/// The authoritative token count comes from the OpenRouter API response.
fn estimate_tokens_rough(messages: &[PromptMessage]) -> usize {
    let chars: usize = messages
        .iter()
        .filter_map(|message| message.content.as_ref())
        .map(|content| content.chars().count())
        .sum();
    (chars / 4).max(1)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::assemble_prompt;
    use crate::{
        agent_def::{AgentDef, LoadedAgent},
        prompt_def::{LoadedPrompt, PromptDef},
        role_def::{LoadedRole, RoleDef},
        types::Effort,
    };

    fn loaded_agent(enabled_tools: Vec<String>) -> LoadedAgent {
        LoadedAgent {
            def: AgentDef {
                name: "coder".to_string(),
                description: None,
                base_url: None,
                api_key: None,
                api_key_env: None,
                default_model: "test-model".to_string(),
                default_effort: Effort::Medium,
                max_output_tokens: None,
                compaction_at_tokens: None,
                skills_dir: None,
                enabled_skills: Vec::new(),
                enabled_tools,
                system_prompt_file: "prompt.md".to_string(),
                timeout: None,
            },
            path: PathBuf::new(),
            system_prompt: "base prompt".to_string(),
        }
    }

    fn loaded_role(system_prompt: Option<&str>) -> LoadedRole {
        LoadedRole {
            def: RoleDef {
                name: "auditor".to_string(),
                description: None,
                system_prompt_file: None,
            },
            path: PathBuf::new(),
            system_prompt: system_prompt.map(ToOwned::to_owned),
        }
    }

    fn loaded_prompt(prompt: Option<&str>) -> LoadedPrompt {
        LoadedPrompt {
            def: PromptDef {
                name: "auditor".to_string(),
                description: None,
                prompt_file: "prompt.md".to_string(),
            },
            path: PathBuf::new(),
            prompt: prompt.unwrap_or_default().to_string(),
        }
    }

    #[test]
    fn assemble_prompt_combines_role_and_agent_into_one_system_message() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["web_search".to_string(), "web_fetch".to_string()]),
            Some(&loaded_role(Some("auditor system"))),
            Some(&loaded_prompt(Some("named prompt"))),
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        assert_eq!(prompt.messages.len(), 2);
        assert_eq!(
            prompt.messages[0].content.as_deref(),
            Some("auditor system\n\nbase prompt")
        );
        assert_eq!(
            prompt.messages[1].content.as_deref(),
            Some("named prompt\nfind docs")
        );
        assert_eq!(prompt.current_user_message, "named prompt\nfind docs");
    }

    #[test]
    fn assemble_prompt_skips_empty_named_prompt_text() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["bash".to_string()]),
            Some(&loaded_role(Some("auditor system"))),
            Some(&loaded_prompt(Some("   "))),
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        assert_eq!(
            prompt.messages[0].content.as_deref(),
            Some("auditor system\n\nbase prompt")
        );
        assert_eq!(prompt.messages[1].content.as_deref(), Some("find docs"));
        assert_eq!(prompt.current_user_message, "find docs");
    }

    #[test]
    fn assemble_prompt_skips_empty_role_sections() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["bash".to_string()]),
            Some(&loaded_role(Some("   "))),
            None,
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        assert_eq!(prompt.messages[0].content.as_deref(), Some("base prompt"));
        assert_eq!(prompt.messages[1].content.as_deref(), Some("find docs"));
    }
}
