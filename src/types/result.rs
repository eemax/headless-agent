use crate::{error::AppError, session::SessionExecutionGuard};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ArtifactRef, Effort, ToolCallRecord, TranscriptRecord};

#[derive(Debug, Clone, Default)]
pub struct RunArtifacts {
    pub paths: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTokenUsage {
    #[serde(default)]
    pub prompt_tokens: usize,
    #[serde(default)]
    pub completion_tokens: usize,
    #[serde(default)]
    pub total_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsageSummary {
    #[serde(default)]
    pub steps: usize,
    #[serde(default)]
    pub steps_with_usage: usize,
    #[serde(default)]
    pub prompt_tokens: usize,
    #[serde(default)]
    pub completion_tokens: usize,
    #[serde(default)]
    pub total_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<usize>,
}

impl ProviderUsageSummary {
    pub fn observe_step(&mut self, usage: Option<&ProviderTokenUsage>) {
        self.steps += 1;
        let Some(usage) = usage else {
            return;
        };
        self.steps_with_usage += 1;
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        self.total_tokens += usage.total_tokens;
        accumulate_optional(&mut self.cached_tokens, usage.cached_tokens);
        accumulate_optional(&mut self.cache_write_tokens, usage.cache_write_tokens);
        accumulate_optional(&mut self.reasoning_tokens, usage.reasoning_tokens);
    }
}

fn accumulate_optional(target: &mut Option<usize>, value: Option<usize>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or(0) + value);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderStepRecord {
    pub v: u8,
    pub ts: String,
    pub step: usize,
    pub model: String,
    pub effort: Effort,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProviderTokenUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_raw: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ToolExecution {
    pub content: String,
    pub preview: Option<String>,
    pub artifact: Option<ArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopTermination {
    Complete,
    Timeout(String),
    Error(String),
}

impl LoopTermination {
    pub fn into_error(self) -> Option<AppError> {
        match self {
            Self::Complete => None,
            Self::Timeout(msg) => Some(AppError::Timeout(msg)),
            Self::Error(msg) => Some(AppError::Runtime(msg)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub final_text: String,
    pub records: Vec<TranscriptRecord>,
    pub artifacts: RunArtifacts,
    pub provider_steps: Vec<ProviderStepRecord>,
    pub provider_usage_summary: ProviderUsageSummary,
    pub termination: LoopTermination,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub result: RunResult,
    pub execution_guard: Option<SessionExecutionGuard>,
}
