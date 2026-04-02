pub mod jsonl;

use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

use fs2::FileExt;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use ulid::Ulid;

use crate::{
    config::GlobalConfig,
    error::AppError,
    types::{Effort, SessionMeta, TranscriptRecord},
};

#[derive(Debug, Clone)]
pub struct SessionStore {
    pub sessions_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SessionCommit {
    pub expected_revision: u64,
    pub char_count_delta: usize,
    pub records: Vec<TranscriptRecord>,
    pub bind_agent_name: Option<String>,
    pub bind_model: Option<String>,
    pub bind_effort: Option<Effort>,
    pub bind_cwd: Option<String>,
    pub bind_plan_enabled: Option<bool>,
    pub bind_initial_role: Option<String>,
}

impl SessionStore {
    pub fn new(config: &GlobalConfig) -> Self {
        Self {
            sessions_dir: config.sessions_dir.clone(),
        }
    }

    pub fn ensure_root(&self) -> Result<(), AppError> {
        fs::create_dir_all(&self.sessions_dir)?;
        Ok(())
    }

    pub fn create_session(&self) -> Result<SessionMeta, AppError> {
        self.ensure_root()?;
        let session_id = new_id();
        let session_dir = self.session_dir(&session_id);
        fs::create_dir_all(session_dir.join("runs"))?;
        File::create(session_dir.join("messages.jsonl"))?;
        File::create(session_dir.join("lock"))?;
        let now = now_rfc3339()?;
        let meta = SessionMeta {
            session_id: session_id.clone(),
            created_at: now.clone(),
            updated_at: now,
            stopped_at: None,
            revision: 0,
            char_count: 0,
            agent_name: None,
            model: None,
            plan_enabled: None,
            initial_role: None,
            cwd: None,
            effort: None,
        };
        self.write_meta(&meta)?;
        Ok(meta)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMeta>, AppError> {
        self.ensure_root()?;
        let mut sessions: Vec<SessionMeta> = Vec::new();
        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let meta_path = entry.path().join("meta.json");
            if meta_path.exists() {
                let raw = fs::read_to_string(meta_path)?;
                sessions.push(serde_json::from_str(&raw)?);
            }
        }
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        Ok(sessions)
    }

    pub fn load_meta(&self, session_id: &str) -> Result<SessionMeta, AppError> {
        let path = self.session_dir(session_id).join("meta.json");
        if !path.exists() {
            return Err(AppError::Session(format!(
                "session `{session_id}` does not exist"
            )));
        }
        let raw = fs::read_to_string(&path)?;
        let meta = serde_json::from_str(&raw)?;
        Ok(meta)
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<TranscriptRecord>, AppError> {
        jsonl::read_records(&self.session_dir(session_id).join("messages.jsonl"))
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(session_id)
    }

    pub fn stop_session(&self, session_id: &str) -> Result<SessionMeta, AppError> {
        let lock_path = self.session_dir(session_id).join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)?;
        lock.lock_exclusive()?;
        let mut meta = self.load_meta(session_id)?;
        meta.stopped_at = Some(now_rfc3339()?);
        meta.updated_at = meta.stopped_at.clone().unwrap_or_default();
        self.write_meta(&meta)?;
        lock.unlock()?;
        Ok(meta)
    }

    pub fn append_run(
        &self,
        session_id: &str,
        commit: SessionCommit,
    ) -> Result<SessionMeta, AppError> {
        let lock_path = self.session_dir(session_id).join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)?;
        lock.lock_exclusive()?;
        let mut meta = self.load_meta(session_id)?;
        if meta.revision != commit.expected_revision {
            lock.unlock()?;
            return Err(AppError::SessionConflict(format!(
                "session `{session_id}` was updated concurrently"
            )));
        }
        if meta.stopped_at.is_some() {
            lock.unlock()?;
            return Err(AppError::Session(format!(
                "session `{session_id}` has been stopped and cannot accept new runs"
            )));
        }
        jsonl::append_records(
            &self.session_dir(session_id).join("messages.jsonl"),
            &commit.records,
        )?;
        if meta.agent_name.is_none() {
            meta.agent_name = commit.bind_agent_name;
            meta.model = commit.bind_model;
            meta.effort = commit.bind_effort;
            meta.cwd = commit.bind_cwd;
            meta.plan_enabled = commit.bind_plan_enabled;
            meta.initial_role = commit.bind_initial_role;
        }
        meta.updated_at = now_rfc3339()?;
        meta.revision += 1;
        meta.char_count += commit.char_count_delta;
        self.write_meta(&meta)?;
        lock.unlock()?;
        Ok(meta)
    }

    fn write_meta(&self, meta: &SessionMeta) -> Result<(), AppError> {
        let session_dir = self.session_dir(&meta.session_id);
        fs::create_dir_all(&session_dir)?;
        let path = session_dir.join("meta.json");
        fs::write(path, serde_json::to_vec_pretty(meta)?)?;
        Ok(())
    }
}

pub fn now_rfc3339() -> Result<String, AppError> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

pub fn new_id() -> String {
    Ulid::new().to_string().to_lowercase()
}
