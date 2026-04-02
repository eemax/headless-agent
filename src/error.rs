use std::io;

use thiserror::Error;

use crate::exit::ExitCode;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Session(String),
    #[error("{0}")]
    SessionConflict(String),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    Timeout(String),
    #[error("{0}")]
    Tool(String),
    #[error("{0}")]
    Shell(String),
    #[error("{0}")]
    Runtime(String),
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("time format error: {0}")]
    TimeFormat(#[from] time::error::Format),
    #[error("time parse error: {0}")]
    TimeParse(#[from] time::error::Parse),
    #[error("cli parse error: {0}")]
    Lexopt(#[from] lexopt::Error),
}

impl AppError {
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Usage(_) | Self::Lexopt(_) => ExitCode::Usage,
            Self::Config(_) | Self::Toml(_) | Self::TomlSer(_) => ExitCode::Config,
            Self::Session(_) => ExitCode::Session,
            Self::SessionConflict(_) => ExitCode::SessionConflict,
            Self::Provider(_) => ExitCode::Provider,
            Self::Timeout(_) => ExitCode::Timeout,
            Self::Tool(_) => ExitCode::Tool,
            Self::Shell(_) => ExitCode::Shell,
            Self::Runtime(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::TimeFormat(_)
            | Self::TimeParse(_) => ExitCode::Runtime,
        }
    }
}
