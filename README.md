# Headless Agent

`headless` is a session-first, non-interactive CLI agent runner for coding and automation work.

This repo contains the first implementation pass: a Rust binary with durable session storage, repo- or home-root agent and role loading, an OpenRouter-backed assistant/tool loop, artifact-backed tool transcripts, and nine built-in tools.

## Current Scope

Implemented now:
- CLI entrypoints for `version`, `agent list`, `role list`, and `session new|list|show|stop`
- prompt runs with `--session`, `--agent`, optional `--role`, `--model`, `--effort`, `--plan`, and `--cwd`
- OpenRouter chat completions integration
- session metadata and JSONL transcript persistence
- optimistic same-session conflict detection plus a dedicated mutating-run execution lock
- built-in tools: `read_file`, `edit_file`, `write_file`, `glob`, `grep`, `apply_patch`, `bash`, `web_search`, `web_fetch`
- repo-owned starter assets in `agents/`, `roles/`, `prompts/`, and `config.toml`
- total run timeout enforcement across provider calls and tool execution

Deferred for a later pass:
- `todo_write`
- `skills`
- rolling summaries and context compaction

## Quick Start

Build and test:

```bash
cargo test
```

Inspect the CLI:

```bash
cargo run -- version
cargo run -- webfetch https://example.com
cargo run -- websearch rust async runtimes
cargo run -- agent list
cargo run -- role list
```

`websearch` requires `EXA_API_KEY` and currently exposes the simplified Exa search modes `auto`, `neural`, and `deep`.

Create a session:

```bash
cargo run -- session new
```

Run the default repo-shipped agent:

```bash
export OPENROUTER_API_KEY=...
export EXA_API_KEY=...
cargo run -- --session new --agent coder "summarize this repo"
```

Run in plan mode:

```bash
cargo run -- --session new --agent coder --plan "inspect the project and propose edits"
```

In `--plan` mode, all built-in tools return planned actions instead of executing, including read-only tools. `--plan` is per-run only; it is not persisted in session metadata.

## Global Install

Install the binary into Cargo's global bin directory:

```bash
cargo install --path . --bin headless
headless version
```

If `headless` is not found after install, add `~/.cargo/bin` to your `PATH`.

When you change the code, rebuild and reinstall the global binary with:

```bash
cargo install --path . --bin headless --force
```

For faster local iteration, you can symlink a release build instead of reinstalling on every change:

```bash
cargo build --release
mkdir -p ~/.local/bin
ln -sf /Users/ysera/headless-agent/target/release/headless ~/.local/bin/headless
```

With that setup, code changes only require:

```bash
cargo build --release
```

If you invoke `headless` inside another repo without `--cwd`, tool execution uses the shell's current working directory. Passing `--cwd` overrides that for the current run. On the first successful prompt run in a session, the effective cwd is stored as that session's sticky default and reused by later runs in the same session when `--cwd` is omitted.

## Configuration Roots

Headless configuration is resolved from exactly two roots:

1. the repo root during development
2. `~/.headless-agent/`

The code checks the repo root first, then the home root. `--cwd` affects tool execution only; it does not affect agent, role, or config resolution.

Current implementation note: repo-root discovery is compiled from the source checkout used to build the binary, so an installed `headless` binary still prefers that checkout's `agents/`, `roles/`, and `config.toml` as long as the checkout exists. For a standalone global setup, place config under `~/.headless-agent/`.

For tests and local harnessing, the implementation also supports `HEADLESS_REPO_ROOT` and `HEADLESS_HOME_ROOT` environment overrides.

## Docs

- [Architecture](/Users/ysera/headless-agent/docs/architecture.md)
- [Configuration reference](/Users/ysera/headless-agent/docs/headless.md)
- [Developer playbook](/Users/ysera/headless-agent/docs/dev.md)
- [Web fetch deep dive](docs/web_fetch.md)

## Repo Layout

```text
agents/      Starter agent definitions
roles/       Starter role definitions
prompts/     Prompt text files referenced by agent and role TOML
src/         Runtime implementation
tests/       CLI, provider, session, and tool contract coverage
scripts/     Small operational helpers such as bench.sh
docs/        Architecture, config reference, and developer playbook
```
