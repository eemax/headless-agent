use std::{
    env,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
    sync::atomic::AtomicBool,
};

use crate::{
    agent::r#loop::{AgentRunContext, run_agent_loop},
    agent_def::LoadedAgent,
    artifact::create_run_dir,
    cli::{self, Command, RunArgs, SessionArg},
    config::{GlobalConfig, HeadlessRoots},
    error::AppError,
    prompt::assemble_prompt,
    role_def::LoadedRole,
    session::{SessionCommit, SessionStore, new_id, now_rfc3339},
    types::{MessageRole, RunOutcome, RunResult, SessionMeta, TranscriptRecord},
};

struct BoundRunValues<'a> {
    agent_name: &'a str,
    model: &'a str,
    effort: crate::types::Effort,
    cwd: &'a std::path::Path,
    role_name: Option<&'a str>,
}

pub fn run_from_env(interrupted: Arc<AtomicBool>) -> Result<(), AppError> {
    let command = cli::parse_from_env()?;
    let output = run(command, interrupted)?;
    let mut stderr = io::stderr().lock();
    for line in output.stderr {
        stderr.write_all(line.as_bytes())?;
        stderr.write_all(b"\n")?;
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.stdout.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct AppOutput {
    pub stdout: String,
    pub stderr: Vec<String>,
}

pub fn run(command: Command, interrupted: Arc<AtomicBool>) -> Result<AppOutput, AppError> {
    let roots = HeadlessRoots::discover();
    let config = GlobalConfig::load(&roots)?;
    let store = SessionStore::new(&config);
    store.ensure_root()?;

    match command {
        Command::Version => Ok(AppOutput {
            stdout: format!("{}\n", env!("CARGO_PKG_VERSION")),
            stderr: Vec::new(),
        }),
        Command::AgentList => Ok(AppOutput {
            stdout: format_lines(roots.list_agents()?),
            stderr: Vec::new(),
        }),
        Command::RoleList => Ok(AppOutput {
            stdout: format_lines(roots.list_roles()?),
            stderr: Vec::new(),
        }),
        Command::SessionNew => {
            let session = store.create_session()?;
            Ok(AppOutput {
                stdout: format!("{}\n", session.session_id),
                stderr: Vec::new(),
            })
        }
        Command::SessionList => {
            let output = store
                .list_sessions()?
                .into_iter()
                .map(|session| session.session_id)
                .collect::<Vec<_>>();
            Ok(AppOutput {
                stdout: format_lines(output),
                stderr: Vec::new(),
            })
        }
        Command::SessionShow { id } => {
            let meta = store.load_meta(&id)?;
            Ok(AppOutput {
                stdout: format!("{}\n", serde_json::to_string_pretty(&meta)?),
                stderr: Vec::new(),
            })
        }
        Command::SessionStop { id } => {
            store.stop_session(&id)?;
            Ok(AppOutput::default())
        }
        Command::Run(args) => run_prompt(args, roots, config, store, interrupted),
    }
}

fn run_prompt(
    args: RunArgs,
    roots: HeadlessRoots,
    config: GlobalConfig,
    store: SessionStore,
    interrupted: Arc<AtomicBool>,
) -> Result<AppOutput, AppError> {
    let mut stderr = Vec::new();
    let current_dir = env::current_dir()
        .map_err(|err| AppError::Runtime(format!("failed to read current directory: {err}")))?;

    let (session_meta, session_id, created_new_session) = match &args.session {
        SessionArg::New => {
            let created = store.create_session()?;
            stderr.push(format!("created session {}", created.session_id));
            (created.clone(), created.session_id.clone(), true)
        }
        SessionArg::Existing(id) => {
            let meta = store.load_meta(id)?;
            (meta, id.clone(), false)
        }
    };

    if session_meta.stopped_at.is_some() {
        return Err(AppError::Session(format!(
            "session `{}` has been stopped and cannot accept new runs",
            session_id
        )));
    }

    let agent_name = match (&args.agent, &session_meta.agent_name) {
        (Some(agent), Some(bound)) if agent != bound => {
            return Err(AppError::Session(format!(
                "session `{session_id}` is bound to agent `{bound}`, not `{agent}`"
            )));
        }
        (Some(agent), _) => agent.clone(),
        (None, Some(bound)) => bound.clone(),
        (None, None) => {
            return Err(AppError::Usage(
                "this session is not yet bound; please provide --agent".to_string(),
            ));
        }
    };

    let agent = LoadedAgent::load(&roots, &agent_name)?;
    let role_name = args
        .role
        .clone()
        .or_else(|| session_meta.initial_role.clone());
    let role = role_name
        .as_ref()
        .map(|name| LoadedRole::load(&roots, name))
        .transpose()?;

    let model = args
        .model
        .clone()
        .or_else(|| session_meta.model.clone())
        .unwrap_or_else(|| agent.def.default_model.clone());
    let effort = args
        .effort
        .or(session_meta.effort)
        .unwrap_or(agent.def.default_effort);
    let plan_mode = args.plan;
    let effective_cwd = args
        .cwd
        .clone()
        .or_else(|| session_meta.cwd.as_ref().map(PathBuf::from))
        .unwrap_or(current_dir);
    let stdin = read_stdin_if_present(config.max_stdin_bytes)?;

    let history = store.load_messages(&session_id)?;
    let prompt = assemble_prompt(
        &agent,
        role.as_ref(),
        &history,
        &args.prompt,
        stdin.as_deref(),
    )?;
    let compaction_at_tokens = agent.def.compaction_at_tokens.unwrap_or(180_000);
    if prompt.estimated_tokens > compaction_at_tokens {
        return Err(AppError::Runtime(format!(
            "prompt assembly exceeded compaction threshold ({compaction_at_tokens} tokens); compaction is not implemented in this first pass"
        )));
    }

    let session_dir = store.session_dir(&session_id);
    let run_id = new_id();
    let user_records = build_user_records(&run_id, &args.prompt, stdin.as_deref())?;
    let run_dir = create_run_dir(&session_dir, &run_id)?;
    let run_context = AgentRunContext {
        session_id: session_id.clone(),
        run_id: run_id.clone(),
        run_dir: run_dir.clone(),
        session_revision: session_meta.revision,
        model: model.clone(),
        cwd: effective_cwd.clone(),
        effort,
        plan_mode,
        prompt_messages: prompt.messages,
        agent: agent.clone(),
        config: config.clone(),
        session_store: store.clone(),
        api_key: resolve_api_key(&agent, &config)?,
        interrupted,
    };
    let RunOutcome {
        result,
        execution_guard,
    } = run_agent_loop(run_context)?;
    let final_text = result.final_text.clone();
    let termination = result.termination.clone();
    let bound_values = BoundRunValues {
        agent_name: &agent_name,
        model: &model,
        effort,
        cwd: &effective_cwd,
        role_name: role_name.as_deref(),
    };
    let commit = build_commit(&session_meta, result, user_records, &bound_values);
    store.append_run(&session_id, commit, execution_guard.as_ref())?;

    if let Some(err) = termination.into_error() {
        return Err(err);
    }

    if args.verbose || args.debug {
        stderr.push(format!(
            "session={} model={} effort={} cwd={}",
            session_id,
            model,
            effort,
            effective_cwd.display()
        ));
        if created_new_session {
            stderr.push("new session initialized".to_string());
        }
    }

    Ok(AppOutput {
        stdout: final_text,
        stderr,
    })
}

fn build_commit(
    session_meta: &SessionMeta,
    mut result: RunResult,
    mut user_records: Vec<TranscriptRecord>,
    bound_values: &BoundRunValues<'_>,
) -> SessionCommit {
    user_records.append(&mut result.records);
    let records = user_records;
    let char_count_delta = records.iter().map(|record| record.char_count()).sum();
    SessionCommit {
        expected_revision: session_meta.revision,
        char_count_delta,
        records,
        bind_agent_name: session_meta
            .agent_name
            .is_none()
            .then(|| bound_values.agent_name.to_string()),
        bind_model: session_meta
            .model
            .is_none()
            .then(|| bound_values.model.to_string()),
        bind_effort: session_meta.effort.is_none().then_some(bound_values.effort),
        bind_cwd: session_meta
            .cwd
            .is_none()
            .then(|| bound_values.cwd.display().to_string()),
        bind_initial_role: session_meta
            .initial_role
            .is_none()
            .then(|| bound_values.role_name.map(ToOwned::to_owned))
            .flatten(),
    }
}

fn build_user_records(
    run_id: &str,
    prompt: &str,
    stdin: Option<&str>,
) -> Result<Vec<TranscriptRecord>, AppError> {
    let mut records = vec![TranscriptRecord {
        v: 1,
        ts: now_rfc3339()?,
        run_id: run_id.to_string(),
        role: MessageRole::User,
        content: Some(prompt.to_string()),
        name: None,
        tool_call_id: None,
        preview: None,
        artifact: None,
        tool_calls: None,
    }];
    if let Some(stdin) = stdin {
        records.push(TranscriptRecord {
            v: 1,
            ts: now_rfc3339()?,
            run_id: run_id.to_string(),
            role: MessageRole::User,
            content: Some(stdin.to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        });
    }
    Ok(records)
}

fn resolve_api_key(agent: &LoadedAgent, config: &GlobalConfig) -> Result<String, AppError> {
    if let Some(api_key) = &agent.def.api_key
        && !api_key.is_empty()
    {
        return Ok(api_key.clone());
    }
    if let Some(env_name) = &agent.def.api_key_env
        && let Ok(api_key) = env::var(env_name)
        && !api_key.is_empty()
    {
        return Ok(api_key);
    }
    if let Some(api_key) = &config.api_key
        && !api_key.is_empty()
    {
        return Ok(api_key.clone());
    }
    if let Some(env_name) = &config.api_key_env
        && let Ok(api_key) = env::var(env_name)
        && !api_key.is_empty()
    {
        return Ok(api_key);
    }
    if let Ok(api_key) = env::var("OPENROUTER_API_KEY")
        && !api_key.is_empty()
    {
        return Ok(api_key);
    }
    Err(AppError::Config(
        "missing OpenRouter API key; set it in the agent file, config.toml, or OPENROUTER_API_KEY"
            .to_string(),
    ))
}

fn read_stdin_if_present(limit: usize) -> Result<Option<String>, AppError> {
    if io::stdin().is_terminal() {
        return Ok(None);
    }
    let mut buffer = Vec::new();
    io::stdin()
        .take(limit as u64 + 1)
        .read_to_end(&mut buffer)?;
    if buffer.is_empty() {
        return Ok(None);
    }
    if buffer.len() > limit {
        return Err(AppError::Runtime(format!(
            "stdin exceeded the configured maximum of {limit} bytes"
        )));
    }
    Ok(Some(String::from_utf8_lossy(&buffer).to_string()))
}

fn format_lines(lines: Vec<String>) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}
