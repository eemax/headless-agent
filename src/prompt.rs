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
}

pub fn assemble_prompt(
    agent: &LoadedAgent,
    role: Option<&LoadedRole>,
    history: &[TranscriptRecord],
    user_prompt: &str,
    stdin: Option<&str>,
) -> Result<PromptAssembly, AppError> {
    let mut messages = Vec::new();

    messages.push(PromptMessage {
        role: MessageRole::System,
        content: Some(agent.system_prompt.clone()),
        name: None,
        tool_call_id: None,
        tool_calls: Vec::new(),
    });

    if let Some(addendum) = browsing_policy_addendum(&agent.def.enabled_tools) {
        messages.push(PromptMessage {
            role: MessageRole::System,
            content: Some(addendum.to_string()),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
    }

    if let Some(role) = role
        && let Some(system_prompt) = &role.system_prompt
    {
        messages.push(PromptMessage {
            role: MessageRole::System,
            content: Some(system_prompt.clone()),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
    }

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

    let mut current_prompt = String::new();
    if let Some(role) = role
        && let Some(prefix) = &role.user_prefix
    {
        current_prompt.push_str(prefix);
        if !prefix.ends_with('\n') {
            current_prompt.push('\n');
        }
    }
    current_prompt.push_str(user_prompt);

    messages.push(PromptMessage {
        role: MessageRole::User,
        content: Some(current_prompt),
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
    })
}

fn browsing_policy_addendum(enabled_tools: &[String]) -> Option<&'static str> {
    let has_search = enabled_tools.iter().any(|tool| tool == "web_search");
    let has_fetch = enabled_tools.iter().any(|tool| tool == "web_fetch");
    match (has_search, has_fetch) {
        (true, true) => Some(
            "Browsing policy:\n- Use web_search first to discover sources.\n- Use web_fetch for live verification or deeper reading when search snippets are not sufficient.\n- Prefer 1-3 sources unless broader coverage is clearly necessary.\n- Cite the exact URLs you used in the final answer.\n\nFreshness policy:\n- Use published_within_days when the task depends on recently published information.\n- Leave published_within_days unset for evergreen topics unless the user asks for recent coverage.\n\nEfficiency policy:\n- Avoid repeated searches with nearly identical queries.\n- Avoid fetching many live pages when search results already provide enough evidence.",
        ),
        (true, false) => Some(
            "Browsing policy:\n- Use web_search to discover relevant sources.\n- Prefer 1-3 sources unless broader coverage is clearly necessary.\n- Cite the exact URLs you used in the final answer.\n\nFreshness policy:\n- Use published_within_days when the task depends on recently published information.\n- Leave published_within_days unset for evergreen topics unless the user asks for recent coverage.\n\nEfficiency policy:\n- Avoid repeated searches with nearly identical queries.",
        ),
        (false, true) => Some(
            "Browsing policy:\n- Use web_fetch to read specific live pages when you need current or external information.\n- Fetch only the pages you need.\n- Cite the exact URLs you used in the final answer.\n\nEfficiency policy:\n- Avoid fetching many live pages when one or two targeted reads are sufficient.",
        ),
        (false, false) => None,
    }
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

    fn system_messages(prompt: &super::PromptAssembly) -> Vec<&str> {
        prompt
            .messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>()
    }

    #[test]
    fn assemble_prompt_includes_joint_browsing_policy_when_both_tools_are_enabled() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["web_search".to_string(), "web_fetch".to_string()]),
            None,
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        let system_messages = system_messages(&prompt);
        assert!(
            system_messages
                .iter()
                .any(|message| message.contains("Use web_search first"))
        );
        assert!(
            system_messages
                .iter()
                .any(|message| message.contains("published_within_days"))
        );
    }

    #[test]
    fn assemble_prompt_uses_search_only_browsing_policy_when_only_web_search_is_enabled() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["web_search".to_string()]),
            None,
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        let system_messages = system_messages(&prompt);
        assert!(
            system_messages
                .iter()
                .any(|message| message.contains("Use web_search to discover relevant sources"))
        );
        assert!(
            system_messages
                .iter()
                .all(|message| !message.contains("Use web_fetch for live verification"))
        );
    }

    #[test]
    fn assemble_prompt_uses_fetch_only_browsing_policy_when_only_web_fetch_is_enabled() {
        let prompt = assemble_prompt(
            &loaded_agent(vec!["web_fetch".to_string()]),
            None,
            &[],
            "find docs",
            None,
        )
        .expect("prompt assembly");

        let system_messages = system_messages(&prompt);
        assert!(
            system_messages
                .iter()
                .any(|message| message.contains("Use web_fetch to read specific live pages"))
        );
        assert!(
            system_messages
                .iter()
                .all(|message| !message.contains("Use web_search first"))
        );
        assert!(
            system_messages
                .iter()
                .all(|message| !message.contains("published_within_days"))
        );
    }
}
