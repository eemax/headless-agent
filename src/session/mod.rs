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
        self.initialize_session(&meta)?;
        Ok(meta)
    }

    pub fn fork_session(&self, source_session_id: &str) -> Result<SessionMeta, AppError> {
        self.ensure_root()?;
        let lock_path = self.session_dir(source_session_id).join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => {
                    AppError::Session(format!("session `{source_session_id}` does not exist"))
                }
                _ => error.into(),
            })?;
        lock.lock_exclusive()?;

        let snapshot = (|| -> Result<(SessionMeta, Vec<u8>), AppError> {
            let meta = self.load_meta(source_session_id)?;
            let messages = fs::read(self.session_dir(source_session_id).join("messages.jsonl"))?;
            Ok((meta, messages))
        })();

        match snapshot {
            Ok((source_meta, messages)) => {
                lock.unlock()?;
                let session_id = new_id();
                let now = now_rfc3339()?;
                let meta = SessionMeta {
                    session_id: session_id.clone(),
                    created_at: now.clone(),
                    updated_at: now,
                    stopped_at: None,
                    revision: source_meta.revision,
                    char_count: source_meta.char_count,
                    agent_name: source_meta.agent_name,
                    model: source_meta.model,
                    initial_role: source_meta.initial_role,
                    cwd: source_meta.cwd,
                    effort: source_meta.effort,
                };
                self.initialize_session_with_messages(&meta, &messages)?;
                Ok(meta)
            }
            Err(error) => {
                let _ = lock.unlock();
                Err(error)
            }
        }
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

    pub fn last_active_session(&self) -> Result<SessionMeta, AppError> {
        let mut selected: Option<(OffsetDateTime, SessionMeta)> = None;
        for meta in self.list_sessions()? {
            if meta.stopped_at.is_some() || meta.revision == 0 {
                continue;
            }
            let updated_at = OffsetDateTime::parse(&meta.updated_at, &Rfc3339)?;
            let replace = match selected.as_ref() {
                Some((best_updated_at, best_meta)) => {
                    updated_at > *best_updated_at
                        || (updated_at == *best_updated_at
                            && meta.session_id > best_meta.session_id)
                }
                None => true,
            };
            if replace {
                selected = Some((updated_at, meta));
            }
        }

        selected.map(|(_, meta)| meta).ok_or_else(|| {
            AppError::Session(
                "no active session with committed history was found; start one with `headless new`"
                    .to_string(),
            )
        })
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
        }
        if meta.model.is_none() {
            meta.model = commit.bind_model;
        }
        if meta.effort.is_none() {
            meta.effort = commit.bind_effort;
        }
        if meta.cwd.is_none() {
            meta.cwd = commit.bind_cwd;
        }
        if meta.initial_role.is_none() {
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

    fn initialize_session(&self, meta: &SessionMeta) -> Result<(), AppError> {
        self.initialize_session_with_messages(meta, b"")
    }

    fn initialize_session_with_messages(
        &self,
        meta: &SessionMeta,
        messages: &[u8],
    ) -> Result<(), AppError> {
        let session_dir = self.session_dir(&meta.session_id);
        let setup = (|| -> Result<(), AppError> {
            fs::create_dir_all(session_dir.join("runs"))?;
            fs::write(session_dir.join("messages.jsonl"), messages)?;
            File::create(session_dir.join("lock"))?;
            File::create(session_dir.join("execution.lock"))?;
            self.write_meta(meta)?;
            Ok(())
        })();
        if let Err(error) = setup {
            let _ = fs::remove_dir_all(&session_dir);
            return Err(error);
        }
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
            default_agent: None,
            api_key: None,
            api_key_env: None,
            source_path: None,
        }
    }

    #[test]
    fn fork_session_copies_persisted_state_and_reopens_the_branch() {
        let temp = TempDir::new().expect("tempdir");
        let config = test_config(temp.path());
        let store = SessionStore::new(&config);
        store.ensure_root().expect("ensure sessions");

        let session = store.create_session().expect("create session");
        let session_id = session.session_id.clone();
        let record = test_record("run-1", "hello");
        let commit = SessionCommit {
            expected_revision: session.revision,
            char_count_delta: record.char_count(),
            records: vec![record],
            bind_agent_name: Some("coder".to_string()),
            bind_model: Some("model/one".to_string()),
            bind_effort: Some(Effort::High),
            bind_cwd: Some("/tmp/worktree".to_string()),
            bind_initial_role: Some("auditor".to_string()),
        };
        store
            .append_run(&session_id, commit, None)
            .expect("append run");
        store.stop_session(&session_id).expect("stop source");

        let forked = store.fork_session(&session_id).expect("fork session");

        assert_ne!(forked.session_id, session_id);
        assert_eq!(forked.revision, 1);
        assert_eq!(forked.char_count, 5);
        assert_eq!(forked.agent_name.as_deref(), Some("coder"));
        assert_eq!(forked.model.as_deref(), Some("model/one"));
        assert_eq!(forked.effort, Some(Effort::High));
        assert_eq!(forked.cwd.as_deref(), Some("/tmp/worktree"));
        assert_eq!(forked.initial_role.as_deref(), Some("auditor"));
        assert!(forked.stopped_at.is_none());
        assert_eq!(
            store
                .load_messages(&forked.session_id)
                .expect("forked messages")
                .first()
                .and_then(|record| record.content.as_deref()),
            Some("hello")
        );
        assert!(
            fs::read_dir(store.session_dir(&forked.session_id).join("runs"))
                .expect("read forked runs")
                .next()
                .is_none()
        );
    }

    #[test]
    fn fork_missing_session_returns_session_error() {
        let temp = TempDir::new().expect("tempdir");
        let config = test_config(temp.path());
        let store = SessionStore::new(&config);
        store.ensure_root().expect("ensure sessions");

        let error = store.fork_session("missing").expect_err("missing session");
        assert!(matches!(error, AppError::Session(_)));
        assert_eq!(
            error.to_string(),
            "session `missing` does not exist".to_string()
        );
    }

    #[test]
    fn last_active_session_prefers_newest_non_stopped_committed_session() {
        let temp = TempDir::new().expect("tempdir");
        let config = test_config(temp.path());
        let store = SessionStore::new(&config);
        store.ensure_root().expect("ensure sessions");

        store
            .initialize_session(&test_meta(
                "empty",
                "2024-01-02T00:00:00Z",
                "2024-01-04T00:00:00Z",
                0,
                None,
            ))
            .expect("initialize empty session");
        store
            .initialize_session(&test_meta(
                "stopped",
                "2024-01-02T00:00:00Z",
                "2024-01-05T00:00:00Z",
                2,
                Some("2024-01-05T00:00:01Z"),
            ))
            .expect("initialize stopped session");
        store
            .initialize_session(&test_meta(
                "older-active",
                "2024-01-02T00:00:00Z",
                "2024-01-06T00:00:00Z",
                1,
                None,
            ))
            .expect("initialize older active session");
        store
            .initialize_session(&test_meta(
                "newest-active",
                "2024-01-02T00:00:00Z",
                "2024-01-07T00:00:00Z",
                3,
                None,
            ))
            .expect("initialize newest active session");

        let resolved = store
            .last_active_session()
            .expect("resolve newest active session");
        assert_eq!(resolved.session_id, "newest-active");
    }

    #[test]
    fn last_active_session_errors_when_no_active_committed_session_exists() {
        let temp = TempDir::new().expect("tempdir");
        let config = test_config(temp.path());
        let store = SessionStore::new(&config);
        store.ensure_root().expect("ensure sessions");

        store
            .initialize_session(&test_meta(
                "empty",
                "2024-01-02T00:00:00Z",
                "2024-01-04T00:00:00Z",
                0,
                None,
            ))
            .expect("initialize empty session");
        store
            .initialize_session(&test_meta(
                "stopped",
                "2024-01-02T00:00:00Z",
                "2024-01-05T00:00:00Z",
                2,
                Some("2024-01-05T00:00:01Z"),
            ))
            .expect("initialize stopped session");

        let error = store
            .last_active_session()
            .expect_err("no qualifying sessions");
        assert!(matches!(error, AppError::Session(_)));
        assert_eq!(
            error.to_string(),
            "no active session with committed history was found; start one with `headless new`"
        );
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

    fn test_meta(
        session_id: &str,
        created_at: &str,
        updated_at: &str,
        revision: u64,
        stopped_at: Option<&str>,
    ) -> SessionMeta {
        SessionMeta {
            session_id: session_id.to_string(),
            created_at: created_at.to_string(),
            updated_at: updated_at.to_string(),
            stopped_at: stopped_at.map(ToOwned::to_owned),
            revision,
            char_count: 0,
            agent_name: Some("coder".to_string()),
            model: Some("model/one".to_string()),
            initial_role: None,
            cwd: Some("/tmp/worktree".to_string()),
            effort: Some(Effort::Medium),
        }
    }
}
