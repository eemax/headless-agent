use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    path::Path,
    path::PathBuf,
    sync::Arc,
    sync::atomic::AtomicBool,
};

use serde::Serialize;

use crate::{
    agent::r#loop::{AgentRunContext, run_agent_loop},
    agent_def::LoadedAgent,
    artifact::create_run_dir,
    cli::{self, Command, RunArgs, SessionArg},
    config::{GlobalConfig, HeadlessRoots},
    error::AppError,
    progress::{BufferedStderrSink, ProgressReporter, StderrSink, WriterStderrSink},
    prompt::assemble_prompt,
    prompt_def::LoadedPrompt,
    role_def::LoadedRole,
    session::{self, SessionCommit, SessionStore, new_id, now_rfc3339},
    tools::{web_fetch, web_search},
    types::{
        LoopTermination, MessageRole, ProviderUsageSummary, RunOutcome, RunResult, SessionMeta,
        TranscriptRecord,
    },
};

#[derive(Debug, Serialize)]
struct PersistedRunOutcome<'a> {
    termination: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    termination_message: Option<&'a str>,
    final_text: &'a str,
    total_prompt_tokens: usize,
    total_completion_tokens: usize,
    provider_usage_summary: &'a ProviderUsageSummary,
    artifacts: &'a [crate::types::ArtifactRef],
}

pub fn run_from_env(interrupted: Arc<AtomicBool>) -> Result<(), AppError> {
    let command = cli::parse_from_env()?;
    if matches!(&command, Command::Run(args) if args.verbose) {
        let (roots, config, store) = prepare_runtime()?;
        let mut stderr = io::stderr().lock();
        let mut stderr_sink = WriterStderrSink::new(&mut stderr);
        let stdout = match command {
            Command::Run(args) => {
                run_prompt(args, roots, config, store, interrupted, &mut stderr_sink)?
            }
            _ => unreachable!("verbose live sink is only used for run commands"),
        };
        let mut stdout_handle = io::stdout().lock();
        stdout_handle.write_all(stdout.as_bytes())?;
        stdout_handle.flush()?;
        return Ok(());
    }

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

#[derive(Debug, Clone, Copy)]
struct SessionSettingUpdates<'a> {
    role_name: Option<Option<&'a str>>,
    model: Option<&'a str>,
    effort: Option<crate::types::Effort>,
    cwd: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy)]
struct CommitInput<'a> {
    agent_name: &'a str,
    updates: SessionSettingUpdates<'a>,
}

pub fn run(command: Command, interrupted: Arc<AtomicBool>) -> Result<AppOutput, AppError> {
    let (roots, config, store) = prepare_runtime()?;
    run_with_runtime(command, roots, config, store, interrupted)
}

fn prepare_runtime() -> Result<(HeadlessRoots, GlobalConfig, SessionStore), AppError> {
    let roots = HeadlessRoots::discover();
    let config = GlobalConfig::load(&roots)?;
    let store = SessionStore::new(&config);
    store.ensure_root()?;
    Ok((roots, config, store))
}

fn run_with_runtime(
    command: Command,
    roots: HeadlessRoots,
    config: GlobalConfig,
    store: SessionStore,
    interrupted: Arc<AtomicBool>,
) -> Result<AppOutput, AppError> {
    match command {
        Command::Version => Ok(AppOutput {
            stdout: format!("{}\n", env!("CARGO_PKG_VERSION")),
            stderr: Vec::new(),
        }),
        Command::WebFetch { url } => Ok(AppOutput {
            stdout: web_fetch::render_cli_output(&web_fetch::fetch_url(&url)),
            stderr: Vec::new(),
        }),
        Command::WebSearch {
            query,
            search_type,
            num_results,
            published_within_days,
            include_domains,
            exclude_domains,
        } => {
            let result = web_search::search_cli(
                &query,
                search_type.as_deref(),
                num_results,
                published_within_days,
                &include_domains,
                &exclude_domains,
            )?;
            Ok(AppOutput {
                stdout: web_search::render_cli_output(&result),
                stderr: Vec::new(),
            })
        }
        Command::AgentList => Ok(AppOutput {
            stdout: format_lines(roots.list_agents()?),
            stderr: Vec::new(),
        }),
        Command::RoleList => Ok(AppOutput {
            stdout: format_lines(roots.list_roles()?),
            stderr: Vec::new(),
        }),
        Command::PromptList => Ok(AppOutput {
            stdout: format_lines(roots.list_prompts()?),
            stderr: Vec::new(),
        }),
        Command::SessionNew => {
            let session = store.create_session()?;
            Ok(AppOutput {
                stdout: format!("{}\n", session.session_id),
                stderr: Vec::new(),
            })
        }
        Command::SessionLast => {
            let session = store.last_active_session()?;
            Ok(AppOutput {
                stdout: format!("{}\n", session.session_id),
                stderr: Vec::new(),
            })
        }
        Command::SessionList => {
            let output = store.list_session_ids()?;
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
        Command::Run(args) => {
            let mut stderr_sink = BufferedStderrSink::default();
            let stdout = run_prompt(args, roots, config, store, interrupted, &mut stderr_sink)?;
            Ok(AppOutput {
                stdout,
                stderr: stderr_sink.into_lines(),
            })
        }
    }
}

fn run_prompt(
    args: RunArgs,
    roots: HeadlessRoots,
    config: GlobalConfig,
    store: SessionStore,
    interrupted: Arc<AtomicBool>,
    stderr_sink: &mut dyn StderrSink,
) -> Result<String, AppError> {
    let mut progress = ProgressReporter::new(args.verbose, stderr_sink);
    let current_dir = env::current_dir()
        .map_err(|err| AppError::Runtime(format!("failed to read current directory: {err}")))?;

    let (session_meta, session_id, created_new_session) = match &args.session {
        SessionArg::New => {
            let created = store.create_session()?;
            progress.emit_line(format!("created session {}", created.session_id))?;
            (created.clone(), created.session_id.clone(), true)
        }
        SessionArg::Last => {
            let resolved = store.last_active_session()?;
            let resolved_id = resolved.session_id.clone();
            match args.fork {
                true => {
                    let forked = store.fork_session(&resolved_id)?;
                    progress.emit_line(format!(
                        "forked session {} from {}",
                        forked.session_id, resolved_id
                    ))?;
                    (forked.clone(), forked.session_id.clone(), false)
                }
                false => (resolved, resolved_id, false),
            }
        }
        SessionArg::Existing(id) => match args.fork {
            true => {
                let forked = store.fork_session(id)?;
                progress.emit_line(format!("forked session {} from {}", forked.session_id, id))?;
                (forked.clone(), forked.session_id.clone(), false)
            }
            false => {
                let meta = store.load_meta(id)?;
                (meta, id.clone(), false)
            }
        },
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
        (None, None) => config.default_agent.clone().ok_or_else(|| {
            AppError::Usage(
                "this session is not yet bound; please provide --agent or set default_agent in config.toml"
                    .to_string(),
            )
        })?,
    };

    let agent = LoadedAgent::load(&roots, &agent_name)?;
    let session_updates = SessionSettingUpdates {
        role_name: match (args.no_role, args.role.as_deref()) {
            (true, None) => Some(None),
            (false, Some(role_name)) => Some(Some(role_name)),
            (false, None) => None,
            (true, Some(_)) => unreachable!("cli parser rejects --role with --no-role"),
        },
        model: args.model.as_deref(),
        effort: args.effort,
        cwd: args.cwd.as_deref(),
    };
    let role_change_note =
        sticky_role_change_note(session_meta.role_name.as_deref(), session_updates.role_name);
    let role_name = match session_updates.role_name {
        Some(Some(role_name)) => Some(role_name.to_string()),
        Some(None) => None,
        None => session_meta.role_name.clone(),
    };
    let role = role_name
        .as_ref()
        .map(|name| LoadedRole::load(&roots, name))
        .transpose()?;
    let named_prompt = args
        .prompt_name
        .as_ref()
        .map(|name| LoadedPrompt::load(&roots, name))
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
    let assembled_prompt = assemble_prompt(
        &agent,
        role.as_ref(),
        named_prompt.as_ref(),
        &history,
        &args.message,
        stdin.as_deref(),
    )?;
    let compaction_at_tokens = agent.def.compaction_at_tokens.unwrap_or(180_000);
    if assembled_prompt.estimated_tokens > compaction_at_tokens {
        return Err(AppError::Runtime(format!(
            "prompt assembly exceeded compaction threshold ({compaction_at_tokens} tokens); compaction is not implemented in this first pass"
        )));
    }

    let session_dir = store.session_dir(&session_id);
    let run_id = new_id();
    let user_records = build_user_records(
        &run_id,
        &assembled_prompt.current_user_message,
        stdin.as_deref(),
    )?;
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
        prompt_messages: assembled_prompt.messages,
        agent: agent.clone(),
        config: config.clone(),
        session_store: store.clone(),
        api_key: resolve_api_key(&agent, &config)?,
        interrupted,
    };
    let RunOutcome {
        result,
        execution_guard,
    } = run_agent_loop(run_context, &mut progress)?;
    let final_text = result.final_text.clone();
    let termination = result.termination.clone();
    persist_run_trace(&run_dir, &user_records, &result)?;
    let commit = build_commit(
        &session_meta,
        &result,
        &user_records,
        CommitInput {
            agent_name: &agent_name,
            updates: session_updates,
        },
    );
    store.append_run(&session_id, commit, execution_guard.as_ref())?;

    if let Some(err) = termination.into_error() {
        return Err(err);
    }

    if let Some(note) = role_change_note {
        progress.emit_line(note)?;
    }
    if args.debug {
        progress.emit_line(format!(
            "session={} model={} effort={} cwd={}",
            session_id,
            model,
            effort,
            effective_cwd.display()
        ))?;
        if created_new_session {
            progress.emit_line("new session initialized")?;
        }
    }

    Ok(final_text)
}

fn build_commit(
    session_meta: &SessionMeta,
    result: &RunResult,
    user_records: &[TranscriptRecord],
    input: CommitInput<'_>,
) -> SessionCommit {
    let records = project_session_history(user_records, result);
    let char_count_delta = records.iter().map(|record| record.char_count()).sum();
    SessionCommit {
        expected_revision: session_meta.revision,
        char_count_delta,
        records,
        bind_agent_name: session_meta
            .agent_name
            .is_none()
            .then(|| input.agent_name.to_string()),
        update_model: input.updates.model.map(ToOwned::to_owned),
        update_effort: input.updates.effort,
        update_cwd: input.updates.cwd.map(|cwd| cwd.display().to_string()),
        update_role_name: input
            .updates
            .role_name
            .map(|value| value.map(ToOwned::to_owned)),
    }
}

fn sticky_role_change_note(
    previous_role_name: Option<&str>,
    role_name_update: Option<Option<&str>>,
) -> Option<String> {
    match (previous_role_name, role_name_update) {
        (_, None) | (None, Some(None)) => None,
        (Some(previous), Some(Some(next))) if previous == next => None,
        (None, Some(Some(next))) => Some(format!("sticky role set to `{next}`")),
        (Some(previous), Some(Some(next))) => {
            Some(format!("sticky role changed from `{previous}` to `{next}`"))
        }
        (Some(previous), Some(None)) => Some(format!("sticky role cleared (was `{previous}`)")),
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

fn persist_run_trace(
    run_dir: &Path,
    user_records: &[TranscriptRecord],
    result: &RunResult,
) -> Result<(), AppError> {
    let mut records = user_records.to_vec();
    records.extend(result.records.iter().cloned());
    session::jsonl::append_records(&run_dir.join("transcript.jsonl"), &records)?;
    session::jsonl::append_records(&run_dir.join("provider.jsonl"), &result.provider_steps)?;

    let outcome = PersistedRunOutcome {
        termination: termination_kind(&result.termination),
        termination_message: termination_message(&result.termination),
        final_text: &result.final_text,
        total_prompt_tokens: result.total_prompt_tokens,
        total_completion_tokens: result.total_completion_tokens,
        provider_usage_summary: &result.provider_usage_summary,
        artifacts: &result.artifacts.paths,
    };
    fs::write(
        run_dir.join("outcome.json"),
        serde_json::to_vec_pretty(&outcome)?,
    )?;
    Ok(())
}

fn project_session_history(
    user_records: &[TranscriptRecord],
    result: &RunResult,
) -> Vec<TranscriptRecord> {
    let mut records = user_records.to_vec();
    if matches!(&result.termination, LoopTermination::Complete)
        && let Some(final_assistant) = result.records.iter().rev().find(|record| {
            record.role == MessageRole::Assistant
                && record
                    .tool_calls
                    .as_ref()
                    .is_none_or(|calls| calls.is_empty())
        })
    {
        records.push(final_assistant.clone());
    }
    records
}

fn termination_kind(termination: &LoopTermination) -> &'static str {
    match termination {
        LoopTermination::Complete => "complete",
        LoopTermination::Timeout(_) => "timeout",
        LoopTermination::Error(_) => "error",
    }
}

fn termination_message(termination: &LoopTermination) -> Option<&str> {
    match termination {
        LoopTermination::Complete => None,
        LoopTermination::Timeout(message) | LoopTermination::Error(message) => Some(message),
    }
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
