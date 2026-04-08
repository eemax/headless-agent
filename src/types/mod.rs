mod message;
mod result;
mod session;

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

pub use message::{ArtifactRef, MessageRole, PromptMessage, ToolCallRecord, TranscriptRecord};
pub use result::{
    LoopTermination, ProviderStepRecord, ProviderTokenUsage, ProviderUsageSummary, RunArtifacts,
    RunOutcome, RunResult, ToolExecution,
};
pub use session::SessionMeta;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        };
        f.write_str(value)
    }
}

impl FromStr for Effort {
    type Err = AppError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            _ => Err(AppError::Usage(format!(
                "invalid effort `{value}`; expected one of none|minimal|low|medium|high|xhigh"
            ))),
        }
    }
}
