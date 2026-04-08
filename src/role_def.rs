use std::{fs, path::PathBuf};

use serde::Deserialize;

use crate::{
    config::{HeadlessRoots, resolve_relative},
    error::AppError,
};

#[derive(Debug, Clone, Deserialize)]
pub struct RoleDef {
    pub name: String,
    pub description: Option<String>,
    pub system_prompt_file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedRole {
    pub def: RoleDef,
    pub path: PathBuf,
    pub system_prompt: Option<String>,
}

impl LoadedRole {
    pub fn load(roots: &HeadlessRoots, name: &str) -> Result<Self, AppError> {
        let path = roots.resolve_role(name)?;
        Self::from_path(path)
    }

    pub fn from_path(path: PathBuf) -> Result<Self, AppError> {
        let raw = fs::read_to_string(&path).map_err(|err| {
            AppError::Config(format!(
                "failed to read role file {}: {err}",
                path.display()
            ))
        })?;
        let def: RoleDef = toml::from_str(&raw)?;
        let system_prompt = def
            .system_prompt_file
            .as_ref()
            .map(|value| resolve_relative(&path, value))
            .transpose()?
            .map(fs::read_to_string)
            .transpose()
            .map_err(|err| {
                AppError::Config(format!(
                    "failed to read role prompt {}: {err}",
                    path.display()
                ))
            })?;
        Ok(Self {
            def,
            path,
            system_prompt,
        })
    }
}
