#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────────
# bench_webfetch.sh — Manual live benchmark: headless vs defuddle vs curl
#
# Compares extraction quality across a set of diverse URLs.  Run after
# making changes to the web_fetch pipeline to spot regressions that the
# offline tests and structured canaries cannot catch.
#
# Usage:
#   ./scripts/bench_webfetch.sh                 # all cases
#   ./scripts/bench_webfetch.sh wikipedia_rust   # single case by id
#   HEADLESS=./target/debug/headless ./scripts/bench_webfetch.sh
#
# Requirements:
#   - Release binary (cargo build --release), or set HEADLESS=…
#   - defuddle CLI (npm i -g defuddle-cli)
#   - curl
#   - node (for defuddle) on PATH or in ~/.nvm
# ──────────────────────────────────────────────────────────────────────
set -euo pipefail

# ── locate tools ─────────────────────────────────────────────────────
HEADLESS="${HEADLESS:-$(dirname "$0")/../target/release/headless}"
if [[ ! -x "$HEADLESS" ]]; then
  echo "error: headless binary not found at $HEADLESS"
  echo "       run 'cargo build --release' or set HEADLESS="
  exit 1
fi

# try to pick up nvm node if system node is missing
if ! command -v node &>/dev/null; then
  # pick the newest nvm node version available
  _nvm_latest=$(ls -1d "$HOME"/.nvm/versions/node/*/bin 2>/dev/null | tail -1)
  if [[ -n "${_nvm_latest:-}" ]]; then
    export PATH="$_nvm_latest:$PATH"
  fi
fi

if ! command -v defuddle &>/dev/null; then
  echo "error: defuddle not found — install with 'npm i -g defuddle-cli'"
  exit 1
fi

# ── benchmark case definitions ───────────────────────────────────────
# Each case: ID|URL|MIN_CHARS|REQUIRED_MARKERS (pipe-separated phrases)
#
# MIN_CHARS is the minimum expected content chars from headless output
# (metadata header excluded).  REQUIRED_MARKERS are pipe-separated
# strings that must appear somewhere in the headless content body.
#
# Add new cases here as we encounter interesting pages.
CASES=(
  "wikipedia_rust|https://en.wikipedia.org/wiki/Rust_(programming_language)|8000|Rust is a|## History"
  "paulgraham_greatwork|https://www.paulgraham.com/greatwork.html|20000|The first step is to decide what to work on|Notes"
  "hackernews_front|https://news.ycombinator.com|2000|points by|comments"
  "mdn_js_functions|https://developer.mozilla.org/en-US/docs/Web/JavaScript/Guide/Functions|4000|## Defining functions|function square"
  "github_docs_hello|https://docs.github.com/en/get-started/quickstart/hello-world|2000|## Step 1: Create a repository|pull request"
  "arxiv_2301_07041|https://arxiv.org/abs/2301.07041|1000|Fully Homomorphic Encryption|Abstract"
  "rust_book_install|https://doc.rust-lang.org/book/ch01-01-installation.html|1200|Rust is installed now|rustup"
  "httpbin_html|https://httpbin.org/html|400|Herman Melville|Moby-Dick"
)

# ── output directory ─────────────────────────────────────────────────
OUTDIR="${BENCH_WEBFETCH_OUTDIR:-/tmp/bench_webfetch}"
rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

# ── colour helpers ───────────────────────────────────────────────────
if [[ -t 1 ]]; then
  GREEN=$'\033[32m' RED=$'\033[31m' YELLOW=$'\033[33m'
  BOLD=$'\033[1m' DIM=$'\033[2m' RESET=$'\033[0m'
else
  GREEN="" RED="" YELLOW="" BOLD="" DIM="" RESET=""
fi

pass() { printf "  ${GREEN}✓${RESET} %s\n" "$1"; }
fail() { printf "  ${RED}✗${RESET} %s\n" "$1"; }
info() { printf "  ${DIM}%s${RESET}\n" "$1"; }

# ── run one case ─────────────────────────────────────────────────────
run_case() {
  local id url min_chars markers_raw
  IFS='|' read -r id url min_chars markers_raw <<< "$1"

  printf "\n${BOLD}── %s${RESET}\n" "$id"
  info "$url"

  local h_file="$OUTDIR/${id}_headless.txt"
  local d_file="$OUTDIR/${id}_defuddle.txt"
  local c_file="$OUTDIR/${id}_curl.txt"

  # fetch in parallel
  "$HEADLESS" webfetch "$url" > "$h_file" 2>/dev/null &
  local pid_h=$!
  defuddle parse --markdown "$url" > "$d_file" 2>/dev/null &
  local pid_d=$!
  curl -sL -m 20 "$url" > "$c_file" 2>/dev/null &
  local pid_c=$!
  wait "$pid_h" "$pid_d" "$pid_c" 2>/dev/null || true

  local h_chars d_chars c_chars
  h_chars=$(wc -c < "$h_file" | tr -d ' ')
  d_chars=$(wc -c < "$d_file" | tr -d ' ')
  c_chars=$(wc -c < "$c_file" | tr -d ' ')

  # ── check: headless extracted more than raw curl? ────────────────
  # (it won't be — curl returns raw HTML; this is a sanity gate that
  # headless produced *something*, not nothing)
  if (( h_chars > 0 )); then
    pass "headless returned content (${h_chars} chars)"
  else
    fail "headless returned NO content"
    FAILURES+=("$id: headless returned no content")
    return
  fi

  # ── check: not drastically smaller than defuddle ─────────────────
  if (( d_chars > 0 )); then
    local ratio
    ratio=$(( h_chars * 100 / d_chars ))
    if (( ratio >= 60 )); then
      pass "headless/defuddle size ratio: ${ratio}% (${h_chars} vs ${d_chars})"
    else
      fail "headless is much smaller than defuddle: ${ratio}% (${h_chars} vs ${d_chars})"
      FAILURES+=("$id: headless/defuddle ratio ${ratio}%")
    fi
  else
    info "defuddle returned nothing — skipping size comparison"
  fi

  # ── check: content body meets min_chars ──────────────────────────
  # strip the metadata header (everything up to and including the --- line)
  local body_chars
  body_chars=$(sed -n '/^---$/,$ p' "$h_file" | tail -n +2 | wc -c | tr -d ' ')
  if (( body_chars >= min_chars )); then
    pass "body content: ${body_chars} chars (min ${min_chars})"
  else
    fail "body content too small: ${body_chars} chars (min ${min_chars})"
    FAILURES+=("$id: body ${body_chars} < min ${min_chars}")
  fi

  # ── check: no content duplication ────────────────────────────────
  # Only count lines with 40+ chars to avoid false positives from blank
  # lines, code braces, code fence markers, and other short repeats.
  local total_lines unique_lines
  total_lines=$(sed -n '/^---$/,$ p' "$h_file" | tail -n +2 \
    | awk 'length >= 40' | wc -l | tr -d ' ')
  unique_lines=$(sed -n '/^---$/,$ p' "$h_file" | tail -n +2 \
    | awk 'length >= 40' | sort -u | wc -l | tr -d ' ')
  if (( total_lines <= 2 )); then
    pass "duplication check: not enough long lines to measure"
  else
    local dup_ratio=$(( unique_lines * 100 / total_lines ))
    if (( dup_ratio >= 75 )); then
      pass "duplication check: ${unique_lines}/${total_lines} unique long lines (${dup_ratio}%)"
    else
      fail "possible content duplication: only ${unique_lines}/${total_lines} unique long lines (${dup_ratio}%)"
      FAILURES+=("$id: duplication — ${dup_ratio}% unique")
    fi
  fi

  # ── check: required markers present ──────────────────────────────
  if [[ -n "$markers_raw" ]]; then
    IFS='|' read -ra markers <<< "$markers_raw"
    for marker in "${markers[@]}"; do
      if grep -qF "$marker" "$h_file"; then
        pass "marker: \"$marker\""
      else
        fail "missing marker: \"$marker\""
        FAILURES+=("$id: missing marker \"$marker\"")
      fi
    done
  fi

  # ── check: headless always beats raw curl ────────────────────────
  # Curl returns raw HTML, so headless should always be smaller (extracted)
  # but still meaningful.  A headless result *bigger* than curl is fine
  # (e.g., link expansion).  A headless result of 0 when curl has content
  # is a failure (caught above).
  if (( c_chars > 0 && h_chars > 0 )); then
    local compression=$(( h_chars * 100 / c_chars ))
    info "extraction ratio vs raw HTML: ${compression}% (${h_chars} / ${c_chars})"
  fi
}

# ── main ─────────────────────────────────────────────────────────────
FAILURES=()
FILTER="${1:-}"

printf "${BOLD}bench_webfetch${RESET} — headless vs defuddle vs curl\n"
printf "output dir: %s\n" "$OUTDIR"

matched=0
for case_def in "${CASES[@]}"; do
  case_id="${case_def%%|*}"
  if [[ -n "$FILTER" && "$case_id" != "$FILTER" ]]; then
    continue
  fi
  matched=$((matched + 1))
  run_case "$case_def"
done

if (( matched == 0 )); then
  echo "error: no case matched filter '$FILTER'"
  echo "available: ${CASES[*]%%|*}"
  exit 1
fi

# ── summary table ────────────────────────────────────────────────────
printf "\n${BOLD}── Size comparison ──${RESET}\n"
printf "%-25s %10s %10s %10s\n" "Case" "Headless" "Defuddle" "Curl"
printf "%-25s %10s %10s %10s\n" "────" "────────" "────────" "────"
for case_def in "${CASES[@]}"; do
  case_id="${case_def%%|*}"
  if [[ -n "$FILTER" && "$case_id" != "$FILTER" ]]; then
    continue
  fi
  h=$(wc -c < "$OUTDIR/${case_id}_headless.txt" 2>/dev/null | tr -d ' ' || echo 0)
  d=$(wc -c < "$OUTDIR/${case_id}_defuddle.txt" 2>/dev/null | tr -d ' ' || echo 0)
  c=$(wc -c < "$OUTDIR/${case_id}_curl.txt" 2>/dev/null | tr -d ' ' || echo 0)
  printf "%-25s %10s %10s %10s\n" "$case_id" "$h" "$d" "$c"
done

# ── result ───────────────────────────────────────────────────────────
printf "\n"
if (( ${#FAILURES[@]} == 0 )); then
  printf "${GREEN}${BOLD}All checks passed.${RESET}\n"
else
  printf "${RED}${BOLD}%d failure(s):${RESET}\n" "${#FAILURES[@]}"
  for f in "${FAILURES[@]}"; do
    printf "  ${RED}•${RESET} %s\n" "$f"
  done
  exit 1
fi
