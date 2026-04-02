use std::{env, ffi::OsString, path::PathBuf};

use lexopt::Parser;

use crate::{error::AppError, types::Effort};

const USAGE: &str = "usage:
  headless version
  headless agent list
  headless role list
  headless session new
  headless session list
  headless session show <id>
  headless session stop <id>
  headless --session <id|new> --agent <name> [--role <name>] [--model <name>] [--effort <none|minimal|low|medium|high|xhigh>] [--plan] [--cwd <path>] [--verbose] [--debug] \"prompt\"";

#[derive(Debug, Clone)]
pub enum Command {
    Version,
    AgentList,
    RoleList,
    SessionNew,
    SessionList,
    SessionShow { id: String },
    SessionStop { id: String },
    Run(RunArgs),
}

#[derive(Debug, Clone)]
pub struct RunArgs {
    pub session: SessionArg,
    pub agent: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub plan: Option<bool>,
    pub cwd: Option<PathBuf>,
    pub verbose: bool,
    pub debug: bool,
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub enum SessionArg {
    New,
    Existing(String),
}

pub fn parse_from_env() -> Result<Command, AppError> {
    parse_from_args(env::args_os().skip(1))
}

pub fn parse_from_args<I>(args: I) -> Result<Command, AppError>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if args.is_empty() {
        return Err(AppError::Usage(USAGE.to_string()));
    }

    match args[0].to_str() {
        Some("version") => {
            if args.len() == 1 {
                Ok(Command::Version)
            } else {
                Err(AppError::Usage(USAGE.to_string()))
            }
        }
        Some("agent") => parse_simple_list("agent", &args, Command::AgentList),
        Some("role") => parse_simple_list("role", &args, Command::RoleList),
        Some("session") => parse_session_subcommand(&args),
        Some("--help") | Some("-h") | Some("help") => Err(AppError::Usage(USAGE.to_string())),
        _ => parse_run_args(args),
    }
}

fn parse_simple_list(
    prefix: &str,
    args: &[OsString],
    command: Command,
) -> Result<Command, AppError> {
    if args.len() == 2 && args[1].to_str() == Some("list") {
        Ok(command)
    } else {
        Err(AppError::Usage(format!(
            "invalid `{prefix}` command\n\n{USAGE}"
        )))
    }
}

fn parse_session_subcommand(args: &[OsString]) -> Result<Command, AppError> {
    match (
        args.get(1).and_then(|value| value.to_str()),
        args.get(2),
        args.len(),
    ) {
        (Some("new"), None, 2) => Ok(Command::SessionNew),
        (Some("list"), None, 2) => Ok(Command::SessionList),
        (Some("show"), Some(id), 3) => Ok(Command::SessionShow {
            id: id.to_string_lossy().to_string(),
        }),
        (Some("stop"), Some(id), 3) => Ok(Command::SessionStop {
            id: id.to_string_lossy().to_string(),
        }),
        _ => Err(AppError::Usage(format!(
            "invalid `session` command\n\n{USAGE}"
        ))),
    }
}

fn parse_run_args(args: Vec<OsString>) -> Result<Command, AppError> {
    let mut parser = Parser::from_args(args);
    let mut session = None;
    let mut agent = None;
    let mut role = None;
    let mut model = None;
    let mut effort = None;
    let mut plan = None;
    let mut cwd = None;
    let mut verbose = false;
    let mut debug = false;
    let mut prompt = None;

    while let Some(arg) = parser.next()? {
        match arg {
            lexopt::Arg::Long("session") => {
                let value = parser.value()?.to_string_lossy().to_string();
                session = Some(if value == "new" {
                    SessionArg::New
                } else {
                    SessionArg::Existing(value)
                });
            }
            lexopt::Arg::Long("agent") => {
                agent = Some(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("role") => {
                role = Some(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("model") => {
                model = Some(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("effort") => {
                effort = Some(parser.value()?.to_string_lossy().parse()?);
            }
            lexopt::Arg::Long("plan") => {
                plan = Some(true);
            }
            lexopt::Arg::Long("cwd") => {
                cwd = Some(PathBuf::from(parser.value()?));
            }
            lexopt::Arg::Long("verbose") => {
                verbose = true;
            }
            lexopt::Arg::Long("debug") => {
                debug = true;
            }
            lexopt::Arg::Value(value) => {
                if prompt.is_some() {
                    return Err(AppError::Usage(format!(
                        "expected a single prompt argument\n\n{USAGE}"
                    )));
                }
                prompt = Some(value.to_string_lossy().to_string());
            }
            _ => {
                return Err(AppError::Usage(USAGE.to_string()));
            }
        }
    }

    let session =
        session.ok_or_else(|| AppError::Usage(format!("missing --session\n\n{USAGE}")))?;
    if matches!(session, SessionArg::New) && agent.is_none() {
        return Err(AppError::Usage(
            "new sessions require --agent when using the run command".to_string(),
        ));
    }
    let prompt = prompt.ok_or_else(|| AppError::Usage(format!("missing prompt\n\n{USAGE}")))?;

    Ok(Command::Run(RunArgs {
        session,
        agent,
        role,
        model,
        effort,
        plan,
        cwd,
        verbose,
        debug,
        prompt,
    }))
}
