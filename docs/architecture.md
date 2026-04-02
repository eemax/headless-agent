# Architecture

## Overview

`headless` is a single-process CLI runtime. Each invocation parses a command, resolves its configuration roots, loads any session state it needs, optionally talks to OpenRouter, then persists the new transcript state back to disk.

The implementation is intentionally direct:
- one provider module
- one session storage backend
- an explicit built-in tool dispatcher
- no daemon
- no database
- no plugin registry

## Runtime Flow

The main entrypoint is [src/app.rs](/Users/ysera/headless-agent/src/app.rs).

For a prompt run, the flow is:

1. Parse CLI args in [src/cli.rs](/Users/ysera/headless-agent/src/cli.rs).
2. Discover Headless roots and load global config in [src/config.rs](/Users/ysera/headless-agent/src/config.rs).
3. Create or load the target session through [src/session/mod.rs](/Users/ysera/headless-agent/src/session/mod.rs).
4. Resolve the effective agent and optional role via [src/agent_def.rs](/Users/ysera/headless-agent/src/agent_def.rs) and [src/role_def.rs](/Users/ysera/headless-agent/src/role_def.rs).
5. Read stdin if present, load prior messages, and assemble provider messages in [src/prompt.rs](/Users/ysera/headless-agent/src/prompt.rs).
6. Create a per-run directory under the session and execute the assistant/tool loop in [src/agent/loop.rs](/Users/ysera/headless-agent/src/agent/loop.rs).
7. If the run reaches a mutating tool, acquire the session execution lock before side effects and hold it through append.
8. Re-acquire the session lock, verify the stored revision did not change, append JSONL records, and update `meta.json`.
9. Print only the final assistant text to stdout.

Non-run commands such as `version`, `agent list`, `role list`, and `session show` stop earlier and do not enter the provider loop.

## Module Map

- [src/main.rs](/Users/ysera/headless-agent/src/main.rs)
  Thin process wrapper that renders errors to stderr and exits through the centralized exit-code mapping.
- [src/app.rs](/Users/ysera/headless-agent/src/app.rs)
  High-level command execution and run orchestration.
- [src/cli.rs](/Users/ysera/headless-agent/src/cli.rs)
  CLI parsing with `lexopt`.
- [src/error.rs](/Users/ysera/headless-agent/src/error.rs) and [src/exit.rs](/Users/ysera/headless-agent/src/exit.rs)
  Error categories and stable numeric exit codes.
- [src/config.rs](/Users/ysera/headless-agent/src/config.rs)
  Root discovery and `config.toml` loading.
- [src/agent_def.rs](/Users/ysera/headless-agent/src/agent_def.rs) and [src/role_def.rs](/Users/ysera/headless-agent/src/role_def.rs)
  Agent and role TOML loading plus relative prompt-path resolution.
- [src/prompt.rs](/Users/ysera/headless-agent/src/prompt.rs)
  Prompt stack assembly and rough token estimation.
- [src/provider/openrouter.rs](/Users/ysera/headless-agent/src/provider/openrouter.rs)
  Direct OpenRouter chat-completions client.
- [src/session/mod.rs](/Users/ysera/headless-agent/src/session/mod.rs) and [src/session/jsonl.rs](/Users/ysera/headless-agent/src/session/jsonl.rs)
  Session creation, metadata persistence, JSONL append/read helpers, and optimistic concurrency.
- [src/tools](/Users/ysera/headless-agent/src/tools)
  Built-in tool specs, dispatch, and implementations.
- [src/artifact.rs](/Users/ysera/headless-agent/src/artifact.rs)
  Artifact and preview handling for large outputs.
- [src/types](/Users/ysera/headless-agent/src/types)
  Shared types for messages, session metadata, and run results.

## Session Model

Sessions live under the configured `sessions_dir`, defaulting to `~/.headless-agent/sessions`.

Current on-disk layout:

```text
<sessions_dir>/
  <session-id>/
    meta.json
    messages.jsonl
    lock
    runs/
      <run-id>/
        assistant/
        tool-outputs/
```

Important behavior:
- session ids and run ids are lowercase ULIDs
- `headless session new` creates the session directory immediately
- new sessions start unbound, with `agent_name`, `model`, `effort`, `cwd`, and `initial_role` set to `null`
- the first successful prompt run binds the session to an `agent_name` and stores sticky defaults for `model`, `effort`, `cwd`, and `initial_role`
- later runs may override `--model`, `--effort`, and `--cwd` per invocation without mutating those stored defaults
- `--plan` is per-invocation only and is not stored in `meta.json`
- `session stop` marks the session as stopped and blocks later appends

## Optimistic Concurrency

Same-session runs do not hold the session lock while the provider is working.

Instead:

1. the run reads `meta.json` and remembers `revision`
2. the provider and tools execute without a long-lived session lock
3. the runtime re-locks just before append
4. if `revision` changed, the run fails with the session-conflict exit code

This keeps different sessions fully parallel while making same-session conflicts explicit and inspectable.

Mutating tools add one more guardrail:

1. the runtime acquires a separate session execution lock immediately before the first mutating tool
2. it re-validates `revision` under that lock before any side effects
3. it holds that execution lock through transcript append
4. same-session runs that cannot acquire the execution lock fail fast with the session-conflict exit code

## Prompt Assembly

Current prompt assembly order:

1. agent system prompt
2. role system prompt, if present
3. full stored message history
4. current user prompt, prefixed by role user text if present
5. stdin as an extra user message, if present

First-pass limitations:
- no rolling summaries
- no adaptive retrieval
- no compaction
- token estimation is intentionally rough, based on character count
- if estimated prompt size exceeds `compaction_at_tokens`, the run fails with a runtime error instead of silently trimming context

## Provider And Tool Loop

The provider loop in [src/agent/loop.rs](/Users/ysera/headless-agent/src/agent/loop.rs) uses:
- OpenRouter only
- a hard step cap of `24`
- a per-tool retry cap of `2` for read-only tools only
- `parallel_tool_calls = false`
- a total run deadline derived from the agent timeout

Loop behavior:
- send the current conversation plus built-in tool definitions
- record the assistant message
- if the assistant requested tools, execute them in order
- append tool results back into the prompt stack
- stop once the assistant returns content without tool calls

## Tools

Current built-in tools:
- `read_file`
- `edit_file`
- `write_file`
- `glob`
- `grep`
- `apply_patch`
- `bash`

Tool properties:
- allowed only if listed in the agent's `enabled_tools`
- dispatched by explicit `match`
- return deterministic JSON payloads
- persist their payloads through the artifact layer
- in `--plan` mode, all tools become non-executing and return planned-action payloads
- `read_file`, `glob`, and `grep` are retried on ordinary tool errors; mutating tools are single-attempt
- `bash` is treated as mutating and is killed on timeout, including its subprocess group

## Artifacts

Large tool payloads are written under each run directory and referenced from the transcript.

Current behavior:
- small payloads stay inline
- larger payloads are stored on disk with a preview in the transcript
- very large assistant text can also be artifact-backed
- transcript records store relative artifact path and byte count

## Output Contract

The stdout/stderr split is strict:
- stdout is reserved for the final assistant text, or the new session id for `headless session new`
- stderr is used for errors, validation failures, and optional verbose/debug metadata

This is why run metadata such as the generated session id for `--session new` is reported on stderr, not stdout.

## Known First-Pass Boundaries

Not implemented yet:
- `todo_write`
- `skills`
- `web_search`
- `web_fetch`
- summary compaction
- alternate providers
- streaming responses
- a plugin system
