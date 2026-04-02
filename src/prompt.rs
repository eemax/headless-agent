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
            MessageRole::Assistant
                if record.tool_calls.as_ref().is_none_or(|tc| tc.is_empty()) =>
            {
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
