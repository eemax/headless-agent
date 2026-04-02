use serde::{Deserialize, Serialize};

use crate::types::Effort;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub stopped_at: Option<String>,
    pub revision: u64,
    pub char_count: usize,
    pub agent_name: Option<String>,
    pub model: Option<String>,
    pub plan_enabled: Option<bool>,
    pub initial_role: Option<String>,
    pub cwd: Option<String>,
    pub effort: Option<Effort>,
}
