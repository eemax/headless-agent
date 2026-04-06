# Developer Playbook

This repo is designed so a fresh engineer or agent can make progress by reading a small number of files in order. Keep it that way.

## Start Here

Read these in order before making structural changes:

1. [README.md](../README.md)
2. [docs/architecture.md](./architecture.md)
3. [src/app.rs](../src/app.rs)
4. the module you plan to change

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
- `config.default_agent` is only a fallback for unbound prompt runs; it does not override a bound session
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

- CLI surface: [src/cli.rs](../src/cli.rs)
- command orchestration: [src/app.rs](../src/app.rs)
- config and root lookup: [src/config.rs](../src/config.rs)
- agent and role loading: [src/agent_def.rs](../src/agent_def.rs), [src/role_def.rs](../src/role_def.rs)
- prompt assembly: [src/prompt.rs](../src/prompt.rs)
- provider integration: [src/provider/openrouter.rs](../src/provider/openrouter.rs)
- session persistence: [src/session/mod.rs](../src/session/mod.rs)
- tool harness: [src/tools/mod.rs](../src/tools/mod.rs) plus the per-tool files

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
cargo test --test agent_loop
cargo test --test agent_role_loading
cargo test --test session_jsonl
cargo test --test prompt_assembly
cargo test --test output_contract
cargo test --test provider_openrouter
cargo test --test tools_bash
cargo test --test tools_core
cargo test --test tools_files
cargo test --test tools_glob
cargo test --test tools_grep
cargo test --test tools_patch
cargo test --test version
```

The provider test strategy is intentional:
- offline request-shaping tests use the fake local HTTP server in [tests/common.rs](../tests/common.rs)
- the live smoke test is ignored by default and should stay opt-in

Do not turn regular CI-style coverage into live network dependence.

The web fetch live canaries are also opt-in and split into three tiers:
- `gating` for a small fast confidence check
- `observational` for a broader real-site corpus
- `self_hosted_edge` for controlled edge-case pages you host yourself

Useful runs:

```bash
cargo test web_fetch_live_canaries_gating -- --ignored --nocapture
cargo test web_fetch_live_canaries_observational -- --ignored --nocapture
HEADLESS_WEB_FETCH_CANARY_CASE_ID=gating_openai_docs_function_calling cargo test web_fetch_live_canaries_gating -- --ignored --nocapture
HEADLESS_WEB_FETCH_CANARY_SELF_HOSTED_BASE_URL=https://canary.example.com cargo test web_fetch_live_canaries_self_hosted_edge -- --ignored --nocapture
```

Useful filters:
- `HEADLESS_WEB_FETCH_CANARY_TIER`
- `HEADLESS_WEB_FETCH_CANARY_CASE_ID`
- `HEADLESS_WEB_FETCH_CANARY_SELF_HOSTED_BASE_URL` for the `self_hosted_edge` tier

## When Adding Features

If you add a new CLI flag or command:
- update [src/cli.rs](../src/cli.rs)
- update the README
- add or update CLI tests

If you add a new tool:
- add its spec and dispatcher branch in [src/tools/mod.rs](../src/tools/mod.rs)
- add an implementation file under [src/tools](../src/tools)
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

There are two manual benchmark helpers:
- [scripts/bench.sh](../scripts/bench.sh) for a quick shell smoke check
- [scripts/bench_perf.py](../scripts/bench_perf.py) for repeatable local performance measurements plus optional live `webfetch` comparisons

Use them for operational checks and before/after optimization comparisons, not as a substitute for targeted tests.

Useful runs:

```bash
./scripts/bench_perf.py --quick
./scripts/bench_perf.py
./scripts/bench_perf.py --quick --live-webfetch
./scripts/bench_perf.py --json > /tmp/headless-bench.json
```

For manual verification with a real key:

```bash
export OPENROUTER_API_KEY=...
cargo run -- new "say hello"
```

## Docs To Keep In Sync

When user-facing behavior changes, update:
- [README.md](../README.md)
- [docs/architecture.md](./architecture.md)
- [docs/headless.md](./headless.md)
