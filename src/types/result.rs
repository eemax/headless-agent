use crate::{
    error::AppError,
    session::SessionExecutionGuard,
    types::{ArtifactRef, TranscriptRecord},
};

#[derive(Debug, Clone, Default)]
pub struct RunArtifacts {
    pub paths: Vec<ArtifactRef>,
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
    pub termination: LoopTermination,
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub result: RunResult,
    pub execution_guard: Option<SessionExecutionGuard>,
}
