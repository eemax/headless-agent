use std::{fs, path::PathBuf};

use serde::Deserialize;

use crate::{
    config::{HeadlessRoots, resolve_relative},
    error::AppError,
};

#[derive(Debug, Clone, Deserialize)]
pub struct PromptDef {
    pub name: String,
    pub description: Option<String>,
    pub prompt_file: String,
}

#[derive(Debug, Clone)]
pub struct LoadedPrompt {
    pub def: PromptDef,
    pub path: PathBuf,
    pub prompt: String,
}

impl LoadedPrompt {
    pub fn load(roots: &HeadlessRoots, name: &str) -> Result<Self, AppError> {
        let path = roots.resolve_prompt(name)?;
        Self::from_path(path)
    }

    pub fn from_path(path: PathBuf) -> Result<Self, AppError> {
        let raw = fs::read_to_string(&path).map_err(|err| {
            AppError::Config(format!(
                "failed to read prompt file {}: {err}",
                path.display()
            ))
        })?;
        let def: PromptDef = toml::from_str(&raw)?;
        let prompt_path = resolve_relative(&path, &def.prompt_file)?;
        let prompt = fs::read_to_string(&prompt_path).map_err(|err| {
            AppError::Config(format!(
                "failed to read prompt text {}: {err}",
                prompt_path.display()
            ))
        })?;
        Ok(Self { def, path, prompt })
    }
}
