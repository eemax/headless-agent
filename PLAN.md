# First Pass: Headless Vertical Slice

## Summary
- Bootstrap a greenfield Rust binary crate named `headless` and implement phases 0-6 only: CLI, root/config/agent/role loading, session lifecycle, real OpenRouter integration, basic assistant/tool loop, optimistic persistence, and the core built-in tools.
- Defer phases 7-8 entirely: `todo_write`, `skills`, `web_search`, `web_fetch`, rolling summaries, adaptive retrieval, and compaction. If a session exceeds `compaction_at_tokens`, fail cleanly with a stable runtime error instead of truncating context implicitly.
- Ship repo-root runtime assets so a fresh checkout is usable immediately: default `coder` agent, `auditor` role, prompt files, and a small repo `config.toml`.

## Interfaces
- Implement the full CLI surface from the spec: `headless version`, `headless agent list`, `headless role list`, `headless session new|list|show|stop`, plus `headless --session <id|new> --agent <name> [--role <name>] [--model <name>] [--effort <none|minimal|low|medium|high|xhigh>] [--plan] [--cwd <path>] [--verbose] [--debug] "prompt"`.
- Preserve the stdout/stderr contract exactly: run commands print only final assistant text to stdout; `headless session new` prints only the new lowercase ULID to stdout; `--session new` reports the generated session id on stderr and keeps stdout reserved for the assistant reply.
- Resolve Headless roots without consulting process cwd or `--cwd`: first use the development repo root via the compile-time manifest directory when that tree contains the expected assets, otherwise fall back to `~/.headless-agent/`.
- Implement `config.toml`, `agents/<name>.toml`, `roles/<name>.toml`, and prompt-file resolution relative to the TOML file that references each prompt. Use runtime precedence `flags > agent file > global config > environment`, and credential precedence `agent.api_key > agent.api_key_env > global config > environment`.
- `headless session new` creates the session directory, `meta.json`, empty `messages.jsonl`, and lock file immediately. `meta.json` allows `agent_name`, `model`, `effort`, `cwd`, `plan_enabled`, and `initial_role` to be `null` until the first successful prompt run.
- The first successful prompt run permanently binds `agent_name` and establishes session defaults for `model`, `effort`, `cwd`, `plan_enabled`, and `initial_role`. Later runs inherit those values when omitted; `--model`, `--effort`, `--cwd`, and `--plan` may override per run without mutating the stored session defaults.
- Implement only the phase-6 tools in this pass: `read_file`, `edit_file`, `write_file`, `glob`, `grep`, `apply_patch`, and `bash`. Dispatch via an explicit `match`, gate every tool behind the agent allowlist, and make every tool non-executing in `--plan` mode, including read-only tools.

## Implementation Changes
- Follow the spec’s suggested Rust layout closely, using `lexopt`, `serde`, `serde_json`, `toml`, `ureq`, `time`, `home`, `thiserror`, `fs2`, `signal-hook`, `ulid`, plus `tempfile`, `assert_cmd`, and `predicates` for tests. Keep numeric exit codes centralized in one module.
- Implement a direct `src/provider/openrouter.rs` module rather than a generic provider trait. Support `model`, `messages`, `tool definitions`, `max_output_tokens`, timeout handling, and `effort`; omit the field when `effort=none`, otherwise pass it through and surface provider rejection as a stable provider error.
- Use lowercase ULIDs for both session ids and run ids. Persist messages in JSONL, create per-run artifact directories, and enforce optimistic concurrency by checking `meta.json.revision` before append; same-session races fail with a specific conflict exit code after provider execution.
- Artifact-back large tool outputs immediately: store full stdout/stderr on disk, keep only previews plus metadata in transcript records, and record relative path and byte count. Keep assistant text in JSONL unless it exceeds the catastrophic-output rail, then persist it as an artifact and reference it.
- Assemble prompts as: agent system prompt, optional role system prompt, existing session history, optional role user prefix, current user prompt, optional stdin. Do not implement summaries or adaptive retrieval yet; once estimated context would cross `compaction_at_tokens`, abort with a clear first-pass limitation error.
- Use runtime defaults from the spec, plus step cap `24` and per-tool retry cap `2`. Default shell execution to `/bin/bash` with `-lc`.
- Include a simple `scripts/bench.sh` that measures `headless version`, `headless session new`, agent listing/loading, parallel runs across different sessions, and conflict behavior on the same session.

## Test Plan
- Verify CLI behavior: `version`, usage errors, stable exit-code mapping, stdout/stderr separation, and the `--session new` convenience contract.
- Verify root and asset loading: repo-root vs home-root precedence, independence from process cwd and `--cwd`, missing-file failures, agent/role loading, and prompt-path resolution relative to TOML.
- Verify session lifecycle: lowercase ULID generation, bare-session nullable metadata, first-run binding, inherited defaults on continuation, per-run override behavior, agent mismatch failure, `session list`, `session show`, and `session stop`.
- Verify provider behavior with offline tests for OpenRouter request shaping and error normalization, plus an ignored live smoke test that runs only when `OPENROUTER_API_KEY` is present.
- Verify persistence and concurrency: JSONL append shape, artifact preview metadata, catastrophic-output handling, conflict failure for concurrent same-session runs, and successful parallel runs across different sessions.
- Verify tool behavior: allowlist enforcement, deterministic `--plan` no-op results for every core tool, and at least one end-to-end loop test that uses `bash` plus file tools.

## Assumptions And Defaults
- The repo is currently greenfield and not a git worktree, so the first pass includes full crate bootstrap and repo-owned runtime assets.
- Repo-root assets are the default development experience; `~/.headless-agent/` is the override/install location.
- Session defaults established on first bind do not change when later runs override `model`, `effort`, `cwd`, or `plan`.
- Phases 7 and 8 are intentionally out of scope for this pass and should be the next follow-up once the vertical slice is stable.
