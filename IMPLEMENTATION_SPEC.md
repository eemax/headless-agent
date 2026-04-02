# Headless Agent Implementation Specification

## Status

This document is the implementation source of truth for the first `Headless Agent` build.

It is written for a fresh implementation agent that may know nothing about prior discussions.

If you are implementing this project, read this document top to bottom before writing code.

## Mission

Build a headless CLI agent runner called `headless`.

The product should be:

- non-interactive
- session-first
- powerful enough for real coding and automation work
- cheap enough to fan out across many independent processes
- explicit and inspectable on disk

This is not meant to be a framework.

This is a clean, opinionated harness for durable headless agents.

## Core Product Definition

There is one run shape:

```text
headless --session <id|new> --agent <name> [--role <name>] [--model <name>] [--effort <level>] [--plan] [--cwd <path>] [--verbose] [--debug] "prompt"
```

Core ideas:

- every run belongs to a session
- every run has an agent
- every run may also have a role
- the agent file is the primary runnable configuration
- the role file is prompt context
- sessions persist durable history in JSONL
- big outputs are preserved as artifacts on disk

## Immediate Direction For The Implementing Agent

If you are starting implementation now, follow this order:

1. Initialize a Rust binary crate named `headless`.
2. Implement the CLI surface and exit code system first.
3. Implement session creation and session metadata next.
4. Implement agent and role loading before provider calls.
5. Implement the OpenRouter integration.
6. Implement the basic session loop.
7. Add the built-in tool harness.
8. Add artifact-backed persistence for large outputs.
9. Add adaptive prompt assembly and compaction support.
10. Add tests and benchmark scripts continuously, not at the end.

Do not start by designing abstractions for hypothetical future providers or plugin systems.

## Product Principles

### Primary Principles

- `stdout` is for final run output only
- `stderr` is for logs, progress, diagnostics, warnings, and errors
- preserve full fidelity on disk whenever practical
- keep active memory smaller than stored data
- prefer explicit files over hidden state
- keep runtime behavior understandable by reading a few functions in order

### Secondary Principles

- optimize for many independent processes, not a central broker
- use optimistic concurrency for same-session conflicts
- keep flags few and meaningful
- put behavior into agent files, not ad hoc runtime switches

## Non-Goals For V1

- multiple providers
- plugin ecosystems
- dynamic tool registration
- SQLite or any database backend
- alternate output envelopes on stdout
- schema-heavy structured output support
- a built-in daemon
- automatic merge of conflicting same-session writes
- parent-directory discovery for agent or role files

## CLI Surface

### Required Runtime Flags

- `--session <id|new>`
- `--agent <name>` for new sessions

### Optional Runtime Flags

- `--role <name>`
- `--model <name>`
- `--effort <none|minimal|low|medium|high|xhigh>`
- `--plan`
- `--cwd <path>` for tool execution and worktree operations only
- `--verbose`
- `--debug`

### Subcommands

- `headless version`
- `headless agent list`
- `headless role list`
- `headless session new`
- `headless session list`
- `headless session show <id>`
- `headless session stop <id>`

### CLI Output Rules

- successful runs write only final assistant text to `stdout`
- runtime metadata does not go to `stdout`
- `headless session new` writes only the new lowercase ULID to `stdout`
- the convenience path `--session new` may write the generated session id to `stderr`

## Session UX

### New Session Creation

Two supported paths:

1. Dedicated creation:

```text
headless session new
```

Behavior:

- create a new lowercase ULID
- initialize session storage on disk
- print only the ULID to `stdout`

2. Convenience creation:

```text
headless --session new --agent coder "help me debug this"
```

Behavior:

- create a new lowercase ULID session id
- report the new id on `stderr`
- run the prompt against that session
- print only the assistant response to `stdout`

### Existing Session Continuation

Example:

```text
headless --session 01hx9cz0z0m4x4h53j9g9y2n3m "continue the fix"
```

Rules:

- if `--agent` is omitted on an existing session, load the stored session agent from metadata
- if `--agent` is provided on an existing session, it must match the stored agent or the command fails
- `--role` is per-run context and does not redefine the session's agent identity

### Session Stop

`headless session stop <id>` should:

- mark the session as stopped in metadata
- prevent new runs from appending to that session
- keep the stored history readable

No reopen command is required in v1.

## Session Identifiers

Use lowercase ULIDs for session ids.

Reasons:

- sortable by time
- compact
- shell-friendly
- easy to read and copy

## Headless Roots

Agent, role, and config resolution must not depend on the target worktree cwd.

Do not resolve these files relative to:

- the process cwd
- `--cwd`
- the user's current project

Instead, support exactly two Headless roots:

1. the `headless-agent` repository root during development
2. the user home root at `~/.headless-agent/`

### Resolution Targets

Agent files:

- `<headless-repo>/agents/<agent-name>.toml`
- `~/.headless-agent/agents/<agent-name>.toml`

Role files:

- `<headless-repo>/roles/<role-name>.toml`
- `~/.headless-agent/roles/<role-name>.toml`

Config file:

- `<headless-repo>/config.toml`
- `~/.headless-agent/config.toml`

### Resolution Order

Recommended order:

1. resolve from the `headless-agent` repository root if the file exists there
2. otherwise resolve from `~/.headless-agent/`

Do not walk parent directories in v1.

Fail fast if a requested file does not exist in either root.

### Meaning Of `--cwd`

`--cwd` affects only:

- tool execution
- file operations
- shell commands
- worktree-relative behavior inside the agent run

It does not affect:

- agent resolution
- role resolution
- config resolution

## Agent Definition

### Agent File Purpose

`agents/<agent-name>.toml` is the primary runnable configuration for an agent.

It should tell an implementer and a user almost everything about how that agent behaves.

### Agent File Example

```toml
name = "coder"
description = "General coding agent"

base_url = "https://openrouter.ai/api/v1"
api_key = ""
api_key_env = "OPENROUTER_API_KEY"

default_model = "openai/gpt-4.1"
default_effort = "medium"
max_output_tokens = 12000
compaction_at_tokens = 180000

skills_dir = "./skills"
enabled_skills = ["repo", "testing", "release"]

enabled_tools = [
  "read_file",
  "edit_file",
  "write_file",
  "glob",
  "grep",
  "apply_patch",
  "bash",
  "todo_write",
  "skills",
  "web_search",
  "web_fetch",
]

system_prompt_file = "./prompts/coder.md"

timeout = "2h"
```

### Agent File Fields

Required:

- `name`
- `default_model`
- `default_effort`
- `enabled_tools`
- `system_prompt_file`

Recommended:

- `description`
- `max_output_tokens`
- `compaction_at_tokens`
- `skills_dir`
- `enabled_skills`
- `timeout`

Optional provider config:

- `base_url`
- `api_key`
- `api_key_env`

### Agent File Semantics

- `default_model` is the model used unless overridden by `--model`
- `default_effort` is the effort used unless overridden by `--effort`
- `max_output_tokens` is the default request cap for model output
- `compaction_at_tokens` is the threshold that triggers context compaction behavior
- `skills_dir` defines where the agent's skills live
- `enabled_skills` is an allowlist
- `enabled_tools` is an allowlist
- `timeout` is the total run timeout unless overridden by internal policy

## Role Definition

### Role File Purpose

`roles/<role-name>.toml` is prompt framing.

It should influence how the agent approaches the current run without redefining the agent's core tool or model contract.

### Role File Example

```toml
name = "auditor"
description = "Risk-focused review framing"
system_prompt_file = "./prompts/auditor.md"
user_prefix_file = "./prompts/auditor-user.md"
```

### Role File Fields

Required:

- `name`

Optional:

- `description`
- `system_prompt_file`
- `user_prefix_file`

### Role File Semantics

- `system_prompt_file` adds system-level framing
- `user_prefix_file` prepends context to the current user prompt only

## Prompt File Rules

Agent and role prompt fields should reference `.md` or `.txt` files.

Supported fields:

- `agent.system_prompt_file`
- `role.system_prompt_file`
- `role.user_prefix_file`

Rules:

- resolve prompt file paths relative to the TOML file that references them
- accept `.md` and `.txt`
- treat both as UTF-8 text
- keep prompt content out of TOML so large prompts remain easy to manage

## Global Configuration

Global configuration should be small and machine-oriented.

Supported config locations:

- `<headless-repo>/config.toml`
- `~/.headless-agent/config.toml`

Example:

```toml
sessions_dir = "~/.headless-agent/sessions"
shell = "/bin/bash"
shell_args = ["-lc"]
max_stdin_bytes = 1048576
artifact_preview_bytes = 16384
catastrophic_output_bytes = 16777216
log_level = "error"
```

### What Belongs In Global Config

- session storage path
- shell path
- shell args
- safety rails
- logging defaults

### What Does Not Primarily Belong In Global Config

- the agent's normal model choice
- the agent's normal effort setting
- enabled skills
- enabled tools
- per-agent provider overrides

Those belong in agent files.

## Configuration Precedence

### General Runtime Precedence

1. explicit runtime overrides such as `--model` and `--effort`
2. agent file config
3. global config
4. process environment fallback

### Credential Precedence

1. `agent.api_key`, if set
2. env var named by `agent.api_key_env`, if set
3. global config credential, if supported
4. default process environment fallback

This is important because the agent file should be portable enough to define its own provider behavior when needed.

## Provider Strategy

### V1 Provider Choice

OpenRouter only.

Do not implement a provider trait in v1.

Use a direct module:

- `src/provider/openrouter.rs`

### Provider Request Requirements

The OpenRouter client should support:

- model selection
- effort selection if supported by the chosen model/provider path
- messages
- tool definitions
- max output token request settings
- timeout handling

### Effort Semantics

Accepted runtime effort values:

- `none`
- `minimal`
- `low`
- `medium`
- `high`
- `xhigh`

The OpenRouter integration must:

- pass the chosen effort if the target model path supports it
- gracefully omit or reject unsupported effort settings according to documented provider behavior

### Model Param Handling

At minimum, the agent implementation should support:

- effort
- max output tokens

Future model parameters may be added, but do not design a huge generic parameter system before the concrete need exists.

## Storage Layout

### Root Storage

```text
~/.headless-agent/
  sessions/
    <session-id>/
      meta.json
      messages.jsonl
      lock
      runs/
        <run-id>/
          tool-outputs/
          fetch/
          patches/
```

### Session Metadata

`meta.json` should include:

- `session_id`
- `created_at`
- `updated_at`
- `stopped_at`
- `revision`
- `char_count`
- `model`
- `initial_role`
- `cwd`
- `agent_name`

Suggested example:

```json
{
  "session_id": "01hx9cz0z0m4x4h53j9g9y2n3m",
  "created_at": "2026-04-02T02:30:00Z",
  "updated_at": "2026-04-02T02:35:00Z",
  "stopped_at": null,
  "revision": 4,
  "char_count": 12549,
  "agent_name": "coder",
  "model": "gpt-4o-mini",
  "initial_role": null,
  "cwd": "/Users/ysera/project",
  "effort": "high"
}
```

### Message Log

`messages.jsonl` stores one message-like record per line.

Suggested shape:

```json
{"v":1,"ts":"2026-04-02T02:30:01Z","run_id":"r_01","role":"user","content":"summarize the repo"}
{"v":1,"ts":"2026-04-02T02:30:05Z","run_id":"r_01","role":"assistant","content":"..."}
{"v":1,"ts":"2026-04-02T02:31:00Z","run_id":"r_02","role":"assistant","tool_calls":[{"id":"call_1","name":"bash","arguments":{"command":"ls"}}]}
{"v":1,"ts":"2026-04-02T02:31:01Z","run_id":"r_02","role":"tool","name":"bash","tool_call_id":"call_1","content":"listing complete","preview":"README.md\nsrc\n...","artifact":{"path":"runs/r_02/tool-outputs/bash-001.stdout","bytes":182734}}
```

### Artifact Philosophy

Do not lose fidelity by default.

If output is large:

- preserve the full content on disk
- store a preview in the transcript
- store artifact metadata in the transcript

Artifact metadata should include:

- relative path
- byte count
- optional hash
- content kind if useful

## Session Concurrency

### High-Level Model

- different sessions should run fully in parallel
- same-session runs should not hold an exclusive lock during provider execution
- same-session write conflicts should fail cleanly

### Mechanism

Use optimistic concurrency:

1. Read `meta.json` and record `revision`.
2. Load session context.
3. Run the provider loop without an exclusive session lock.
4. Re-acquire the session lock before append.
5. Re-read `meta.json`.
6. If `revision` changed, fail with a session conflict exit code.
7. If unchanged, append messages and increment `revision`.

### Why This Matters

This preserves:

- good parallelism across processes
- explicit same-session conflict semantics
- clean failure behavior

without locking a session for the full duration of a model run.

## Memory Strategy

The correct goal is not "small at any cost."

The correct goal is:

- preserve full fidelity on disk
- avoid loading unnecessary data into memory
- keep catastrophic cases bounded

### Rules

- keep full history on disk
- keep full large outputs on disk
- avoid loading the full transcript into memory
- avoid loading full artifacts into prompt context by default
- cap stdin for safety
- cap catastrophic outputs for safety

### Safety Rails

Suggested starting defaults:

- `max_stdin_bytes = 1 MiB`
- `artifact_preview_bytes = 16 KiB`
- `catastrophic_output_bytes = 16 MiB`

These are not normal operating caps on useful output.

They are emergency rails for obviously extreme cases.

## Prompt Assembly

Prompt assembly should be explicit and stable.

### System Prompt Stack

1. agent system prompt file contents
2. role system prompt file contents
3. session summary, if present
4. skill-derived prompt material, if the agent enables skills

### Conversation Stack

5. relevant session history
6. current user prompt, optionally prefixed by role user prefix text
7. stdin as a separate user message, if present

### Context Strategy

Do not use a naive "load all history every time" strategy.

Instead:

- prefer rolling summary plus recent unsummarized history
- allow adaptive retrieval of older records only when needed
- treat artifact previews as default inline context
- let the agent reopen full artifacts through tools when necessary

### Compaction Trigger

Use the agent's `compaction_at_tokens` setting as the threshold to compact or summarize. This feature can be optimized later on in the project.

Compaction should:

- preserve fidelity on disk
- update session summary
- reduce what is sent to the provider

## Tool Harness

The built-in tools for v1 are:

- `read_file`
- `edit_file`
- `write_file`
- `glob`
- `grep`
- `apply_patch`
- `bash`
- `todo_write`
- `skills`
- `web_search`
- `web_fetch`

These are built in, not dynamically registered.

Tool dispatch should be a simple explicit `match`.

## Tool Contracts

### General Tool Rules

- tools are allowed only if enabled by the agent
- each tool should have explicit input parsing
- each tool must have deterministic result structure
- tools should be plan-aware
- tool failures must not corrupt session state

### `read_file`

Purpose:

- read a file from disk

Behavior:

- return file contents
- support line-oriented reading if useful
- fail cleanly on missing files or invalid paths

### `edit_file`

Purpose:

- targeted in-place text edit

Behavior:

- apply one bounded edit operation
- preserve file if the edit does not match expected content
- emit clear errors for mismatch cases

### `write_file`

Purpose:

- create or replace file contents

Behavior:

- write UTF-8 text
- create parent directories only if the tool contract allows it

### `glob`

Purpose:

- find files by pattern

Behavior:

- return matching relative paths
- support ignore rules if defined later

### `grep`

Purpose:

- search project text

Behavior:

- return file paths, line numbers, and matching lines
- prefer ripgrep-like semantics where practical

### `apply_patch`

Purpose:

- apply structured multi-file patches

Behavior:

- accept a patch text format
- write exact requested changes
- reject malformed patches

### `bash`

Purpose:

- run shell commands through the configured shell

Behavior:

- execute in the effective cwd
- store large stdout/stderr as artifacts
- keep transcript previews small
- surface exit code clearly

Plan mode behavior:

- do not execute
- return a planned action result

### `todo_write`

Purpose:

- maintain an explicit work list for the current run

Behavior:

- store a structured checklist in memory for the run
- optionally persist as a run artifact if useful

### `skills`

Purpose:

- inspect agent-enabled skills
- load skill prompt material

Behavior:

- respect `skills_dir`
- respect `enabled_skills`
- fail if a requested skill is not enabled

### `web_search`

Purpose:

- perform web search

Behavior:

- return summarized results with URLs
- keep large search payloads out of prompt context

### `web_fetch`

Purpose:

- fetch and inspect a specific web page

Behavior:

- store large fetched content as artifacts
- provide a preview plus metadata in transcript

## Plan Mode

`--plan` means:

- no side-effecting tool execution
- tools return planned actions instead of performing work
- the assistant should explain what it would do
- the setting applies only to the current invocation

For tools with pure read behavior, decide carefully whether "plan" should still allow reads.

Recommended v1 approach:

- all tools should become non-executing in plan mode for consistency

## Runtime Flow

### End-To-End Flow

1. Parse argv.
2. Resolve Headless roots for config, agents, and roles.
3. Resolve `--cwd` for tool execution and worktree operations.
4. Load global config.
5. Create or resolve the session id.
6. Resolve the effective agent.
7. Resolve the optional role.
8. Load prompt files.
9. Load recent session state and revision.
10. Read stdin if present and within limits.
11. Assemble provider messages.
12. Execute the agent loop.
13. Re-lock the session.
14. Check session revision.
15. Persist new messages and artifacts.
16. Update metadata.
17. Render final assistant text to stdout.
18. Exit with a stable exit code.

## Logging And Output Rules

### Stdout

Use `stdout` only for:

- final assistant text for runs
- the new session id for `headless session new`

### Stderr

Use `stderr` for:

- verbose logs
- debug traces
- warnings
- validation errors
- runtime failures
- convenience reporting of new session ids for `--session new`

## Exit Codes

Create a centralized exit code mapping.

Suggested categories:

- success
- CLI usage error
- config error
- session error
- session conflict
- provider error
- timeout
- tool error
- shell error
- runtime error

Keep this in one module and never scatter numeric exit codes through the codebase.

## Suggested Rust Stack

Runtime crates:

- `lexopt`
- `serde`
- `serde_json`
- `toml`
- `ureq`
- `time`
- `home`
- `thiserror`
- `fs2`
- `signal-hook`
- a ULID crate such as `ulid`

Dev/test crates:

- `tempfile`
- `assert_cmd`
- `predicates`

Release profile:

```toml
[profile.release]
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
opt-level = 3
```

## Suggested Source Layout

```text
Cargo.toml
src/
  main.rs
  cli.rs
  app.rs
  error.rs
  exit.rs
  config.rs
  agent_def.rs
  role_def.rs
  prompt.rs
  artifact.rs
  provider/
    mod.rs
    openrouter.rs
  session/
    mod.rs
    jsonl.rs
  agent/
    mod.rs
    loop.rs
  tools/
    mod.rs
    bash.rs
    files.rs
    glob.rs
    grep.rs
    patch.rs
    todo.rs
    skills.rs
    web.rs
  types/
    mod.rs
    message.rs
    result.rs
    session.rs
tests/
  cli_run.rs
  session_jsonl.rs
  agent_role_loading.rs
  tools_bash.rs
  output_contract.rs
  version.rs
scripts/
  bench.sh
```

## Implementation Plan

### Phase 0: Crate Bootstrap

Deliverables:

- Rust crate initialized
- release profile configured
- centralized exit code module
- `headless version`

Acceptance criteria:

- `cargo build` succeeds
- `headless version` returns 0

### Phase 1: CLI And File Loading

Deliverables:

- root CLI
- global config loading
- agent loading
- role loading
- prompt file loading
- runtime override precedence for `--model` and `--effort`

Acceptance criteria:

- agent and role files resolve from the Headless roots, not the project cwd
- config resolves from the Headless roots, not the project cwd
- prompt files resolve relative to the TOML file
- missing files fail clearly

### Phase 2: Sessions And IDs

Deliverables:

- `headless session new`
- `--session new`
- lowercase ULID generation
- session metadata creation
- session listing and show

Acceptance criteria:

- new ids are lowercase ULIDs
- `headless session new` prints only the id to stdout
- `--session new` keeps stdout clean

### Phase 3: Provider Integration

Deliverables:

- OpenRouter client
- model selection
- effort selection
- timeout handling

Acceptance criteria:

- a simple prompt completes successfully
- runtime/provider failures map to stable exit codes

### Phase 4: Basic Agent Loop

Deliverables:

- assistant/tool loop
- step cap
- tool retry cap
- plan mode

Acceptance criteria:

- loop terminates correctly
- plan mode prevents execution

### Phase 5: Session Persistence

Deliverables:

- JSONL message append
- run artifact directories
- optimistic concurrency via `revision`

Acceptance criteria:

- different sessions do not block each other
- same-session conflicts fail with a specific exit code

### Phase 6: Core Tool Harness

Deliverables:

- `read_file`
- `edit_file`
- `write_file`
- `glob`
- `grep`
- `apply_patch`
- `bash`

Acceptance criteria:

- tools run only when enabled by the agent
- plan mode returns planned results

### Phase 7: Remaining Tools

Deliverables:

- `todo_write`
- `skills`
- `web_search`
- `web_fetch`

Acceptance criteria:

- `skills` respects the agent allowlist
- web artifacts are stored correctly

### Phase 8: Context Compaction And Artifacts

Deliverables:

- rolling session summary
- adaptive history loading
- artifact previews

Acceptance criteria:

- large outputs stay available on disk
- prompt size stays bounded in practice

## Test Plan

Minimum required tests:

- stdout vs stderr contract
- session new behavior
- lowercase ULID generation
- agent loading
- role loading
- prompt file resolution
- override precedence for `--model` and `--effort`
- API key precedence
- enabled skill loading behavior
- same-session conflict behavior
- tool allowlist enforcement
- plan mode no-op behavior
- artifact persistence behavior
- session metadata updates

## Benchmark Plan

Start simple with a shell benchmark script.

Measure:

- `headless version`
- `headless session new`
- `headless agent list`
- 100 parallel `version` runs
- 100 parallel agent-file loads
- 100 parallel runs across 100 different sessions
- 20 to 50 parallel runs against the same session to verify conflict handling

Do not begin with heavy benchmark frameworks.

## Open Questions To Keep Small

If a question can be deferred safely, defer it.

Examples:

- exact compaction algorithm internals
- richer web fetch parsing
- skill packaging format
- future provider expansion

Do not block the first implementation on these.

## Final Guidance For The Implementing Agent

Do not optimize for elegance before behavior works.

Do not over-abstract.

Do not design for many future providers.

Do not move important runtime behavior into hidden defaults.

The correct first version is:

- explicit
- file-driven
- session-first
- artifact-backed
- robust under process fan-out

If forced to choose, prefer:

- understandable code over clever code
- explicit file formats over indirection
- durable on-disk state over in-memory convenience
- clean failure semantics over silent best-effort behavior
