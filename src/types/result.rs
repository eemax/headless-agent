use crate::types::{ArtifactRef, TranscriptRecord};

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

#[derive(Debug, Clone)]
pub struct RunResult {
    pub final_text: String,
    pub records: Vec<TranscriptRecord>,
    pub artifacts: RunArtifacts,
}
