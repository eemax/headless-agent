use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::error::AppError;

const HOME_ROOT_DIR: &str = ".headless-agent";

#[derive(Debug, Clone)]
pub struct HeadlessRoots {
    pub repo_root: Option<PathBuf>,
    pub home_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct PartialGlobalConfig {
    sessions_dir: Option<String>,
    shell: Option<String>,
    shell_args: Option<Vec<String>>,
    max_stdin_bytes: Option<usize>,
    artifact_preview_bytes: Option<usize>,
    catastrophic_output_bytes: Option<usize>,
    default_agent: Option<String>,
    api_key: Option<String>,
    api_key_env: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GlobalConfig {
    pub sessions_dir: PathBuf,
    pub shell: String,
    pub shell_args: Vec<String>,
    pub max_stdin_bytes: usize,
    pub artifact_preview_bytes: usize,
    pub catastrophic_output_bytes: usize,
    pub default_agent: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub source_path: Option<PathBuf>,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            sessions_dir: PathBuf::from("~/.headless-agent/sessions"),
            shell: "/bin/bash".to_string(),
            shell_args: vec!["-lc".to_string()],
            max_stdin_bytes: 1024 * 1024,
            artifact_preview_bytes: 16 * 1024,
            catastrophic_output_bytes: 16 * 1024 * 1024,
            default_agent: None,
            api_key: None,
            api_key_env: None,
            source_path: None,
        }
    }
}

impl GlobalConfig {
    pub fn load(roots: &HeadlessRoots) -> Result<Self, AppError> {
        let mut config = GlobalConfig::default();
        if let Some(path) = roots.resolve_config() {
            let raw = fs::read_to_string(&path).map_err(|err| {
                AppError::Config(format!(
                    "failed to read config file {}: {err}",
                    path.display()
                ))
            })?;
            let partial: PartialGlobalConfig = toml::from_str(&raw)?;
            if let Some(value) = partial.sessions_dir {
                config.sessions_dir = expand_tilde(&value)?;
            } else {
                config.sessions_dir = expand_tilde("~/.headless-agent/sessions")?;
            }
            if let Some(value) = partial.shell {
                config.shell = value;
            }
            if let Some(value) = partial.shell_args {
                config.shell_args = value;
            }
            if let Some(value) = partial.max_stdin_bytes {
                config.max_stdin_bytes = value;
            }
            if let Some(value) = partial.artifact_preview_bytes {
                config.artifact_preview_bytes = value;
            }
            if let Some(value) = partial.catastrophic_output_bytes {
                config.catastrophic_output_bytes = value;
            }
            config.default_agent = partial.default_agent;
            config.api_key = partial.api_key;
            config.api_key_env = partial.api_key_env;
            config.source_path = Some(path);
        } else {
            config.sessions_dir = expand_tilde("~/.headless-agent/sessions")?;
        }
        Ok(config)
    }
}

impl HeadlessRoots {
    pub fn discover() -> Self {
        let repo_root = env::var_os("HEADLESS_REPO_ROOT")
            .map(PathBuf::from)
            .or_else(|| {
                let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                if has_headless_layout(&manifest_root) {
                    Some(manifest_root)
                } else {
                    None
                }
            });
        let home_root = env::var_os("HEADLESS_HOME_ROOT")
            .map(PathBuf::from)
            .or_else(|| home::home_dir().map(|path| path.join(HOME_ROOT_DIR)));
        Self {
            repo_root,
            home_root,
        }
    }

    pub fn resolve_config(&self) -> Option<PathBuf> {
        self.resolve_existing("config.toml")
    }

    pub fn resolve_agent(&self, name: &str) -> Result<PathBuf, AppError> {
        self.resolve_named("agents", name)
    }

    pub fn resolve_role(&self, name: &str) -> Result<PathBuf, AppError> {
        self.resolve_named("roles", name)
    }

    pub fn list_agents(&self) -> Result<Vec<String>, AppError> {
        self.list_named("agents")
    }

    pub fn list_roles(&self) -> Result<Vec<String>, AppError> {
        self.list_named("roles")
    }

    fn resolve_named(&self, dir: &str, name: &str) -> Result<PathBuf, AppError> {
        let relative = format!("{dir}/{name}.toml");
        self.resolve_existing(&relative).ok_or_else(|| {
            AppError::Config(format!(
                "unable to resolve {dir} `{name}` in any Headless root"
            ))
        })
    }

    fn resolve_existing(&self, relative: &str) -> Option<PathBuf> {
        if let Some(repo_root) = &self.repo_root {
            let candidate = repo_root.join(relative);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        if let Some(home_root) = &self.home_root {
            let candidate = home_root.join(relative);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }

    fn list_named(&self, dir: &str) -> Result<Vec<String>, AppError> {
        let mut names = BTreeSet::new();
        for root in [&self.repo_root, &self.home_root].into_iter().flatten() {
            let path = root.join(dir);
            if !path.exists() {
                continue;
            }
            for entry in fs::read_dir(&path).map_err(|err| {
                AppError::Config(format!("failed to list {}: {err}", path.display()))
            })? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) == Some("toml")
                    && let Some(stem) = path.file_stem().and_then(|value| value.to_str())
                {
                    names.insert(stem.to_string());
                }
            }
        }
        Ok(names.into_iter().collect())
    }
}

fn has_headless_layout(root: &Path) -> bool {
    root.join("agents").exists() || root.join("roles").exists() || root.join("config.toml").exists()
}

pub fn resolve_relative(base_file: &Path, value: &str) -> Result<PathBuf, AppError> {
    let base_dir = base_file.parent().ok_or_else(|| {
        AppError::Config(format!(
            "path {} has no parent directory",
            base_file.display()
        ))
    })?;
    Ok(base_dir.join(value))
}

pub fn expand_tilde(input: &str) -> Result<PathBuf, AppError> {
    if let Some(stripped) = input.strip_prefix("~/") {
        let home = home::home_dir()
            .ok_or_else(|| AppError::Config("unable to resolve home directory".to_string()))?;
        return Ok(home.join(stripped));
    }
    if input == "~" {
        return home::home_dir()
            .ok_or_else(|| AppError::Config("unable to resolve home directory".to_string()));
    }
    Ok(PathBuf::from(input))
}
