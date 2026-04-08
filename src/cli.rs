use std::{env, ffi::OsString, path::PathBuf};

use lexopt::Parser;

use crate::{error::AppError, tools::web_search, types::Effort};

const USAGE: &str = "usage:
  headless version
  headless webfetch <url>
  headless websearch [--type <mode>] [--num_results <n>] [--published_within_days <n>] [--include_domains <domain>]... [--exclude_domains <domain>]... <query...>
  headless agent list
  headless role list
  headless prompt list
  headless session new
  headless session last
  headless session list
  headless session show <id>
  headless session stop <id>
  headless new [--agent <name>] [--role <name>|--no-role] [--prompt <name>] [--model <name>] [--effort <none|minimal|low|medium|high|xhigh>] [--plan] [--cwd <path>] [--verbose] [--debug] \"message\"
  headless last [--fork] [--agent <name>] [--role <name>|--no-role] [--prompt <name>] [--model <name>] [--effort <none|minimal|low|medium|high|xhigh>] [--plan] [--cwd <path>] [--verbose] [--debug] \"message\"
  headless --session <id|new|last> [--fork] [--agent <name>] [--role <name>|--no-role] [--prompt <name>] [--model <name>] [--effort <none|minimal|low|medium|high|xhigh>] [--plan] [--cwd <path>] [--verbose] [--debug] \"message\"";

#[derive(Debug, Clone)]
pub enum Command {
    Version,
    WebFetch {
        url: String,
    },
    WebSearch {
        query: String,
        search_type: Option<String>,
        num_results: Option<usize>,
        published_within_days: Option<usize>,
        include_domains: Vec<String>,
        exclude_domains: Vec<String>,
    },
    AgentList,
    RoleList,
    PromptList,
    SessionNew,
    SessionLast,
    SessionList,
    SessionShow {
        id: String,
    },
    SessionStop {
        id: String,
    },
    Run(RunArgs),
}

#[derive(Debug, Clone)]
pub struct RunArgs {
    pub session: SessionArg,
    pub fork: bool,
    pub agent: Option<String>,
    pub role: Option<String>,
    pub no_role: bool,
    pub prompt_name: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub plan: bool,
    pub cwd: Option<PathBuf>,
    pub verbose: bool,
    pub debug: bool,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum SessionArg {
    New,
    Last,
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
        Some("webfetch") => parse_webfetch(&args),
        Some("websearch") => parse_websearch(&args),
        Some("agent") => parse_simple_list("agent", &args, Command::AgentList),
        Some("role") => parse_simple_list("role", &args, Command::RoleList),
        Some("prompt") => parse_simple_list("prompt", &args, Command::PromptList),
        Some("session") => parse_session_subcommand(&args),
        Some("new") => parse_run_args(args.iter().skip(1).cloned(), Some(SessionArg::New)),
        Some("last") => parse_run_args(args.iter().skip(1).cloned(), Some(SessionArg::Last)),
        Some("--help") | Some("-h") | Some("help") => Err(AppError::Usage(USAGE.to_string())),
        _ => parse_run_args(args, None),
    }
}

fn parse_webfetch(args: &[OsString]) -> Result<Command, AppError> {
    if args.len() == 2 {
        Ok(Command::WebFetch {
            url: args[1].to_string_lossy().to_string(),
        })
    } else {
        Err(AppError::Usage(format!(
            "invalid `webfetch` command\n\n{USAGE}"
        )))
    }
}

fn parse_websearch(args: &[OsString]) -> Result<Command, AppError> {
    let mut parser = Parser::from_args(args.iter().skip(1).cloned());
    let mut search_type = None;
    let mut num_results = None;
    let mut published_within_days = None;
    let mut include_domains = Vec::new();
    let mut exclude_domains = Vec::new();
    let mut query_parts = Vec::new();

    while let Some(arg) = parser.next()? {
        match arg {
            lexopt::Arg::Long("type") => {
                let raw = parser.value()?.to_string_lossy().to_string();
                let validated = web_search::validate_search_type(Some(&raw)).map_err(|_| {
                    AppError::Usage(format!(
                        "invalid `--type` value `{raw}`; expected one of auto|neural|deep\n\n{USAGE}"
                    ))
                })?;
                search_type = Some(validated);
            }
            lexopt::Arg::Long("num_results") => {
                let raw = parser.value()?.to_string_lossy().to_string();
                let parsed = raw.parse::<usize>().map_err(|_| {
                    AppError::Usage(format!("invalid `--num_results` value `{raw}`\n\n{USAGE}"))
                })?;
                let validated = web_search::validate_num_results(Some(parsed as u64))
                    .map_err(|error| AppError::Usage(format!("{error}\n\n{USAGE}")))?;
                num_results = Some(validated);
            }
            lexopt::Arg::Long("published_within_days") => {
                let raw = parser.value()?.to_string_lossy().to_string();
                let parsed = raw.parse::<usize>().map_err(|_| {
                    AppError::Usage(format!(
                        "invalid `--published_within_days` value `{raw}`\n\n{USAGE}"
                    ))
                })?;
                let validated = web_search::validate_published_within_days(Some(parsed as u64))
                    .map_err(|error| AppError::Usage(format!("{error}\n\n{USAGE}")))?;
                published_within_days = validated;
            }
            lexopt::Arg::Long("include_domains") => {
                include_domains.push(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("exclude_domains") => {
                exclude_domains.push(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Value(value) => {
                query_parts.push(value.to_string_lossy().to_string());
            }
            _ => {
                return Err(AppError::Usage(format!(
                    "invalid `websearch` command\n\n{USAGE}"
                )));
            }
        }
    }

    if query_parts.is_empty() {
        return Err(AppError::Usage(format!(
            "missing search query for `websearch`\n\n{USAGE}"
        )));
    }

    Ok(Command::WebSearch {
        query: query_parts.join(" "),
        search_type,
        num_results,
        published_within_days,
        include_domains,
        exclude_domains,
    })
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
        (Some("last"), None, 2) => Ok(Command::SessionLast),
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

fn parse_run_args<I>(args: I, implicit_session: Option<SessionArg>) -> Result<Command, AppError>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut parser = Parser::from_args(args);
    let mut session = implicit_session;
    let mut fork = false;
    let mut agent = None;
    let mut role = None;
    let mut no_role = false;
    let mut prompt_name = None;
    let mut model = None;
    let mut effort = None;
    let mut plan = false;
    let mut cwd = None;
    let mut verbose = false;
    let mut debug = false;
    let mut prompt = None;

    while let Some(arg) = parser.next()? {
        match arg {
            lexopt::Arg::Long("session") => {
                if session.is_some() {
                    return Err(AppError::Usage(format!(
                        "session may only be selected once\n\n{USAGE}"
                    )));
                }
                let value = parser.value()?.to_string_lossy().to_string();
                session = Some(parse_session_arg(&value));
            }
            lexopt::Arg::Long("fork") => {
                fork = true;
            }
            lexopt::Arg::Long("agent") => {
                agent = Some(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("role") => {
                if no_role {
                    return Err(AppError::Usage(format!(
                        "--role and --no-role cannot be used together\n\n{USAGE}"
                    )));
                }
                role = Some(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("no-role") => {
                if role.is_some() || no_role {
                    return Err(AppError::Usage(format!(
                        "--role and --no-role cannot be used together\n\n{USAGE}"
                    )));
                }
                no_role = true;
            }
            lexopt::Arg::Long("prompt") => {
                prompt_name = Some(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("model") => {
                model = Some(parser.value()?.to_string_lossy().to_string());
            }
            lexopt::Arg::Long("effort") => {
                effort = Some(parser.value()?.to_string_lossy().parse()?);
            }
            lexopt::Arg::Long("plan") => {
                plan = true;
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

    let session = session.ok_or_else(|| {
        if fork {
            AppError::Usage(format!("missing --session when using --fork\n\n{USAGE}"))
        } else {
            AppError::Usage(format!("missing --session\n\n{USAGE}"))
        }
    })?;
    if fork && matches!(session, SessionArg::New) {
        return Err(AppError::Usage(format!(
            "--fork requires an existing session id or `last`\n\n{USAGE}"
        )));
    }
    let message = prompt.ok_or_else(|| AppError::Usage(format!("missing message\n\n{USAGE}")))?;

    Ok(Command::Run(RunArgs {
        session,
        fork,
        agent,
        role,
        no_role,
        prompt_name,
        model,
        effort,
        plan,
        cwd,
        verbose,
        debug,
        message,
    }))
}

fn parse_session_arg(value: &str) -> SessionArg {
    match value {
        "new" => SessionArg::New,
        "last" => SessionArg::Last,
        _ => SessionArg::Existing(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use crate::error::AppError;

    use super::{Command, RunArgs, SessionArg, USAGE, parse_from_args};

    #[test]
    fn run_parses_new_command() {
        let command = parse_from_args(["new", "hello"]).expect("parse run");

        match command {
            Command::Run(RunArgs {
                session: SessionArg::New,
                fork,
                agent,
                message,
                ..
            }) => {
                assert!(!fork);
                assert!(agent.is_none());
                assert_eq!(message, "hello");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_parses_last_command() {
        let command = parse_from_args(["last", "resume"]).expect("parse run");

        match command {
            Command::Run(RunArgs {
                session: SessionArg::Last,
                fork,
                agent,
                message,
                ..
            }) => {
                assert!(!fork);
                assert!(agent.is_none());
                assert_eq!(message, "resume");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_parses_last_session_selector() {
        let command =
            parse_from_args(["--session", "last", "--fork", "resume"]).expect("parse run");

        match command {
            Command::Run(RunArgs {
                session: SessionArg::Last,
                fork,
                message,
                ..
            }) => {
                assert!(fork);
                assert_eq!(message, "resume");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_parses_fork_on_existing_session() {
        let command = parse_from_args(["--session", "abc123", "--fork", "continue"])
            .expect("parse forked run");

        match command {
            Command::Run(RunArgs {
                session: SessionArg::Existing(id),
                fork,
                message,
                ..
            }) => {
                assert_eq!(id, "abc123");
                assert!(fork);
                assert_eq!(message, "continue");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_parses_prompt_and_role_flags() {
        let command = parse_from_args([
            "--session",
            "new",
            "--role",
            "auditor",
            "--prompt",
            "auditor",
            "audit this commit",
        ])
        .expect("parse run");

        match command {
            Command::Run(RunArgs {
                role,
                no_role,
                prompt_name,
                message,
                ..
            }) => {
                assert_eq!(role.as_deref(), Some("auditor"));
                assert!(!no_role);
                assert_eq!(prompt_name.as_deref(), Some("auditor"));
                assert_eq!(message, "audit this commit");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn run_rejects_role_and_no_role_together() {
        let error = parse_from_args([
            "--session",
            "new",
            "--role",
            "auditor",
            "--no-role",
            "hello",
        ])
        .expect_err("conflicting role flags");
        match error {
            AppError::Usage(message) => {
                assert!(message.contains("--role and --no-role cannot be used together"))
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn run_rejects_fork_without_session() {
        let error = parse_from_args(["--fork", "hello"]).expect_err("missing session");
        match error {
            AppError::Usage(message) => {
                assert!(message.contains("missing --session when using --fork"))
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn run_rejects_fork_with_new_command() {
        let error = parse_from_args(["new", "--fork", "hello"]).expect_err("invalid fork");
        match error {
            AppError::Usage(message) => {
                assert!(message.contains("--fork requires an existing session id or `last`"))
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn run_rejects_fork_with_session_new() {
        let error =
            parse_from_args(["--session", "new", "--fork", "hello"]).expect_err("invalid fork");
        match error {
            AppError::Usage(message) => {
                assert!(message.contains("--fork requires an existing session id or `last`"))
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn run_rejects_removed_new_flag() {
        let error = parse_from_args(["--new", "hello"]).expect_err("removed flag");
        match error {
            AppError::Usage(message) => assert_eq!(message, USAGE),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn run_rejects_duplicate_session_selection_in_new_command() {
        let error =
            parse_from_args(["new", "--session", "abc123", "hello"]).expect_err("duplicate");
        match error {
            AppError::Usage(message) => {
                assert!(message.contains("session may only be selected once"))
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn run_rejects_duplicate_session_selection_in_last_command() {
        let error =
            parse_from_args(["last", "--session", "abc123", "hello"]).expect_err("duplicate");
        match error {
            AppError::Usage(message) => {
                assert!(message.contains("session may only be selected once"))
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn websearch_parses_query_and_flags() {
        let command = parse_from_args([
            "websearch",
            "--type",
            "neural",
            "--num_results",
            "8",
            "--published_within_days",
            "7",
            "--include_domains",
            "docs.rs",
            "--include_domains",
            "crates.io",
            "--exclude_domains",
            "example.com",
            "rust",
            "async",
            "runtimes",
        ])
        .expect("parse websearch");

        match command {
            Command::WebSearch {
                query,
                search_type,
                num_results,
                published_within_days,
                include_domains,
                exclude_domains,
            } => {
                assert_eq!(query, "rust async runtimes");
                assert_eq!(search_type.as_deref(), Some("neural"));
                assert_eq!(num_results, Some(8));
                assert_eq!(published_within_days, Some(7));
                assert_eq!(include_domains, vec!["docs.rs", "crates.io"]);
                assert_eq!(exclude_domains, vec!["example.com"]);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn websearch_rejects_missing_query() {
        let error = parse_from_args(["websearch"]).expect_err("missing query");
        match error {
            AppError::Usage(message) => assert!(message.contains("missing search query")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn websearch_rejects_invalid_type() {
        let error =
            parse_from_args(["websearch", "--type", "keyword", "rust"]).expect_err("invalid type");
        match error {
            AppError::Usage(message) => assert!(message.contains("invalid `--type` value")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn websearch_rejects_invalid_num_results() {
        let error = parse_from_args(["websearch", "--num_results", "0", "rust"])
            .expect_err("invalid num_results");
        match error {
            AppError::Usage(message) => assert!(message.contains("tool argument `num_results`")),
            other => panic!("unexpected error: {other}"),
        }
    }
}
