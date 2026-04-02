use std::{fs, path::PathBuf};

use serde::Deserialize;

use crate::{
    config::{HeadlessRoots, resolve_relative},
    error::AppError,
    types::Effort,
};

#[derive(Debug, Clone, Deserialize)]
pub struct AgentDef {
    pub name: String,
    pub description: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub default_model: String,
    pub default_effort: Effort,
    pub max_output_tokens: Option<usize>,
    pub compaction_at_tokens: Option<usize>,
    pub skills_dir: Option<String>,
    #[serde(default)]
    pub enabled_skills: Vec<String>,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    pub system_prompt_file: String,
    pub timeout: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedAgent {
    pub def: AgentDef,
    pub path: PathBuf,
    pub system_prompt: String,
}

impl LoadedAgent {
    pub fn load(roots: &HeadlessRoots, name: &str) -> Result<Self, AppError> {
        let path = roots.resolve_agent(name)?;
        Self::from_path(path)
    }

    pub fn from_path(path: PathBuf) -> Result<Self, AppError> {
        let raw = fs::read_to_string(&path).map_err(|err| {
            AppError::Config(format!(
                "failed to read agent file {}: {err}",
                path.display()
            ))
        })?;
        let def: AgentDef = toml::from_str(&raw)?;
        if def.name.is_empty() {
            return Err(AppError::Config(format!(
                "agent file {} is missing a name",
                path.display()
            )));
        }
        if def.enabled_tools.is_empty() {
            return Err(AppError::Config(format!(
                "agent file {} must enable at least one tool",
                path.display()
            )));
        }
        let prompt_path = resolve_relative(&path, &def.system_prompt_file)?;
        let system_prompt = fs::read_to_string(&prompt_path).map_err(|err| {
            AppError::Config(format!(
                "failed to read system prompt {}: {err}",
                prompt_path.display()
            ))
        })?;
        Ok(Self {
            def,
            path,
            system_prompt,
        })
    }

    pub fn skills_dir(&self) -> Option<PathBuf> {
        self.def
            .skills_dir
            .as_ref()
            .map(|value| resolve_relative(&self.path, value))
            .transpose()
            .ok()
            .flatten()
    }

    pub fn timeout_seconds(&self) -> Result<u64, AppError> {
        let Some(value) = &self.def.timeout else {
            return Ok(2 * 60 * 60);
        };
        parse_duration(value)
    }
}

fn parse_duration(input: &str) -> Result<u64, AppError> {
    let input = input.trim();
    if let Some(value) = input.strip_suffix('h') {
        let hours: u64 = value
            .parse()
            .map_err(|_| AppError::Config(format!("invalid timeout value `{input}`")))?;
        return Ok(hours * 60 * 60);
    }
    if let Some(value) = input.strip_suffix('m') {
        let minutes: u64 = value
            .parse()
            .map_err(|_| AppError::Config(format!("invalid timeout value `{input}`")))?;
        return Ok(minutes * 60);
    }
    if let Some(value) = input.strip_suffix('s') {
        let seconds: u64 = value
            .parse()
            .map_err(|_| AppError::Config(format!("invalid timeout value `{input}`")))?;
        return Ok(seconds);
    }
    Err(AppError::Config(format!("invalid timeout value `{input}`")))
}
