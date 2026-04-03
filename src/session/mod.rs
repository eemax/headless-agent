pub mod jsonl;

#[cfg(test)]
use std::path::Path;
use std::{
    fs::{self, File, OpenOptions},
    io,
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

#[derive(Debug)]
pub struct SessionExecutionGuard {
    file: File,
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
        File::create(session_dir.join("execution.lock"))?;
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
        let _ = self.load_meta(session_id)?;
        let lock_path = self.session_dir(session_id).join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock.lock_exclusive()?;
        let mut meta = self.load_meta(session_id)?;
        let now = now_rfc3339()?;
        meta.stopped_at = Some(now.clone());
        meta.updated_at = now;
        self.write_meta(&meta)?;
        lock.unlock()?;
        Ok(meta)
    }

    pub fn append_run(
        &self,
        session_id: &str,
        commit: SessionCommit,
        execution_guard: Option<&SessionExecutionGuard>,
    ) -> Result<SessionMeta, AppError> {
        self.append_run_inner(session_id, commit, execution_guard, || Ok(()))
    }

    fn append_run_inner<F>(
        &self,
        session_id: &str,
        commit: SessionCommit,
        execution_guard: Option<&SessionExecutionGuard>,
        after_execution_lock: F,
    ) -> Result<SessionMeta, AppError>
    where
        F: FnOnce() -> Result<(), AppError>,
    {
        let lock_path = self.session_dir(session_id).join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock.lock_exclusive()?;
        // Read-only appends hold execution.lock through commit so a concurrent
        // mutating run fails before side effects. This must remain try-lock
        // based because append_run already holds the session lock here.
        let _transient_guard = if execution_guard.is_none() {
            Some(self.acquire_execution_lock(session_id, commit.expected_revision)?)
        } else {
            None
        };
        after_execution_lock()?;
        let mut meta = self.load_meta(session_id)?;
        if meta.revision != commit.expected_revision {
            lock.unlock()?;
            return Err(AppError::SessionConflict(format!(
                "session `{session_id}` was updated concurrently"
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
            meta.initial_role = commit.bind_initial_role;
        }
        meta.updated_at = now_rfc3339()?;
        meta.revision += 1;
        meta.char_count += commit.char_count_delta;
        self.write_meta(&meta)?;
        lock.unlock()?;
        Ok(meta)
    }

    pub fn acquire_execution_lock(
        &self,
        session_id: &str,
        expected_revision: u64,
    ) -> Result<SessionExecutionGuard, AppError> {
        let file = self.open_execution_lock(session_id)?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(AppError::SessionConflict(format!(
                    "session `{session_id}` already has a mutating run in progress"
                )));
            }
            Err(error) => return Err(error.into()),
        }

        let meta = match self.load_meta(session_id) {
            Ok(meta) => meta,
            Err(error) => {
                let _ = file.unlock();
                return Err(error);
            }
        };
        if meta.revision != expected_revision {
            let _ = file.unlock();
            return Err(AppError::SessionConflict(format!(
                "session `{session_id}` was updated concurrently"
            )));
        }
        // `session stop` is enforced when the run is admitted in app.rs. Once a
        // run has started, it may reach its first mutating tool later and still
        // finish under the execution lock.

        Ok(SessionExecutionGuard { file })
    }

    fn write_meta(&self, meta: &SessionMeta) -> Result<(), AppError> {
        let session_dir = self.session_dir(&meta.session_id);
        fs::create_dir_all(&session_dir)?;
        let path = session_dir.join("meta.json");
        fs::write(path, serde_json::to_vec_pretty(meta)?)?;
        Ok(())
    }

    fn open_execution_lock(&self, session_id: &str) -> Result<File, AppError> {
        let path = self.session_dir(session_id).join("execution.lock");
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?)
    }
}

impl Drop for SessionExecutionGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn now_rfc3339() -> Result<String, AppError> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

pub fn new_id() -> String {
    Ulid::new().to_string().to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use tempfile::TempDir;

    use super::*;
    use crate::types::MessageRole;

    #[test]
    fn read_only_append_holds_execution_lock_while_commit_is_in_progress() {
        let temp = TempDir::new().expect("tempdir");
        let config = test_config(temp.path());
        let store = SessionStore::new(&config);
        store.ensure_root().expect("ensure sessions");
        let session = store.create_session().expect("create session");
        let session_id = session.session_id.clone();
        let expected_revision = session.revision;

        let record = test_record("run-1", "hello");
        let commit = SessionCommit {
            expected_revision,
            char_count_delta: record.char_count(),
            records: vec![record],
            bind_agent_name: None,
            bind_model: None,
            bind_effort: None,
            bind_cwd: None,
            bind_initial_role: None,
        };

        let ready = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let store_clone = store.clone();
        let thread_session_id = session_id.clone();
        let ready_clone = Arc::clone(&ready);
        let release_clone = Arc::clone(&release);
        let handle = thread::spawn(move || {
            store_clone.append_run_inner(&thread_session_id, commit, None, || {
                ready_clone.wait();
                release_clone.wait();
                Ok(())
            })
        });

        ready.wait();
        let blocked = store.acquire_execution_lock(&session_id, expected_revision);
        release.wait();

        let err = blocked.expect_err("mutating run should be blocked during read-only append");
        assert!(matches!(err, AppError::SessionConflict(_)));

        let meta = handle
            .join()
            .expect("join append thread")
            .expect("append succeeds");
        assert_eq!(meta.revision, 1);
        assert_eq!(meta.char_count, 5);

        let messages = store.load_messages(&session_id).expect("load messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("hello"));

        let guard = store
            .acquire_execution_lock(&session_id, meta.revision)
            .expect("execution lock released after append");
        drop(guard);
    }

    fn test_config(root: &Path) -> GlobalConfig {
        GlobalConfig {
            sessions_dir: root.join("sessions"),
            shell: "/bin/bash".to_string(),
            shell_args: vec!["-lc".to_string()],
            max_stdin_bytes: 1024,
            artifact_preview_bytes: 256,
            catastrophic_output_bytes: 4096,
            api_key: None,
            api_key_env: None,
            source_path: None,
        }
    }

    fn test_record(run_id: &str, content: &str) -> TranscriptRecord {
        TranscriptRecord {
            v: 1,
            ts: now_rfc3339().expect("timestamp"),
            run_id: run_id.to_string(),
            role: MessageRole::User,
            content: Some(content.to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        }
    }
}
