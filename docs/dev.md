# Developer Playbook

This repo is designed so a fresh engineer or agent can make progress by reading a small number of files in order. Keep it that way.

## Start Here

Read these in order before making structural changes:

1. [README.md](/Users/ysera/headless-agent/README.md)
2. [IMPLEMENTATION_SPEC.md](/Users/ysera/headless-agent/IMPLEMENTATION_SPEC.md)
3. [docs/architecture.md](/Users/ysera/headless-agent/docs/architecture.md)
4. [src/app.rs](/Users/ysera/headless-agent/src/app.rs)
5. the module you plan to change

If you are changing behavior, update docs alongside code whenever the user-facing contract changes.

## Project Principles

Keep the implementation:
- explicit
- file-driven
- session-first
- easy to trace from CLI entrypoint to disk writes

Avoid:
- speculative abstraction
- generic provider frameworks
- hidden runtime behavior
- silent best-effort recovery that weakens inspectability

## Important Invariants

Do not break these without a deliberate product decision:

`stdout` contract
- prompt runs print only final assistant text to stdout
- `headless session new` prints only the session id to stdout
- metadata and diagnostics belong on stderr

Root resolution
- config, agents, and roles resolve from the repo root first, then `~/.headless-agent/`
- `--cwd` must not affect those lookups

Session identity
- a session is bound to a single `agent_name` on the first successful prompt run
- later runs may override `--model`, `--effort`, and `--cwd` without mutating stored sticky defaults
- `--plan` is per-run only and is never stored in session metadata

Concurrency
- do not hold the session lock across the provider call
- preserve optimistic concurrency with revision checks at append time
- mutating tools must acquire the separate execution lock before side effects and hold it through append

Plan mode
- in the first pass, all tools are non-executing in `--plan`

## Working In The Codebase

Typical file ownership by concern:

- CLI surface: [src/cli.rs](/Users/ysera/headless-agent/src/cli.rs)
- command orchestration: [src/app.rs](/Users/ysera/headless-agent/src/app.rs)
- config and root lookup: [src/config.rs](/Users/ysera/headless-agent/src/config.rs)
- agent and role loading: [src/agent_def.rs](/Users/ysera/headless-agent/src/agent_def.rs), [src/role_def.rs](/Users/ysera/headless-agent/src/role_def.rs)
- prompt assembly: [src/prompt.rs](/Users/ysera/headless-agent/src/prompt.rs)
- provider integration: [src/provider/openrouter.rs](/Users/ysera/headless-agent/src/provider/openrouter.rs)
- session persistence: [src/session/mod.rs](/Users/ysera/headless-agent/src/session/mod.rs)
- tool harness: [src/tools/mod.rs](/Users/ysera/headless-agent/src/tools/mod.rs) plus the per-tool files

Prefer small, local changes over broad refactors. If a change only touches one phase of the runtime flow, keep the patch in that phase.

## Testing Workflow

Normal loop:

```bash
cargo fmt
cargo test
```

Useful targeted runs:

```bash
cargo test --test cli_run
cargo test --test session_jsonl
cargo test --test tools_bash
cargo test --test provider_openrouter
```

The provider test strategy is intentional:
- offline request-shaping tests use the fake local HTTP server in [tests/common.rs](/Users/ysera/headless-agent/tests/common.rs)
- the live smoke test is ignored by default and should stay opt-in

Do not turn regular CI-style coverage into live network dependence.

## When Adding Features

If you add a new CLI flag or command:
- update [src/cli.rs](/Users/ysera/headless-agent/src/cli.rs)
- update the README
- add or update CLI tests

If you add a new tool:
- add its spec and dispatcher branch in [src/tools/mod.rs](/Users/ysera/headless-agent/src/tools/mod.rs)
- add an implementation file under [src/tools](/Users/ysera/headless-agent/src/tools)
- decide the plan-mode payload
- add allowlist and end-to-end coverage

If you change session persistence:
- preserve inspectable on-disk state
- keep JSONL append behavior straightforward
- add coverage for conflicts, execution-lock behavior, and metadata updates

If you change provider behavior:
- keep the implementation direct
- prefer shaping around real OpenRouter API behavior rather than inventing internal provider abstractions
- add request-shape tests
- preserve total-run timeout behavior across provider and tool execution

## Current Gaps To Respect

These areas are intentionally incomplete in the first pass:
- compaction and rolling summaries
- skills
- web tools
- alternate providers
- streaming

Do not paper over those gaps with hidden fallback behavior. It is better to fail clearly than to silently change the product contract.

## Benchmarks And Manual Checks

There is a lightweight benchmark helper at [scripts/bench.sh](/Users/ysera/headless-agent/scripts/bench.sh).

Use it for quick operational smoke checks, not as a substitute for targeted tests.

For manual verification with a real key:

```bash
export OPENROUTER_API_KEY=...
cargo run -- --session new --agent coder "say hello"
```

## Docs To Keep In Sync

When user-facing behavior changes, update:
- [README.md](/Users/ysera/headless-agent/README.md)
- [docs/architecture.md](/Users/ysera/headless-agent/docs/architecture.md)
- [docs/headless.md](/Users/ysera/headless-agent/docs/headless.md)

If the implementation direction changes materially, also update:
- [PLAN.md](/Users/ysera/headless-agent/PLAN.md)
- [IMPLEMENTATION_SPEC.md](/Users/ysera/headless-agent/IMPLEMENTATION_SPEC.md), if the source-of-truth spec should keep matching the shipped direction
