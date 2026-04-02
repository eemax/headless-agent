# Headless Configuration Reference

This document describes the current `.toml` configuration surfaces implemented in this repo:
- global `config.toml`
- agent files in `agents/*.toml`
- role files in `roles/*.toml`

It reflects the current code, including runtime defaults and first-pass limitations.

## Resolution Rules

Headless resolves config from exactly two roots:

1. repo root
2. `~/.headless-agent/`

Resolution order is repo root first, then home root.

Current implementation notes:
- `HEADLESS_REPO_ROOT` can override the repo root for testing or harnessing
- `HEADLESS_HOME_ROOT` can override the home root for testing or harnessing
- `--cwd` does not affect config, agent, or role lookup
- prompt-file paths are resolved relative to the TOML file that references them

## Global `config.toml`

Supported locations:
- `<repo-root>/config.toml`
- `~/.headless-agent/config.toml`

Current default values come from [src/config.rs](/Users/ysera/headless-agent/src/config.rs).

### Full Example

```toml
sessions_dir = "~/.headless-agent/sessions"
shell = "/bin/bash"
shell_args = ["-lc"]
max_stdin_bytes = 1048576
artifact_preview_bytes = 16384
catastrophic_output_bytes = 16777216
api_key_env = "OPENROUTER_API_KEY"
```

### Field Reference

`sessions_dir`
- Type: string path
- Default: `"~/.headless-agent/sessions"`
- Purpose: root directory where session folders are stored
- Notes: `~` is expanded by the loader

`shell`
- Type: string
- Default: `"/bin/bash"`
- Purpose: executable used by the `bash` tool

`shell_args`
- Type: array of strings
- Default: `["-lc"]`
- Purpose: arguments passed before the tool's command string

`max_stdin_bytes`
- Type: integer
- Default: `1048576`
- Purpose: hard safety limit for stdin size

`artifact_preview_bytes`
- Type: integer
- Default: `16384`
- Purpose: max inline preview size before large payloads are artifact-backed

`catastrophic_output_bytes`
- Type: integer
- Default: `16777216`
- Purpose: emergency threshold for very large output; beyond this, inline assistant text is replaced with an artifact reference string

`api_key`
- Type: string
- Default: unset
- Purpose: optional global OpenRouter API key

`api_key_env`
- Type: string
- Default: unset in code, `"OPENROUTER_API_KEY"` in the repo-shipped starter config
- Purpose: names an environment variable to read for the API key

### Credential Precedence

Current runtime precedence for the OpenRouter API key is:

1. `agent.api_key`, if non-empty
2. env var named by `agent.api_key_env`
3. `config.api_key`, if non-empty
4. env var named by `config.api_key_env`
5. `OPENROUTER_API_KEY`

## Agent Files

Supported locations:
- `<repo-root>/agents/<name>.toml`
- `~/.headless-agent/agents/<name>.toml`

Agent files are the primary runnable configuration.

### Repo Starter Example

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
enabled_skills = []

enabled_tools = [
  "read_file",
  "edit_file",
  "write_file",
  "glob",
  "grep",
  "apply_patch",
  "bash",
]

system_prompt_file = "../prompts/coder.md"

timeout = "2h"
```

### Required Fields

`name`
- Type: string
- Default: none
- Notes: must not be empty

`default_model`
- Type: string
- Default: none
- Notes: used when `--model` is omitted and the session is not yet bound to another default

`default_effort`
- Type: string enum
- Allowed values: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`
- Default: none

`enabled_tools`
- Type: array of strings
- Default: none
- Notes: must contain at least one entry

`system_prompt_file`
- Type: string path
- Default: none
- Notes: resolved relative to the agent TOML file

### Optional Fields And Runtime Defaults

`description`
- Type: string
- Default: unset

`base_url`
- Type: string
- Default at runtime: `"https://openrouter.ai/api/v1"`
- Notes: if omitted, the OpenRouter client falls back to the default base URL

`api_key`
- Type: string
- Default: unset
- Notes: empty string behaves like unset

`api_key_env`
- Type: string
- Default: unset

`max_output_tokens`
- Type: integer
- Default at runtime: `12000`
- Purpose: sent to OpenRouter as `max_completion_tokens`

`compaction_at_tokens`
- Type: integer
- Default at runtime: `180000`
- Purpose: prompt-size threshold
- Current behavior: the runtime fails if estimated prompt size exceeds this threshold because compaction is not implemented yet

`skills_dir`
- Type: string path
- Default: unset
- Current status: parsed and path-resolved, but not used by the first-pass runtime

`enabled_skills`
- Type: array of strings
- Default: `[]`
- Current status: parsed only; skills are not implemented in the first pass

`timeout`
- Type: duration string
- Supported suffixes: `h`, `m`, `s`
- Default at runtime: `"2h"`
- Purpose: total wall-clock run timeout across provider calls, tool execution, and loop overhead

### Tool Names

Recognized built-in tools in the first pass:
- `read_file`
- `edit_file`
- `write_file`
- `glob`
- `grep`
- `apply_patch`
- `bash`

Unknown tool names are ignored when building provider tool definitions, but a model cannot successfully call them because the dispatcher only knows the built-ins above.

### Agent Runtime Precedence

Current effective value precedence is:

1. explicit run flags such as `--model` or `--effort`
2. stored sticky session defaults, once the session has been bound
3. agent file values
4. global config or process environment fallback where applicable

`--plan` is intentionally not part of session-default precedence. It applies only to the current invocation.

## Role Files

Supported locations:
- `<repo-root>/roles/<name>.toml`
- `~/.headless-agent/roles/<name>.toml`

Roles are prompt-framing overlays, not independent runnable agents.

### Example

```toml
name = "auditor"
description = "Risk-focused review framing"
system_prompt_file = "../prompts/auditor.md"
user_prefix_file = "../prompts/auditor-user.md"
```

### Field Reference

`name`
- Type: string
- Default: none
- Notes: required

`description`
- Type: string
- Default: unset

`system_prompt_file`
- Type: string path
- Default: unset
- Purpose: appended to the system-prompt stack after the agent system prompt

`user_prefix_file`
- Type: string path
- Default: unset
- Purpose: prepended to the current user prompt only

## Prompt File Notes

Prompt paths are relative to the TOML file that references them.

Current implementation behavior:
- the loader reads the file as text
- the repo convention is Markdown prompt files
- there is no enforced extension allowlist in code today

## Session Metadata Defaults

These values are not configured through TOML, but they matter when reasoning about configuration behavior.

New sessions start with:

```json
{
  "agent_name": null,
  "model": null,
  "effort": null,
  "cwd": null,
  "initial_role": null
}
```

On the first successful prompt run, those fields are bound from the effective runtime values and then reused as sticky defaults when later runs omit them. Plan mode is never stored in session metadata.

## First-Pass Limitations

These config surfaces are intentionally not active yet:
- `enabled_skills`
- `skills_dir`
- any future provider config beyond the current OpenRouter fields
- compaction behavior after `compaction_at_tokens`

Plan-mode behavior is also intentionally strict in this pass:
- `--plan` makes all built-in tools non-executing, including reads
- `--plan` applies only to the current run
