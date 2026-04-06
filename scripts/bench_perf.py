#!/usr/bin/env python3
"""Manual performance benchmark harness for headless.

This script focuses on two classes of checks:
1. deterministic local benchmarks driven by a fake OpenRouter-compatible server
2. optional live webfetch benchmarks against public pages

The local section is designed to be repeatable enough to compare changes on the
same machine over time. The live webfetch section is intentionally noisier and
should be treated as observational.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_BIN = REPO_ROOT / "target" / "release" / "headless"
LOREM = (
    "Rust-first session runner with durable transcripts, tool orchestration, "
    "and deterministic filesystem behavior for repeatable automation workloads. "
)


@dataclass(frozen=True)
class RepeatProfile:
    basic_cli_repeat: int
    basic_cli_warmup: int
    session_repeat: int
    session_warmup: int
    prompt_repeat: int
    prompt_warmup: int
    live_repeat: int
    session_counts: tuple[int, ...]
    history_turns: tuple[int, ...]


FULL_PROFILE = RepeatProfile(
    basic_cli_repeat=60,
    basic_cli_warmup=5,
    session_repeat=20,
    session_warmup=3,
    prompt_repeat=12,
    prompt_warmup=2,
    live_repeat=6,
    session_counts=(100, 1000, 5000),
    history_turns=(0, 100, 1000),
)

QUICK_PROFILE = RepeatProfile(
    basic_cli_repeat=15,
    basic_cli_warmup=2,
    session_repeat=6,
    session_warmup=1,
    prompt_repeat=5,
    prompt_warmup=1,
    live_repeat=3,
    session_counts=(100, 1000),
    history_turns=(0, 1000),
)


class FakeProvider:
    def __init__(self, fixture_path: pathlib.Path) -> None:
        self.fixture_path = fixture_path
        self._requests: list[dict[str, Any]] = []
        self._lock = threading.Lock()
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), self._build_handler())
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def _build_handler(self):
        outer = self
        fixture_path = str(self.fixture_path)

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self) -> None:
                if self.path.rstrip("/") != "/chat/completions":
                    self.send_error(404)
                    return

                length = int(self.headers.get("Content-Length", "0"))
                body = self.rfile.read(length)
                payload = json.loads(body or b"{}")
                messages = payload.get("messages", [])
                session_id = str(payload.get("session_id") or "")
                with outer._lock:
                    outer._requests.append(
                        {
                            "session_id": session_id,
                            "bytes": len(body),
                            "messages": len(messages),
                            "tools": len(payload.get("tools", [])),
                        }
                    )

                has_tool_result = any(
                    message.get("role") == "tool" for message in messages if isinstance(message, dict)
                )
                if session_id.startswith("tool-") and not has_tool_result:
                    response = {
                        "choices": [
                            {
                                "message": {
                                    "content": None,
                                    "tool_calls": [
                                        {
                                            "id": "call_read_file_1",
                                            "type": "function",
                                            "function": {
                                                "name": "read_file",
                                                "arguments": json.dumps({"path": fixture_path}),
                                            },
                                        }
                                    ],
                                }
                            }
                        ],
                        "usage": {
                            "prompt_tokens": 128,
                            "completion_tokens": 8,
                            "total_tokens": 136,
                        },
                    }
                else:
                    response = {
                        "choices": [{"message": {"content": "ok"}}],
                        "usage": {
                            "prompt_tokens": 128,
                            "completion_tokens": 2,
                            "total_tokens": 130,
                        },
                    }

                data = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, _fmt: str, *_args: object) -> None:
                return

        return Handler

    @property
    def base_url(self) -> str:
        host, port = self._server.server_address
        return f"http://{host}:{port}"

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=1.0)

    def request_stats(self, prefix: str) -> dict[str, float]:
        with self._lock:
            matching = [entry for entry in self._requests if entry["session_id"].startswith(prefix)]
        if not matching:
            return {"avg_request_bytes": 0.0, "avg_messages": 0.0}
        return {
            "avg_request_bytes": round(statistics.fmean(entry["bytes"] for entry in matching), 1),
            "avg_messages": round(statistics.fmean(entry["messages"] for entry in matching), 1),
        }


class Workspace:
    def __init__(self, root: pathlib.Path, provider_url: str) -> None:
        self.root = root
        self.repo = root / "repo"
        self.home_root = root / "home-root"
        self.sessions = root / "sessions"
        self.home = root / "home"
        self.worktree = root / "worktree"
        for path in [
            self.repo / "agents",
            self.repo / "prompts",
            self.repo / "roles",
            self.home_root,
            self.sessions,
            self.home,
            self.worktree,
        ]:
            path.mkdir(parents=True, exist_ok=True)

        (self.repo / "config.toml").write_text(
            "\n".join(
                [
                    f'sessions_dir = "{self.sessions}"',
                    'shell = "/bin/bash"',
                    'shell_args = ["-lc"]',
                    "max_stdin_bytes = 1048576",
                    "artifact_preview_bytes = 256",
                    "catastrophic_output_bytes = 65536",
                    'default_agent = "coder"',
                    'api_key_env = "OPENROUTER_API_KEY"',
                    "",
                ]
            )
        )
        (self.repo / "agents" / "coder.toml").write_text(
            "\n".join(
                [
                    'name = "coder"',
                    'description = "bench coder"',
                    f'base_url = "{provider_url}"',
                    'api_key_env = "OPENROUTER_API_KEY"',
                    'default_model = "openai/gpt-4.1"',
                    'default_effort = "medium"',
                    "max_output_tokens = 12000",
                    "compaction_at_tokens = 180000",
                    'enabled_tools = ["read_file", "edit_file", "write_file", "glob", "grep", "apply_patch", "bash", "web_search", "web_fetch"]',
                    'system_prompt_file = "../prompts/coder.md"',
                    'timeout = "2h"',
                    "",
                ]
            )
        )
        (self.repo / "prompts" / "coder.md").write_text("performance benchmark prompt")
        (self.repo / "roles" / "auditor.toml").write_text(
            "\n".join(
                [
                    'name = "auditor"',
                    'description = "auditor role"',
                    'system_prompt_file = "../prompts/auditor.md"',
                    'user_prefix_file = "../prompts/auditor-user.md"',
                    "",
                ]
            )
        )
        (self.repo / "prompts" / "auditor.md").write_text("auditor system")
        (self.repo / "prompts" / "auditor-user.md").write_text("risk-focused prefix")
        (self.worktree / "fixture.txt").write_text(
            "Fixture file for the repeatable tool-loop performance benchmark.\n"
        )

    @property
    def env(self) -> dict[str, str]:
        env = os.environ.copy()
        env.update(
            {
                "HEADLESS_REPO_ROOT": str(self.repo),
                "HEADLESS_HOME_ROOT": str(self.home_root),
                "HOME": str(self.home),
                "OPENROUTER_API_KEY": "test-key",
            }
        )
        return env


def ensure_release_binary(binary: pathlib.Path) -> None:
    if binary.exists():
        return
    subprocess.run(
        ["cargo", "build", "--release"],
        cwd=REPO_ROOT,
        check=True,
    )


def summarize(samples: list[float]) -> dict[str, float]:
    ordered = sorted(samples)
    p95_index = min(len(ordered) - 1, max(0, int(len(ordered) * 0.95) - 1))
    return {
        "mean_ms": round(statistics.fmean(samples), 3),
        "median_ms": round(statistics.median(samples), 3),
        "p95_ms": round(ordered[p95_index], 3),
        "min_ms": round(ordered[0], 3),
        "max_ms": round(ordered[-1], 3),
    }


def run_checked(
    cmd: list[str],
    *,
    env: dict[str, str],
    cwd: pathlib.Path,
    capture: bool = False,
) -> subprocess.CompletedProcess[str]:
    kwargs: dict[str, Any] = {"cwd": cwd, "env": env, "text": True}
    if capture:
        kwargs["stdout"] = subprocess.PIPE
        kwargs["stderr"] = subprocess.PIPE
    else:
        kwargs["stdout"] = subprocess.DEVNULL
        kwargs["stderr"] = subprocess.DEVNULL
    proc = subprocess.run(cmd, **kwargs)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\n"
            f"stdout={getattr(proc, 'stdout', '')}\n"
            f"stderr={getattr(proc, 'stderr', '')}"
        )
    return proc


def bench_cmd(
    cmd: list[str],
    *,
    env: dict[str, str],
    cwd: pathlib.Path,
    repeat: int,
    warmup: int,
) -> tuple[dict[str, float], subprocess.CompletedProcess[str]]:
    proc = run_checked(cmd, env=env, cwd=cwd, capture=True)
    for _ in range(warmup):
        run_checked(cmd, env=env, cwd=cwd)
    samples: list[float] = []
    for _ in range(repeat):
        started = time.perf_counter()
        run_checked(cmd, env=env, cwd=cwd)
        samples.append((time.perf_counter() - started) * 1000)
    return summarize(samples), proc


def measure_memory(cmd: list[str], *, env: dict[str, str], cwd: pathlib.Path) -> dict[str, int | None]:
    proc = subprocess.run(
        ["/usr/bin/time", "-l", *cmd],
        cwd=cwd,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    max_rss = None
    peak_mem = None
    for line in proc.stderr.splitlines():
        match = re.search(r"^\s*(\d+)\s+maximum resident set size$", line)
        if match:
            max_rss = int(match.group(1))
        match = re.search(r"^\s*(\d+)\s+peak memory footprint$", line)
        if match:
            peak_mem = int(match.group(1))
    return {"max_rss_bytes": max_rss, "peak_mem_bytes": peak_mem}


def rfc3339(seconds: int) -> str:
    base = datetime(2026, 1, 1, tzinfo=timezone.utc)
    return (base + timedelta(seconds=seconds)).isoformat().replace("+00:00", "Z")


def seed_session(workspace: Workspace, session_id: str, turns: int) -> None:
    session_dir = workspace.sessions / session_id
    (session_dir / "runs").mkdir(parents=True, exist_ok=True)
    (session_dir / "lock").write_text("")
    (session_dir / "execution.lock").write_text("")
    records: list[dict[str, Any]] = []
    char_count = 0
    for index in range(turns):
        user = f"User turn {index}. " + LOREM * 2
        assistant = f"Assistant turn {index}. " + LOREM * 2
        char_count += len(user) + len(assistant)
        records.append(
            {
                "v": 1,
                "ts": rfc3339(index * 2),
                "run_id": "seed",
                "role": "user",
                "content": user,
            }
        )
        records.append(
            {
                "v": 1,
                "ts": rfc3339(index * 2 + 1),
                "run_id": "seed",
                "role": "assistant",
                "content": assistant,
            }
        )
    with (session_dir / "messages.jsonl").open("w") as handle:
        for record in records:
            handle.write(json.dumps(record))
            handle.write("\n")
    meta = {
        "session_id": session_id,
        "created_at": rfc3339(0),
        "updated_at": rfc3339(max(1, turns * 2)),
        "stopped_at": None,
        "revision": max(1, turns),
        "char_count": char_count,
        "agent_name": "coder",
        "model": "openai/gpt-4.1",
        "initial_role": None,
        "cwd": str(workspace.worktree),
        "effort": "medium",
    }
    (session_dir / "meta.json").write_text(json.dumps(meta, indent=2))


def seed_active_sessions(workspace: Workspace, count: int) -> None:
    for index in range(count):
        session_id = f"s{index:05d}"
        session_dir = workspace.sessions / session_id
        (session_dir / "runs").mkdir(parents=True, exist_ok=True)
        (session_dir / "lock").write_text("")
        (session_dir / "execution.lock").write_text("")
        meta = {
            "session_id": session_id,
            "created_at": rfc3339(index),
            "updated_at": rfc3339(index),
            "stopped_at": None,
            "revision": 1,
            "char_count": 1,
            "agent_name": "coder",
            "model": "openai/gpt-4.1",
            "initial_role": None,
            "cwd": str(workspace.worktree),
            "effort": "medium",
        }
        (session_dir / "meta.json").write_text(json.dumps(meta))
        (session_dir / "messages.jsonl").write_text("")


def parallel_bench(shell_cmd: str, *, env: dict[str, str], cwd: pathlib.Path) -> float:
    started = time.perf_counter()
    subprocess.run(
        ["/bin/bash", "-lc", shell_cmd],
        cwd=cwd,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=True,
        check=True,
    )
    return time.perf_counter() - started


def bench_prompt(
    binary: pathlib.Path,
    *,
    workspace: Workspace,
    provider: FakeProvider,
    label: str,
    turns: int,
    repeat: int,
    warmup: int,
) -> dict[str, float]:
    prefix = f"{label}-{turns}"
    for index in range(repeat + warmup + 1):
        session_id = f"{prefix}-{turns:04d}-{index:04d}"
        seed_session(workspace, session_id, turns)
        cmd = [str(binary), "--session", session_id, "history probe"]
        if index == 0:
            proc = run_checked(cmd, env=workspace.env, cwd=workspace.worktree, capture=True)
            if proc.stdout.strip() != "ok":
                raise RuntimeError(f"unexpected prompt output for {session_id}: {proc.stdout!r}")
            continue
        yield_time = time.perf_counter()
        run_checked(cmd, env=workspace.env, cwd=workspace.worktree)
        elapsed = (time.perf_counter() - yield_time) * 1000
        if index == warmup:
            samples = []
        if index >= warmup:
            samples.append(elapsed)
    result = summarize(samples)
    result.update(provider.request_stats(prefix))
    return result


def run_local_benchmarks(binary: pathlib.Path, profile: RepeatProfile) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="headless-bench-") as temp_dir:
        root = pathlib.Path(temp_dir)
        fixture_dir = root / "fixture"
        fixture_dir.mkdir(parents=True, exist_ok=True)
        fixture_path = fixture_dir / "tool-fixture.txt"
        fixture_path.write_text("read_file benchmark fixture\n")

        provider = FakeProvider(fixture_path)
        provider.start()
        try:
            basic_workspace = Workspace(root / "basic", provider.base_url)
            results: dict[str, Any] = {
                "environment": {
                    "machine": os.uname().machine,
                    "platform": " ".join(os.uname()),
                    "binary": str(binary),
                    "profile": "quick" if profile is QUICK_PROFILE else "full",
                },
                "basic_cli": {},
                "memory": {},
            }

            for label, cmd in [
                ("version", [str(binary), "version"]),
                ("agent_list", [str(binary), "agent", "list"]),
                ("role_list", [str(binary), "role", "list"]),
                ("session_new", [str(binary), "session", "new"]),
            ]:
                stats, _ = bench_cmd(
                    cmd,
                    env=basic_workspace.env,
                    cwd=basic_workspace.worktree,
                    repeat=profile.basic_cli_repeat,
                    warmup=profile.basic_cli_warmup,
                )
                results["basic_cli"][label] = stats

            results["memory"]["version"] = measure_memory(
                [str(binary), "version"],
                env=basic_workspace.env,
                cwd=basic_workspace.worktree,
            )

            session_scale: dict[str, Any] = {}
            for count in profile.session_counts:
                workspace = Workspace(root / f"sessions-{count}", provider.base_url)
                seed_active_sessions(workspace, count)
                list_stats, _ = bench_cmd(
                    [str(binary), "session", "list"],
                    env=workspace.env,
                    cwd=workspace.worktree,
                    repeat=profile.session_repeat,
                    warmup=profile.session_warmup,
                )
                last_stats, _ = bench_cmd(
                    [str(binary), "session", "last"],
                    env=workspace.env,
                    cwd=workspace.worktree,
                    repeat=profile.session_repeat,
                    warmup=profile.session_warmup,
                )
                session_scale[str(count)] = {
                    "session_list": list_stats,
                    "session_last": last_stats,
                }
            results["session_scale"] = session_scale

            prompt_workspace = Workspace(root / "prompt", provider.base_url)
            prompt_scale: dict[str, Any] = {}
            for turns in profile.history_turns:
                prompt_scale[str(turns)] = bench_prompt(
                    binary,
                    workspace=prompt_workspace,
                    provider=provider,
                    label="single",
                    turns=turns,
                    repeat=profile.prompt_repeat,
                    warmup=profile.prompt_warmup,
                )
            prompt_scale["tool_loop_0"] = bench_prompt(
                binary,
                workspace=prompt_workspace,
                provider=provider,
                label="tool",
                turns=0,
                repeat=profile.prompt_repeat,
                warmup=profile.prompt_warmup,
            )
            results["prompt_scale"] = prompt_scale

            seed_session(prompt_workspace, "mem-1000", 1000)
            results["memory"]["prompt_1000_turns"] = measure_memory(
                [str(binary), "--session", "mem-1000", "history probe"],
                env=prompt_workspace.env,
                cwd=prompt_workspace.worktree,
            )

            parallel_workspace = Workspace(root / "parallel", provider.base_url)
            version_elapsed = parallel_bench(
                f"seq 200 | xargs -P 32 -I{{}} {binary} version >/dev/null",
                env=parallel_workspace.env,
                cwd=parallel_workspace.worktree,
            )
            session_new_elapsed = parallel_bench(
                f"seq 100 | xargs -P 32 -I{{}} {binary} session new >/dev/null",
                env=parallel_workspace.env,
                cwd=parallel_workspace.worktree,
            )
            results["parallel"] = {
                "version_200_runs_p32": {
                    "total_seconds": round(version_elapsed, 3),
                    "throughput_per_sec": round(200 / version_elapsed, 1),
                },
                "session_new_100_runs_p32": {
                    "total_seconds": round(session_new_elapsed, 3),
                    "throughput_per_sec": round(100 / session_new_elapsed, 1),
                },
            }

            return results
        finally:
            provider.stop()


def bench_live_webfetch(binary: pathlib.Path, repeat: int) -> dict[str, Any]:
    urls = {
        "example": "https://example.com/",
        "rust_book": "https://doc.rust-lang.org/book/ch01-01-installation.html",
        "python_tutorial": "https://docs.python.org/3.12/tutorial/controlflow.html",
    }

    def bench(cmd: list[str]) -> dict[str, float]:
        run_checked(cmd, env=os.environ.copy(), cwd=REPO_ROOT)
        samples: list[float] = []
        for _ in range(repeat):
            started = time.perf_counter()
            run_checked(cmd, env=os.environ.copy(), cwd=REPO_ROOT)
            samples.append((time.perf_counter() - started) * 1000)
        return {
            "mean_ms": round(statistics.fmean(samples), 3),
            "median_ms": round(statistics.median(samples), 3),
            "min_ms": round(min(samples), 3),
            "max_ms": round(max(samples), 3),
        }

    results: dict[str, Any] = {}
    for label, url in urls.items():
        try:
            headless = bench([str(binary), "webfetch", url])
            curl = bench(["curl", "-sS", url])
            results[label] = {
                "url": url,
                "headless": headless,
                "curl": curl,
                "mean_delta_ms": round(headless["mean_ms"] - curl["mean_ms"], 3),
                "mean_ratio": round(headless["mean_ms"] / curl["mean_ms"], 2),
            }
        except Exception as exc:  # pragma: no cover - best effort manual path
            results[label] = {"url": url, "error": str(exc)}
    return results


def render_summary(results: dict[str, Any]) -> str:
    lines = []
    lines.append("bench_perf")
    lines.append(f"binary: {results['environment']['binary']}")
    lines.append(f"profile: {results['environment']['profile']}")
    lines.append("")

    lines.append("Basic CLI")
    for label, stats in results["basic_cli"].items():
        lines.append(
            f"  {label:12s} mean={stats['mean_ms']:7.3f} ms "
            f"median={stats['median_ms']:7.3f} ms p95={stats['p95_ms']:7.3f} ms"
        )

    lines.append("")
    lines.append("Session Scale")
    for count, stats in results["session_scale"].items():
        lines.append(
            f"  {count:>5s} sessions  "
            f"list={stats['session_list']['mean_ms']:7.3f} ms  "
            f"last={stats['session_last']['mean_ms']:7.3f} ms"
        )

    lines.append("")
    lines.append("Prompt Scale")
    for label, stats in results["prompt_scale"].items():
        lines.append(
            f"  {label:12s} mean={stats['mean_ms']:7.3f} ms "
            f"req={stats['avg_request_bytes']:9.1f} B messages={stats['avg_messages']:7.1f}"
        )

    lines.append("")
    lines.append("Parallel")
    for label, stats in results["parallel"].items():
        lines.append(
            f"  {label:22s} total={stats['total_seconds']:6.3f} s "
            f"throughput={stats['throughput_per_sec']:7.1f}/s"
        )

    lines.append("")
    lines.append("Memory")
    for label, stats in results["memory"].items():
        lines.append(
            f"  {label:16s} max_rss={stats['max_rss_bytes']} peak={stats['peak_mem_bytes']}"
        )

    if "live_webfetch" in results:
        lines.append("")
        lines.append("Live Webfetch")
        for label, stats in results["live_webfetch"].items():
            if "error" in stats:
                lines.append(f"  {label:16s} error={stats['error']}")
                continue
            lines.append(
                f"  {label:16s} headless={stats['headless']['mean_ms']:7.3f} ms "
                f"curl={stats['curl']['mean_ms']:7.3f} ms ratio={stats['mean_ratio']:5.2f}x"
            )

    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=pathlib.Path, default=DEFAULT_BIN)
    parser.add_argument(
        "--quick",
        action="store_true",
        help="run a shorter benchmark profile for fast iteration",
    )
    parser.add_argument(
        "--live-webfetch",
        action="store_true",
        help="include observational live webfetch comparisons against curl",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="print the full result payload as JSON instead of a human summary",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    profile = QUICK_PROFILE if args.quick else FULL_PROFILE
    ensure_release_binary(args.bin)

    results = run_local_benchmarks(args.bin, profile)
    if args.live_webfetch:
        results["live_webfetch"] = bench_live_webfetch(args.bin, profile.live_repeat)

    if args.json:
        print(json.dumps(results, indent=2))
    else:
        print(render_summary(results))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
