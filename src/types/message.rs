use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub path: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptMessage {
    pub role: MessageRole,
    pub content: Option<String>,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptRecord {
    pub v: u8,
    pub ts: String,
    pub run_id: String,
    pub role: MessageRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRecord>>,
}

impl TranscriptRecord {
    pub fn content_for_prompt(&self) -> Option<String> {
        if let Some(content) = &self.content {
            return Some(content.clone());
        }
        if let Some(preview) = &self.preview {
            return Some(preview.clone());
        }
        self.artifact.as_ref().map(|artifact| {
            format!(
                "[artifact stored at {} ({} bytes)]",
                artifact.path, artifact.bytes
            )
        })
    }

    pub fn char_count(&self) -> usize {
        self.content
            .as_ref()
            .map_or(0, |value| value.chars().count())
            + self
                .preview
                .as_ref()
                .map_or(0, |value| value.chars().count())
    }
}
