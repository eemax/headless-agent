#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${BIN:-$ROOT_DIR/target/release/headless}"
TMP_HOME="${TMP_HOME:-$(mktemp -d)}"

export HOME="$TMP_HOME"
export HEADLESS_REPO_ROOT="$ROOT_DIR"

if [[ ! -x "$BIN" ]]; then
  echo "building release binary at $BIN" >&2
  cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" >&2
fi

time "$BIN" version >/dev/null
time "$BIN" agent list >/dev/null
time "$BIN" role list >/dev/null
time "$BIN" session new >/dev/null

echo "100 parallel version runs" >&2
seq 100 | xargs -n1 -P100 -I{} "$BIN" version >/dev/null

echo "100 parallel agent list runs" >&2
seq 100 | xargs -n1 -P100 -I{} "$BIN" agent list >/dev/null

echo "session conflict smoke" >&2
SESSION_ID="$("$BIN" session new)"
if [[ -n "${OPENROUTER_API_KEY:-}" ]]; then
  printf 'noop' | "$BIN" --session "$SESSION_ID" --agent coder --plan "read stdin once" >/dev/null
  seq 20 | xargs -n1 -P20 -I{} "$BIN" --session "$SESSION_ID" --plan "parallel plan run {}" >/dev/null || true
else
  echo "skipping live run benchmark because OPENROUTER_API_KEY is not set" >&2
fi
