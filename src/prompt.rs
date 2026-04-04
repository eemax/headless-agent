use crate::{
    agent_def::LoadedAgent,
    error::AppError,
    role_def::LoadedRole,
    types::{MessageRole, PromptMessage, TranscriptRecord},
};

#[derive(Debug, Clone)]
pub struct PromptAssembly {
    pub messages: Vec<PromptMessage>,
    pub estimated_tokens: usize,
    pub current_user_prompt: String,
}

pub fn assemble_prompt(
    agent: &LoadedAgent,
    role: Option<&LoadedRole>,
    apply_role_user_prefix: bool,
    history: &[TranscriptRecord],
    user_prompt: &str,
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
                });
            }
            MessageRole::Assistant if record.tool_calls.as_ref().is_none_or(|tc| tc.is_empty()) => {
                messages.push(PromptMessage {
                    role: record.role,
                    content: record.content_for_prompt(),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                });
            }
            _ => {}
        }
    }

    let current_prompt = build_current_user_prompt(role, apply_role_user_prefix, user_prompt);

    messages.push(PromptMessage {
        role: MessageRole::User,
        content: Some(current_prompt.clone()),
        name: None,
        tool_call_id: None,
        tool_calls: Vec::new(),
    });

    if let Some(stdin) = stdin {
        messages.push(PromptMessage {
            role: MessageRole::User,
            content: Some(stdin.to_string()),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
    }

    let estimated_tokens = estimate_tokens_rough(&messages);
    Ok(PromptAssembly {
        messages,
        estimated_tokens,
        current_user_prompt: current_prompt,
    })
}

fn build_system_prompt(agent: &LoadedAgent, role: Option<&LoadedRole>) -> String {
    let mut parts = Vec::new();
    let base = agent.system_prompt.trim();
    if !base.is_empty() {
        parts.push(base);
    }
    if let Some(system_prompt) = role.and_then(|value| value.system_prompt.as_deref()) {
        let trimmed = system_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed);
        }
    }
    parts.join("\n\n")
}

fn build_current_user_prompt(
    role: Option<&LoadedRole>,
    apply_role_user_prefix: bool,
    user_prompt: &str,
) -> String {
    let mut current_prompt = String::new();
    if apply_role_user_prefix
        && let Some(prefix) = role
            .and_then(|value| value.user_prefix.as_deref())
            .filter(|value| !value.trim().is_empty())
    {
        current_prompt.push_str(prefix);
        if !prefix.ends_with('\n') {
            current_prompt.push('\n');
        }
    }
    current_prompt.push_str(user_prompt);
    current_prompt
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

    fn loaded_role(system_prompt: Option<&str>, user_prefix: Option<&str>) -> LoadedRole {
        LoadedRole {
            def: RoleDef {
                name: "auditor".to_string(),
                description: None,
                system_prompt_file: None,
                user_prefix_file: None,
            },
            path: PathBuf::new(),
            system_prompt: system_prompt.map(ToOwned::to_owned),
            user_prefix: user_prefix.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn assemble_prompt_combines_agent_and_role_into_one_system_message() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["web_search".to_string(), "web_fetch".to_string()]),
            Some(&loaded_role(
                Some("auditor system"),
                Some("role user prefix"),
            )),
            true,
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        assert_eq!(prompt.messages.len(), 2);
        assert_eq!(
            prompt.messages[0].content.as_deref(),
            Some("base prompt\n\nauditor system")
        );
        assert_eq!(
            prompt.messages[1].content.as_deref(),
            Some("role user prefix\nfind docs")
        );
        assert_eq!(prompt.current_user_prompt, "role user prefix\nfind docs");
    }

    #[test]
    fn assemble_prompt_applies_role_user_prefix_only_when_requested() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["bash".to_string()]),
            Some(&loaded_role(
                Some("auditor system"),
                Some("role user prefix"),
            )),
            false,
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        assert_eq!(
            prompt.messages[0].content.as_deref(),
            Some("base prompt\n\nauditor system")
        );
        assert_eq!(prompt.messages[1].content.as_deref(), Some("find docs"));
        assert_eq!(prompt.current_user_prompt, "find docs");
    }

    #[test]
    fn assemble_prompt_skips_empty_role_sections() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["bash".to_string()]),
            Some(&loaded_role(Some("   "), Some("   "))),
            true,
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        assert_eq!(prompt.messages[0].content.as_deref(), Some("base prompt"));
        assert_eq!(prompt.messages[1].content.as_deref(), Some("find docs"));
    }
}
