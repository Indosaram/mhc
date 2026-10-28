#!/usr/bin/env python3
"""Smoke-test an MIT-profile `mhc` binary (maho-cli built with `--no-default-features`).

Runs `mhc -p` against a loopback OpenAI-compatible model that first calls the builtin `write` tool
and then answers in text, and checks that:

- basic chat works (the final text reaches stdout, exit 0);
- builtin tools are offered (read, write, edit, and the shell: `bash` or the terminal's `bash_*`
  tools) and run (the file is written);
- no omo tool is offered (team_*, memory, workpool, ...); the MIT LSP tools (`maho-lsp`) and the MIT
  delegation tools (`maho-task`: task, task_output, task_list, task_resume, task_cancel) are required;
- the `task` tool delegates: a child `mhc -p` process answers through the same loopback model and
  its result comes back to the parent as a completed `<task-result>`;
- isolated delegation (`maho-isolation`): in a git repository a `task` call with `isolated: true`
  runs the child in a git worktree, the file the child writes there is applied back to the parent
  checkout, the result note says so and the worktree is removed;
- team runs (`maho-team`: team_create, team_members, team_message, team_wait, team_stop): a
  model-driven `team_create` starts two members as real child `mhc -p` processes and `team_wait`
  collects both results; when `tmux` is on PATH the same run is repeated attached to a private tmux
  server and each member is shown in a pane that follows its output and closes afterwards;
- the read-only git tools (`maho-git`: git_overview, git_file_diff, git_hunk, recent_commits) are
  offered and `git_overview` reports the repository's changes;
- the MIT memory tools (`maho-memory`: retain, recall, memory_get, memory_forget) are offered; a
  fact stored with `retain` in one run is auto-recalled into the next run's first turn and found by
  the `recall` tool;
- `mhc memory import-legacy` imports a legacy Markdown store and `mhc memory search` finds it;
- the memory MCP server (`mhc --memory-mcp`, legacy `--omo-memory-mcp`) answers initialize,
  tools/list and tools/call;
- the MIT browser/computer-use tools (`maho-browser`: browser, computer) are offered; when
  `MHC_SMOKE_BROWSER` names a Chromium executable, a model-driven `browser` call opens a page and
  its accessibility snapshot (with element refs) comes back to the model, and a `computer`
  screenshot of that tab is taken;
- telemetry (`maho-telemetry`): every model call appends a line to the local usage ledger
  (`<agent dir>/stats/usage.jsonl`) and `mhc usage` reports it; with an OTLP endpoint configured
  the run is exported to a loopback collector as OTLP/HTTP protobuf (traces, metrics, logs, no
  prompt text); `DO_NOT_TRACK=1` stops the export but keeps the ledger; `MAHO_TELEMETRY=0` stops
  both;
- skills (`maho-skills`) and MCP (`maho-ext-mcp` + `maho-mcp-builtins`): a user skill and a
  bundled skill (`MAHO_SKILLS_ROOT`) are listed in the system prompt while a skill named in
  `disabled_skills` is not; the `skill` tool is offered and a model call loads the bundled skill's
  instructions; a stdio MCP server configured in `<agent dir>/mcp.json` is started, its tool is
  offered as `mcp_smokefake_echo` and a model call to it returns the server's answer; the builtin
  remote MCP servers (`context7`, `grep_app`) are not declared in offline mode;
- the session gateway (`maho-gateway`): an RPC session registers as a live session (`mhc gateway
  list --json`); `mhc gateway send` delivers a message into it, which reaches its model with the
  gateway header; a `-p` run whose model calls `session_list` and `session_send` relays a message
  into the same RPC session; the session is unlisted after it exits; `MAHO_GATEWAY=0` hides the
  tools;
- `mhc doctor` reports the configuration sources and resolves a pinned model profile;
- the omo-only entries report that they are unavailable (`mhc ulw-loop`, `--memory-supervisor`).

Usage: python3 scripts/mit-profile-smoke.py <path-to-mhc>
See docs/MIT-PROFILE.md.
"""

from __future__ import annotations

import http.server
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import threading

MARKER = "mit-profile-smoke-ok"
REQUIRED_TOOLS = {"read", "write", "edit"}
# Tools the omo port registers; none may appear in the MIT profile.
OMO_TOOL_PREFIXES = ("task", "team_", "workpool", "memory", "x_search", "omo_")
# MIT replacements that are expected in the MIT profile.
MIT_TASK_TOOLS = {"task", "task_output", "task_list", "task_resume", "task_cancel"}
MIT_GIT_TOOLS = {"git_overview", "git_file_diff", "git_hunk", "recent_commits"}
MIT_TEAM_TOOLS = {"team_create", "team_members", "team_message", "team_wait", "team_stop"}
TEAM_MARKER = "mit-profile-team-ok"
ISOLATED_FILE = "isolated-smoke.txt"
ISOLATED_CONTENT = "written by an isolated child"
ISOLATION_MARKER = "mit-profile-isolation-ok"
CHILD_MARKER = "mit-profile-child-ok"
PARENT_MARKER = "mit-profile-delegation-ok"
MIT_LSP_TOOLS = {"lsp_diagnostics", "lsp_goto_definition", "lsp_find_references", "lsp_symbols", "lsp_prepare_rename", "lsp_rename"}
UNAVAILABLE = "is not available in this build"
MIT_MEMORY_TOOLS = {"retain", "recall", "memory_get", "memory_forget"}
MEMORY_FACT = "The smoke deployment target is cluster-zeta-42."
MEMORY_MARKER = "mit-profile-memory-ok"
MIT_BROWSER_TOOLS = {"browser", "computer"}
BROWSER_MARKER = "mit-profile-browser-ok"
BROWSER_HEADING = "smoke-browser-heading"
TELEMETRY_MARKER = "mit-profile-telemetry-ok"
TELEMETRY_PROMPT = "telemetry smoke prompt text that must never be exported"
SKILL_MARKER = "mit-profile-skill-ok"
SKILL_BODY = "bundled smoke skill instructions: say the magic word"
MCP_ECHO = "mcp-smoke-ping"
GATEWAY_TOOLS = {"session_list", "session_send"}
GATEWAY_CLI_TEXT = "gateway-cli-ping"
GATEWAY_AGENT_TEXT = "gateway-agent-ping"
GATEWAY_CLI_MARKER = "mit-profile-gateway-cli-ok"
GATEWAY_AGENT_MARKER = "mit-profile-gateway-agent-ok"
GATEWAY_RELAY_MARKER = "mit-profile-gateway-relay-ok"
GATEWAY_HEADER = "[gateway message from "
FAKE_MCP_SERVER = r"""
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if "id" not in msg:
        continue
    method, mid = msg.get("method"), msg["id"]
    if method == "initialize":
        result = {"protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "smokefake", "version": "1.0.0"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo", "description": "Echo text back with a prefix",
                             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}},
                                             "required": ["text"]}}]}
    elif method == "tools/call":
        text = msg.get("params", {}).get("arguments", {}).get("text", "")
        result = {"content": [{"type": "text", "text": "mcp-smoke-" + text}]}
    elif method == "ping":
        result = {}
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid,
                                     "error": {"code": -32601, "message": "method not found"}}) + "\n")
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"""
OTEL_VARIABLES = ("OTEL_EXPORTER_OTLP_ENDPOINT", "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                  "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                  "OTEL_SDK_DISABLED", "DO_NOT_TRACK", "MAHO_TELEMETRY", "MAHO_USAGE_STATS",
                  "MAHO_USAGE_DIR", "OTEL_TRACES_EXPORTER", "OTEL_METRICS_EXPORTER", "OTEL_LOGS_EXPORTER",
                  "OTEL_EXPORTER_OTLP_PROTOCOL")


def sse(chunks: list[dict]) -> bytes:
    lines = [f"data: {json.dumps(chunk)}\n\n" for chunk in chunks]
    lines.append("data: [DONE]\n\n")
    return "".join(lines).encode()


def chunk(delta: dict, finish: str | None = None) -> dict:
    return {
        "id": "smoke", "object": "chat.completion.chunk", "created": 0, "model": "offline",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    }


class Model(http.server.BaseHTTPRequestHandler):
    requests: list[dict] = []

    def log_message(self, *_args) -> None:  # keep test output clean
        pass

    def do_POST(self) -> None:  # noqa: N802 (http.server API)
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        Model.requests.append(payload)
        if len(Model.requests) == 1:
            arguments = json.dumps({"path": "smoke.txt", "content": MARKER})
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": "call_smoke", "type": "function",
                    "function": {"name": "write", "arguments": arguments},
                }]}),
                chunk({}, "tool_calls"),
            ])
        else:
            body = sse([chunk({"role": "assistant", "content": MARKER}), chunk({}, "stop")])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


class DelegateModel(http.server.BaseHTTPRequestHandler):
    """Parent: calls `task`, then answers. Child (no `task` tool at depth 1): answers CHILD_MARKER."""
    requests: list[dict] = []

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        DelegateModel.requests.append(payload)
        offered = tool_names(payload)
        has_tool_result = any(message.get("role") == "tool" for message in payload.get("messages") or [])
        if "task" not in offered:
            body = sse([chunk({"role": "assistant", "content": CHILD_MARKER}), chunk({}, "stop")])
        elif not has_tool_result:
            arguments = json.dumps({"task": "Reply with the child marker.", "name": "SmokeChild"})
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": "call_task", "type": "function",
                    "function": {"name": "task", "arguments": arguments},
                }]}),
                chunk({}, "tool_calls"),
            ])
        else:
            body = sse([chunk({"role": "assistant", "content": PARENT_MARKER}), chunk({}, "stop")])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


class IsolationModel(http.server.BaseHTTPRequestHandler):
    """Parent: `task` (isolated), then `git_overview`, then answers. Child: `write`, then answers."""
    requests: list[dict] = []

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        IsolationModel.requests.append(payload)
        offered = tool_names(payload)
        results = sum(1 for message in payload.get("messages") or [] if message.get("role") == "tool")
        call = None
        if "task" not in offered:
            if results == 0:
                call = ("write", {"path": ISOLATED_FILE, "content": ISOLATED_CONTENT})
            else:
                text = CHILD_MARKER
        elif results == 0:
            call = ("task", {"task": "Write the smoke file.", "name": "IsoChild", "isolated": True})
        elif results == 1:
            call = ("git_overview", {"staged": False, "include_untracked": True})
        else:
            text = ISOLATION_MARKER
        if call:
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": f"call_{call[0]}", "type": "function",
                    "function": {"name": call[0], "arguments": json.dumps(call[1])},
                }]}),
                chunk({}, "tool_calls"),
            ])
        else:
            body = sse([chunk({"role": "assistant", "content": text}), chunk({}, "stop")])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


class TeamModel(http.server.BaseHTTPRequestHandler):
    """Lead: `team_create` (two members), then `team_wait`, then answers. Members answer with their
    own marker taken from the assignment text."""
    requests: list[dict] = []

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        TeamModel.requests.append(payload)
        offered = tool_names(payload)
        results = sum(1 for message in payload.get("messages") or [] if message.get("role") == "tool")
        call = None
        text = TEAM_MARKER
        if "team_create" not in offered:
            prompt = json.dumps(payload.get("messages") or [])
            text = "team-member-alpha-ok" if "alpha assignment" in prompt else "team-member-beta-ok"
        elif results == 0:
            call = ("team_create", {"name": "smoke", "context": "Smoke team.", "members": [
                {"name": "alpha", "task": "alpha assignment: reply with your marker"},
                {"name": "beta", "task": "beta assignment: reply with your marker"},
            ]})
        elif results == 1:
            call = ("team_wait", {"timeout_ms": 120000})
        if call:
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": f"call_{call[0]}", "type": "function",
                    "function": {"name": call[0], "arguments": json.dumps(call[1])},
                }]}),
                chunk({}, "tool_calls"),
            ])
        else:
            body = sse([chunk({"role": "assistant", "content": text}), chunk({}, "stop")])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


class MemoryModel(http.server.BaseHTTPRequestHandler):
    """`phase` "retain": calls `retain` with MEMORY_FACT; "recall": calls `recall`; then answers."""
    requests: list[dict] = []
    phase = "retain"

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        MemoryModel.requests.append(payload)
        has_tool_result = any(message.get("role") == "tool" for message in payload.get("messages") or [])
        if has_tool_result:
            body = sse([chunk({"role": "assistant", "content": MEMORY_MARKER}), chunk({}, "stop")])
        else:
            if MemoryModel.phase == "retain":
                name, arguments = "retain", {"items": [{"content": MEMORY_FACT, "context": "smoke"}]}
            else:
                name, arguments = "recall", {"query": "smoke deployment cluster"}
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": f"call_{name}", "type": "function",
                    "function": {"name": name, "arguments": json.dumps(arguments)},
                }]}),
                chunk({}, "tool_calls"),
            ])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


class BrowserModel(http.server.BaseHTTPRequestHandler):
    """Calls `browser` (open + snapshot), then `computer` (screenshot of that tab), then answers."""
    requests: list[dict] = []

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        BrowserModel.requests.append(payload)
        tool_results = [m for m in payload.get("messages") or [] if m.get("role") == "tool"]
        if len(tool_results) >= 2:
            body = sse([chunk({"role": "assistant", "content": BROWSER_MARKER}), chunk({}, "stop")])
        else:
            if not tool_results:
                name = "browser"
                arguments = {"action": "open", "snapshot": True,
                             "url": f"data:text/html,<title>smoke</title><h1>{BROWSER_HEADING}</h1><button>Go</button>"}
            else:
                name, arguments = "computer", {"action": "screenshot"}
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": f"call_{name}", "type": "function",
                    "function": {"name": name, "arguments": json.dumps(arguments)},
                }]}),
                chunk({}, "tool_calls"),
            ])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)



class TelemetryModel(http.server.BaseHTTPRequestHandler):
    """Answers in text and reports token usage in the final chunk."""

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        self.rfile.read(length)
        final = chunk({}, "stop")
        final["usage"] = {"prompt_tokens": 1200, "completion_tokens": 34, "total_tokens": 1234}
        body = sse([chunk({"role": "assistant", "content": TELEMETRY_MARKER}), final])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


class Collector(http.server.BaseHTTPRequestHandler):
    """A loopback OTLP/HTTP receiver that records (path, content type, body)."""
    received: list[tuple[str, str, bytes]] = []

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        Collector.received.append((self.path, self.headers.get("content-type", ""), self.rfile.read(length)))
        self.send_response(200)
        self.send_header("Content-Type", "application/x-protobuf")
        self.send_header("Content-Length", "0")
        self.end_headers()


def ledger_lines(path: pathlib.Path) -> int:
    return len(path.read_text().splitlines()) if path.is_file() else 0


def mcp_session(mhc: str, flag: str, cwd: pathlib.Path, env: dict, calls: list[dict]) -> list[dict]:
    """Runs the memory MCP server with `flag`, sends initialize, tools/list and `calls`; returns responses."""
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    for index, call in enumerate(calls):
        requests.append({"jsonrpc": "2.0", "id": 3 + index, "method": "tools/call", "params": call})
    result = subprocess.run([mhc, flag], cwd=cwd, env=env, input="\n".join(json.dumps(r) for r in requests) + "\n",
                            capture_output=True, text=True, timeout=60)
    if result.returncode != 0:
        raise RuntimeError(f"`mhc {flag}` exited {result.returncode}: {result.stderr}")
    return [json.loads(line) for line in result.stdout.splitlines() if line.strip()]


def tool_text(payload: dict) -> str:
    parts = []
    for message in payload.get("messages") or []:
        if message.get("role") != "tool":
            continue
        content = message.get("content")
        if isinstance(content, str):
            parts.append(content)
        elif isinstance(content, list):
            parts.extend(str(part.get("text", "")) for part in content if isinstance(part, dict))
    return "\n".join(parts)


def models_json(port: int) -> str:
    return json.dumps({"providers": {"offline": {
        "api": "openai-completions", "baseUrl": f"http://127.0.0.1:{port}/v1",
        "apiKey": "offline-fixture",
        "models": [{"id": "offline", "reasoning": False, "input": ["text"],
                    "contextWindow": 128000, "maxTokens": 4096}],
    }}})


def tool_names(payload: dict) -> set[str]:
    names = set()
    for tool in payload.get("tools") or []:
        function = tool.get("function") or {}
        if function.get("name"):
            names.add(function["name"])
    return names


def fail(message: str) -> int:
    print(f"mit-profile-smoke: FAIL: {message}")
    if os.environ.get("GITHUB_ACTIONS"):
        # Surface the reason as a workflow annotation so it is readable without the job log.
        escaped = message[-6000:].replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
        print(f"::error title=mit-profile-smoke::{escaped}")
    return 1


def team_smoke(mhc: str, home: pathlib.Path, work: pathlib.Path, agent: pathlib.Path, env: dict) -> str | None:
    """Runs the two-member team headless, then (when tmux is on PATH) attached to a private tmux
    server. Returns a failure message or None."""
    import shutil
    import time

    def run_team(extra: dict, label: str) -> subprocess.CompletedProcess | str:
        TeamModel.requests = []
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), TeamModel)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        (agent / "models.json").write_text(models_json(server.server_port))
        run_env = {**env, "PI_OFFLINE": "1", **extra}
        for name in ("TMUX", "TMUX_PANE", "CMUX_WORKSPACE_ID", "CMUX_SURFACE_ID"):
            if name not in extra:
                run_env.pop(name, None)
        run = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "run the smoke team"],
            cwd=work, env=run_env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=240,
        )
        server.shutdown()
        if run.returncode != 0 or TEAM_MARKER not in run.stdout:
            return f"{label} team run failed (exit {run.returncode})\nstdout: {run.stdout}\nstderr: {run.stderr}"
        members = [p for p in TeamModel.requests if "team_create" not in tool_names(p)]
        if len(members) < 2:
            return f"{label}: expected two member requests, got {len(members)}\nstderr: {run.stderr}"
        results = "\n".join(tool_text(p) for p in TeamModel.requests)
        for needle in ("Team smoke created with 2 members", "team smoke: 2/2 done",
                       "team-member-alpha-ok", "team-member-beta-ok"):
            if needle not in results:
                return f"{label}: {needle!r} missing from the tool results\n{results}\nstderr: {run.stderr}"
        return run

    outcome = run_team({"MAHO_TEAM_PANES": "auto"}, "headless")
    if isinstance(outcome, str):
        return outcome
    print("ok   team run (headless): team_create started two child mhc members, team_wait collected both results")

    tmux = shutil.which("tmux", path=env.get("PATH"))
    if not tmux:
        print("skip team panes: tmux not on PATH")
        return None
    socket = str(home / "tmux.sock")
    tmux_cmd = lambda *args: subprocess.run([tmux, "-S", socket, *args], capture_output=True, text=True, env=env)
    tmux_cmd("new-session", "-d", "-s", "lead", "-x", "220", "-y", "60", "sleep 300")
    lead = tmux_cmd("list-panes", "-t", "lead", "-F", "#{pane_id}").stdout.strip()
    server_pid = tmux_cmd("display-message", "-p", "#{pid}").stdout.strip()
    seen: dict[str, str] = {}
    stop = threading.Event()

    def watch() -> None:
        while not stop.is_set():
            for pane in tmux_cmd("list-panes", "-a", "-F", "#{pane_id}").stdout.split():
                if pane != lead:
                    text = tmux_cmd("capture-pane", "-p", "-t", pane).stdout
                    if text.strip():
                        seen[pane] = text
            time.sleep(0.1)

    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()
    try:
        outcome = run_team({"TMUX": f"{socket},{server_pid},0", "TMUX_PANE": lead, "MAHO_TEAM_PANE_LINGER": "1"},
                           "tmux")
        if isinstance(outcome, str):
            return outcome
        deadline = time.time() + 10
        while time.time() < deadline:
            left = [p for p in tmux_cmd("list-panes", "-a", "-F", "#{pane_id}").stdout.split() if p != lead]
            if not left:
                break
            time.sleep(0.2)
        stop.set()
        watcher.join(timeout=5)
        if len(seen) < 2:
            return f"tmux: expected two member panes, saw {sorted(seen)}\nstderr: {outcome.stderr}"
        titles = [text for text in seen.values() if "team smoke" in text]
        if len(titles) < 2:
            return f"tmux: member panes did not show the team title\n{seen}"
        if left:
            return f"tmux: member panes were not closed: {left}"
    finally:
        stop.set()
        tmux_cmd("kill-server")
    print("ok   team run (tmux): each member followed in its own pane (title + live output), panes closed afterwards")
    return None


class SkillMcpModel(http.server.BaseHTTPRequestHandler):
    """Calls `skill` (bundled-skill), then `mcp_smokefake_echo`, then answers SKILL_MARKER."""
    requests: list[dict] = []

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        SkillMcpModel.requests.append(payload)
        results = sum(1 for message in payload.get("messages") or [] if message.get("role") == "tool")
        if results >= 2:
            body = sse([chunk({"role": "assistant", "content": SKILL_MARKER}), chunk({}, "stop")])
        else:
            name, arguments = (("skill", {"name": "bundled-skill"}) if results == 0
                               else ("mcp_smokefake_echo", {"text": "ping"}))
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": f"call_{results}", "type": "function",
                    "function": {"name": name, "arguments": json.dumps(arguments)},
                }]}),
                chunk({}, "tool_calls"),
            ])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


def system_text(payload: dict) -> str:
    parts = []
    for message in payload.get("messages") or []:
        if message.get("role") not in ("system", "developer"):
            continue
        content = message.get("content")
        if isinstance(content, str):
            parts.append(content)
        elif isinstance(content, list):
            parts.extend(str(part.get("text", "")) for part in content if isinstance(part, dict))
    return "\n".join(parts)


def skills_mcp_smoke(mhc: str, home: pathlib.Path, work: pathlib.Path, agent: pathlib.Path, env: dict) -> str | None:
    def skill(root: pathlib.Path, name: str, body: str) -> None:
        (root / name).mkdir(parents=True, exist_ok=True)
        (root / name / "SKILL.md").write_text(
            f"---\nname: {name}\ndescription: Smoke skill {name} for the MIT profile.\n---\n\n{body}\n")

    bundled = home / "bundled-skills"
    skill(agent / "skills", "smoke-skill", "user smoke skill")
    skill(agent / "skills", "hidden-skill", "this skill is disabled")
    skill(bundled, "bundled-skill", SKILL_BODY)
    (home / ".omo").mkdir(exist_ok=True)
    (home / ".omo" / "omo.jsonc").write_text('{ "disabled_skills": ["hidden-skill"] }\n')
    server_script = home / "fake_mcp.py"
    server_script.write_text(FAKE_MCP_SERVER)
    (agent / "mcp.json").write_text(json.dumps({"mcpServers": {"smokefake": {
        "command": sys.executable, "args": [str(server_script)]}}}))
    model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), SkillMcpModel)
    threading.Thread(target=model.serve_forever, daemon=True).start()
    (agent / "models.json").write_text(models_json(model.server_port))
    try:
        run = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "use the smoke skill and the mcp tool"],
            cwd=work, env={**env, "MAHO_SKILLS_ROOT": str(bundled)}, stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=180,
        )
    finally:
        model.shutdown()
        (agent / "mcp.json").unlink(missing_ok=True)
        (home / ".omo" / "omo.jsonc").unlink(missing_ok=True)
    if run.returncode != 0 or SKILL_MARKER not in run.stdout:
        return f"skills/MCP run failed (exit {run.returncode})\nstdout: {run.stdout}\nstderr: {run.stderr}"
    first = SkillMcpModel.requests[0]
    offered = tool_names(first)
    if "skill" not in offered:
        return f"the skill tool is not offered (offered: {sorted(offered)})"
    if "mcp_smokefake_echo" not in offered:
        return f"the configured MCP server's tool is not offered (offered: {sorted(offered)})\nstderr: {run.stderr}"
    builtin = sorted(name for name in offered if name.startswith(("mcp_context7", "mcp_grep_app")))
    if builtin:
        return f"builtin remote MCP tools offered in offline mode: {builtin}"
    prompt = system_text(first)
    for name in ("smoke-skill", "bundled-skill"):
        if f"<name>{name}</name>" not in prompt:
            return f"skill {name} missing from the system prompt\nprompt: {prompt[-3000:]}"
    if "<name>hidden-skill</name>" in prompt:
        return "a disabled skill is still listed in the system prompt"
    results = "\n".join(tool_text(p) for p in SkillMcpModel.requests)
    if SKILL_BODY not in results or '<skill name="bundled-skill"' not in results:
        return f"the skill tool did not return the bundled skill\ntool results: {results[:3000]}"
    if MCP_ECHO not in results:
        return f"the MCP tool result did not reach the model\ntool results: {results[:3000]}\nstderr: {run.stderr}"
    print("ok   skills + MCP: user/bundled skills listed, disabled one hidden, skill tool loaded the bundled skill, "
          "stdio MCP tool mcp_smokefake_echo answered, no builtin remote MCPs offline")
    return None


def all_text(payload: dict) -> str:
    parts = []
    for message in payload.get("messages") or []:
        content = message.get("content")
        if isinstance(content, str):
            parts.append(content)
        elif isinstance(content, list):
            parts.extend(str(part.get("text", "")) for part in content if isinstance(part, dict))
    return "\n".join(parts)


class GatewayModel(http.server.BaseHTTPRequestHandler):
    """RPC receiver: answers a marker per delivered text. `-p` sender: session_list, session_send, done."""
    requests: list[dict] = []
    target = ""

    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = json.loads(self.rfile.read(length) or b"{}")
        GatewayModel.requests.append(payload)
        text = all_text(payload)
        results = sum(1 for message in payload.get("messages") or [] if message.get("role") == "tool")
        call = None
        if "relay to the rpc session" in text:
            if results == 0:
                call = ("session_list", {})
            elif results == 1:
                call = ("session_send", {"to": GatewayModel.target, "message": GATEWAY_AGENT_TEXT})
            else:
                answer = GATEWAY_RELAY_MARKER
        elif GATEWAY_AGENT_TEXT in text:
            answer = GATEWAY_AGENT_MARKER
        elif GATEWAY_CLI_TEXT in text:
            answer = GATEWAY_CLI_MARKER
        else:
            answer = "idle"
        if call:
            body = sse([
                chunk({"role": "assistant", "tool_calls": [{
                    "index": 0, "id": f"call_gw_{results}", "type": "function",
                    "function": {"name": call[0], "arguments": json.dumps(call[1])},
                }]}),
                chunk({}, "tool_calls"),
            ])
        else:
            body = sse([chunk({"role": "assistant", "content": answer}), chunk({}, "stop")])
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


def gateway_smoke(mhc: str, home: pathlib.Path, work: pathlib.Path, agent: pathlib.Path, env: dict) -> str | None:
    import time

    gw_env = {**env, "MAHO_GATEWAY_DIR": str(home / "gateway-store")}
    gw_env.pop("MAHO_GATEWAY", None)

    def gateway(*argv: str) -> subprocess.CompletedProcess:
        return subprocess.run([mhc, "gateway", *argv], cwd=work, env=gw_env, stdin=subprocess.DEVNULL,
                              capture_output=True, text=True, timeout=60)

    def received(marker: str) -> bool:
        return any(marker in json.dumps(p) for p in GatewayModel.requests) or marker in "".join(out_lines)

    model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), GatewayModel)
    threading.Thread(target=model.serve_forever, daemon=True).start()
    (agent / "models.json").write_text(models_json(model.server_port))
    out_lines: list[str] = []
    rpc = subprocess.Popen([mhc, "--mode", "rpc", "--offline", "--no-session", "--model", "offline/offline"],
                           cwd=work, env=gw_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, text=True)
    threading.Thread(target=lambda: out_lines.extend(iter(rpc.stdout.readline, "")), daemon=True).start()
    err_lines: list[str] = []
    threading.Thread(target=lambda: err_lines.extend(iter(rpc.stderr.readline, "")), daemon=True).start()
    try:
        records = []
        deadline = time.time() + 60
        while time.time() < deadline and not records:
            listed = gateway("list", "--json")
            records = json.loads(listed.stdout or "[]") if listed.returncode == 0 else []
            if not records:
                time.sleep(0.3)
        if len(records) != 1:
            return f"the RPC session did not register with the gateway (records: {records})\nstderr: {''.join(err_lines)[-3000:]}"
        session_id = records[0]["session_id"]
        if records[0].get("pid") != rpc.pid:
            return f"gateway record names pid {records[0].get('pid')}, the RPC session is {rpc.pid}"
        sent = gateway("send", "--from", "smoke", session_id[:8], GATEWAY_CLI_TEXT)
        if sent.returncode != 0 or session_id not in sent.stdout:
            return f"`mhc gateway send` failed (exit {sent.returncode})\nstdout: {sent.stdout}\nstderr: {sent.stderr}"
        deadline = time.time() + 60
        while time.time() < deadline and not received(GATEWAY_CLI_MARKER):
            time.sleep(0.3)
        cli_requests = [p for p in GatewayModel.requests if GATEWAY_CLI_TEXT in all_text(p)]
        if not cli_requests or not received(GATEWAY_CLI_MARKER):
            return f"the delivered message did not start a turn in the RPC session\nstdout: {''.join(out_lines)[-3000:]}\nstderr: {''.join(err_lines)[-3000:]}"
        delivered = all_text(cli_requests[0])
        if GATEWAY_HEADER + "smoke (command line)" not in delivered:
            return f"the delivered message lacks the gateway header\nmodel input: {delivered[-2000:]}"
        if list((home / "gateway-store" / "inbox" / session_id).glob("*.json")):
            return "the delivered message is still in the inbox"

        GatewayModel.target = session_id
        relay = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "relay to the rpc session"],
            cwd=work, env=gw_env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120)
        if relay.returncode != 0 or GATEWAY_RELAY_MARKER not in relay.stdout:
            return f"relay run failed (exit {relay.returncode})\nstdout: {relay.stdout}\nstderr: {relay.stderr}"
        relay_requests = [p for p in GatewayModel.requests if "relay to the rpc session" in all_text(p)]
        offered = tool_names(relay_requests[0])
        if not GATEWAY_TOOLS <= offered:
            return f"gateway tools not offered (offered: {sorted(offered)})"
        results = tool_text(relay_requests[-1])
        if session_id not in results or "Sent to session" not in results:
            return f"session_list/session_send results are wrong\ntool results: {results[:2000]}"
        deadline = time.time() + 60
        while time.time() < deadline and not received(GATEWAY_AGENT_MARKER):
            time.sleep(0.3)
        agent_requests = [p for p in GatewayModel.requests
                          if GATEWAY_AGENT_TEXT in all_text(p) and "relay to the rpc session" not in all_text(p)]
        if not agent_requests or GATEWAY_HEADER + "session " not in all_text(agent_requests[0]):
            return f"the relayed message did not reach the RPC session's model with the agent header\nstdout: {''.join(out_lines)[-3000:]}"
    finally:
        try:
            rpc.stdin.close()
        except OSError:
            pass
        try:
            rpc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            rpc.kill()
            rpc.wait()
        model.shutdown()
    after = gateway("list")
    if after.stdout.strip() != "No live sessions.":
        return f"the RPC session is still listed after it exited: {after.stdout}"
    off = subprocess.run(
        [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "relay to the rpc session"],
        cwd=work, env={**gw_env, "MAHO_GATEWAY": "0"}, stdin=subprocess.DEVNULL, capture_output=True,
        text=True, timeout=120)
    off_requests = [p for p in GatewayModel.requests[-3:] if "relay to the rpc session" in all_text(p)]
    if off_requests and GATEWAY_TOOLS & tool_names(off_requests[-1]):
        return f"gateway tools offered with MAHO_GATEWAY=0 (exit {off.returncode})"
    print("ok   gateway: RPC session registered, mhc gateway send delivered into it with the gateway header, "
          "a -p run relayed through session_list/session_send, unlisted after exit, MAHO_GATEWAY=0 hides the tools")
    return None


def main() -> int:
    if len(sys.argv) != 2:
        sys.stderr.write(__doc__ or "")
        return 2
    mhc = str(pathlib.Path(sys.argv[1]).resolve())
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory() as temp:
        home = pathlib.Path(temp)
        work = home / "work"
        agent = home / "agent"
        work.mkdir()
        agent.mkdir()
        (agent / "models.json").write_text(json.dumps({"providers": {"offline": {
            "api": "openai-completions", "baseUrl": f"http://127.0.0.1:{server.server_port}/v1",
            "apiKey": "offline-fixture",
            "models": [{"id": "offline", "reasoning": False, "input": ["text"],
                        "contextWindow": 128000, "maxTokens": 4096}],
        }}}))
        env = {**os.environ, "HOME": str(home), "USERPROFILE": str(home), "MAHO_CODING_AGENT_DIR": str(agent)}
        env.pop("__PI_INTERNAL_SPAWN", None)
        for name in OTEL_VARIABLES:
            env.pop(name, None)

        chat = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "write the smoke file"],
            cwd=work, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120,
        )
        server.shutdown()
        if chat.returncode != 0:
            return fail(f"chat exited {chat.returncode}\nstdout: {chat.stdout}\nstderr: {chat.stderr}")
        if MARKER not in chat.stdout:
            return fail(f"final answer missing from stdout\nstdout: {chat.stdout}\nstderr: {chat.stderr}")
        if not Model.requests:
            return fail("the model was never called")
        offered = tool_names(Model.requests[0])
        missing = REQUIRED_TOOLS - offered
        if missing:
            return fail(f"builtin tools not offered: {sorted(missing)} (offered: {sorted(offered)})")
        if not any(name == "bash" or name.startswith("bash_") for name in offered):
            return fail(f"no shell tool offered (offered: {sorted(offered)})")
        omo = sorted(name for name in offered
                     if name.startswith(OMO_TOOL_PREFIXES)
                     and name not in MIT_TASK_TOOLS | MIT_MEMORY_TOOLS | MIT_TEAM_TOOLS)
        browser_missing = MIT_BROWSER_TOOLS - offered
        if browser_missing:
            return fail(f"MIT browser tools not offered: {sorted(browser_missing)} (offered: {sorted(offered)})")
        if omo:
            return fail(f"omo tools offered in the MIT profile: {omo}")
        lsp_missing = MIT_LSP_TOOLS - offered
        if lsp_missing:
            return fail(f"MIT LSP tools not offered: {sorted(lsp_missing)} (offered: {sorted(offered)})")
        task_missing = MIT_TASK_TOOLS - offered
        if task_missing:
            return fail(f"MIT task tools not offered: {sorted(task_missing)} (offered: {sorted(offered)})")
        team_missing = MIT_TEAM_TOOLS - offered
        if team_missing:
            return fail(f"MIT team tools not offered: {sorted(team_missing)} (offered: {sorted(offered)})")
        git_missing = MIT_GIT_TOOLS - offered
        if git_missing:
            return fail(f"MIT git tools not offered: {sorted(git_missing)} (offered: {sorted(offered)})")
        memory_missing = MIT_MEMORY_TOOLS - offered
        if memory_missing:
            return fail(f"MIT memory tools not offered: {sorted(memory_missing)} (offered: {sorted(offered)})")
        if len(Model.requests) < 2:
            return fail("the write tool result never went back to the model")
        written = work / "smoke.txt"
        if not written.is_file() or written.read_text() != MARKER:
            return fail(f"builtin write tool did not write {written}\nstderr: {chat.stderr}")
        print(f"ok   chat + builtin write tool ({len(offered)} tools offered incl. MIT LSP, task, team, git, memory and browser, none from omo)")

        delegate = http.server.ThreadingHTTPServer(("127.0.0.1", 0), DelegateModel)
        threading.Thread(target=delegate.serve_forever, daemon=True).start()
        (agent / "models.json").write_text(models_json(delegate.server_port))
        run = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "delegate the smoke task"],
            cwd=work, env={**env, "PI_OFFLINE": "1"}, stdin=subprocess.DEVNULL, capture_output=True,
            text=True, timeout=180,
        )
        delegate.shutdown()
        if run.returncode != 0 or PARENT_MARKER not in run.stdout:
            return fail(f"delegation run failed (exit {run.returncode})\nstdout: {run.stdout}\nstderr: {run.stderr}")
        child_requests = [p for p in DelegateModel.requests if "task" not in tool_names(p)]
        if not child_requests:
            return fail(f"no child request reached the model\nstderr: {run.stderr}")
        result = "\n".join(tool_text(p) for p in DelegateModel.requests)
        if CHILD_MARKER not in result or 'status="completed"' not in result:
            return fail(f"the task result did not reach the parent\ntool results: {result}\nstderr: {run.stderr}")
        print("ok   task tool: child mhc process ran and its <task-result> reached the parent")

        repo = home / "repo"
        repo.mkdir()
        git = lambda *args: subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, env=env)
        git("init", "-q")
        (repo / "README.md").write_text("smoke\n")
        git("add", "README.md")
        git("-c", "user.name=smoke", "-c", "user.email=smoke@localhost", "-c", "commit.gpgsign=false",
            "commit", "-q", "-m", "init")
        isolation = http.server.ThreadingHTTPServer(("127.0.0.1", 0), IsolationModel)
        threading.Thread(target=isolation.serve_forever, daemon=True).start()
        (agent / "models.json").write_text(models_json(isolation.server_port))
        run = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "delegate an isolated task"],
            cwd=repo, env={**env, "PI_OFFLINE": "1"}, stdin=subprocess.DEVNULL, capture_output=True,
            text=True, timeout=180,
        )
        isolation.shutdown()
        if run.returncode != 0 or ISOLATION_MARKER not in run.stdout:
            return fail(f"isolated delegation failed (exit {run.returncode})\nstdout: {run.stdout}\nstderr: {run.stderr}")
        child_cwds = [p for p in IsolationModel.requests if "task" not in tool_names(p)]
        if not child_cwds:
            return fail(f"no isolated child request reached the model\nstderr: {run.stderr}")
        results = "\n".join(tool_text(p) for p in IsolationModel.requests)
        if "Isolation (worktree): applied 1 changed file" not in results or 'status="completed"' not in results:
            return fail(f"isolation note missing from the task result\ntool results: {results}\nstderr: {run.stderr}")
        applied = repo / ISOLATED_FILE
        if not applied.is_file() or applied.read_text() != ISOLATED_CONTENT:
            return fail(f"the isolated child's file was not applied to {repo}\ntool results: {results}")
        listed = subprocess.run(["git", "-C", str(repo), "worktree", "list", "--porcelain"], capture_output=True,
                                text=True, check=True).stdout
        if listed.count("worktree ") != 1 or any((agent / "wt").glob("t*")):
            return fail(f"the isolation worktree was not removed\n{listed}")
        if f'"untrackedFiles": [\n    "{ISOLATED_FILE}"' not in results:
            return fail(f"git_overview did not report the applied file\ntool results: {results}")
        print("ok   isolated task: child ran in a git worktree, its change was applied back and the worktree removed; git_overview saw it")

        failure = team_smoke(mhc, home, work, agent, env)
        if failure:
            return fail(failure)

        memory_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), MemoryModel)
        threading.Thread(target=memory_server.serve_forever, daemon=True).start()
        (agent / "models.json").write_text(models_json(memory_server.server_port))
        memory_env = {**env, "PI_OFFLINE": "1"}
        retain = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "remember the smoke fact"],
            cwd=work, env=memory_env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120,
        )
        if retain.returncode != 0 or MEMORY_MARKER not in retain.stdout:
            memory_server.shutdown()
            return fail(f"retain run failed (exit {retain.returncode})\nstdout: {retain.stdout}\nstderr: {retain.stderr}")
        if "1 memory stored." not in "\n".join(tool_text(p) for p in MemoryModel.requests):
            memory_server.shutdown()
            return fail(f"retain tool did not store the fact\nstderr: {retain.stderr}")
        if not (agent / "memories" / "mnemopi" / "banks").is_dir():
            memory_server.shutdown()
            return fail("retain did not create a project bank under <agent dir>/memories/mnemopi/banks")
        MemoryModel.phase = "recall"
        MemoryModel.requests = []
        recall = subprocess.run(
            [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "what is the smoke deployment cluster"],
            cwd=work, env=memory_env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120,
        )
        memory_server.shutdown()
        if recall.returncode != 0 or MEMORY_MARKER not in recall.stdout:
            return fail(f"recall run failed (exit {recall.returncode})\nstdout: {recall.stdout}\nstderr: {recall.stderr}")
        first = json.dumps(MemoryModel.requests[0]) if MemoryModel.requests else ""
        if "<memories>" not in first or "cluster-zeta-42" not in first:
            return fail(f"the retained fact was not auto-recalled into the first turn\nrequest: {first[:4000]}")
        if "cluster-zeta-42" not in "\n".join(tool_text(p) for p in MemoryModel.requests):
            return fail(f"the recall tool did not return the retained fact\nstderr: {recall.stderr}")
        print("ok   memory: retain stored a fact; the next run auto-recalled it and the recall tool found it")

        legacy = home / "legacy-memory"
        (legacy / "notes" / "facts").mkdir(parents=True)
        (legacy / "notes" / "facts" / "2026-09.md").write_text("- Smoke builds are linked with zig cc\n")
        (legacy / "people" / "kim").mkdir(parents=True)
        (legacy / "people" / "kim" / "card.md").write_text("Kim owns billing.\n")
        imported = subprocess.run([mhc, "memory", "import-legacy", "--from", str(legacy), "--json"], cwd=work,
                                  env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        try:
            report = json.loads(imported.stdout)
        except json.JSONDecodeError:
            report = {}
        if imported.returncode != 0 or report.get("blocks_imported") != 1 or report.get("skipped_people_files") != 1:
            return fail(f"`mhc memory import-legacy` failed (exit {imported.returncode})\nstdout: {imported.stdout}\nstderr: {imported.stderr}")
        search = subprocess.run([mhc, "memory", "search", "zig", "builds"], cwd=work, env=env,
                                stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        if search.returncode != 0 or "linked with zig cc" not in search.stdout:
            return fail(f"`mhc memory search` did not find the imported fact\nstdout: {search.stdout}\nstderr: {search.stderr}")
        print("ok   mhc memory import-legacy + search (people files skipped by default)")

        try:
            responses = mcp_session(mhc, "--memory-mcp", work, env, [
                {"name": "mnemopi_remember", "arguments": {"content": "MCP smoke memory about tangerines"}},
                {"name": "mnemopi_recall", "arguments": {"query": "tangerines"}},
            ])
            legacy_flag = mcp_session(mhc, "--omo-memory-mcp", work, env, [])
        except RuntimeError as error:
            return fail(str(error))
        by_id = {response.get("id"): response for response in responses}
        tools = {tool["name"] for tool in by_id.get(2, {}).get("result", {}).get("tools", [])}
        recalled = json.dumps(by_id.get(4, {}))
        if by_id.get(1, {}).get("result", {}).get("serverInfo", {}).get("name") != "mnemopi" \
                or not {"mnemopi_remember", "mnemopi_recall"} <= tools or "tangerines" not in recalled \
                or not legacy_flag or "result" not in legacy_flag[0]:
            return fail(f"memory MCP server answered unexpectedly: {responses} / {legacy_flag}")
        print(f"ok   memory MCP server (--memory-mcp, --omo-memory-mcp): {len(tools)} tools, remember + recall")


        telemetry_model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), TelemetryModel)
        threading.Thread(target=telemetry_model.serve_forever, daemon=True).start()
        collector = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Collector)
        threading.Thread(target=collector.serve_forever, daemon=True).start()
        (agent / "models.json").write_text(models_json(telemetry_model.server_port))
        ledger = agent / "stats" / "usage.jsonl"
        otel_env = {**env, "PI_OFFLINE": "1", "OTEL_EXPORTER_OTLP_ENDPOINT": f"http://127.0.0.1:{collector.server_port}"}

        def telemetry_run(extra: dict) -> subprocess.CompletedProcess:
            return subprocess.run(
                [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", TELEMETRY_PROMPT],
                cwd=work, env={**otel_env, **extra}, stdin=subprocess.DEVNULL, capture_output=True,
                text=True, timeout=120,
            )

        before = ledger_lines(ledger)
        exported = telemetry_run({})
        if exported.returncode != 0 or TELEMETRY_MARKER not in exported.stdout:
            return fail(f"telemetry run failed (exit {exported.returncode})\nstdout: {exported.stdout}\nstderr: {exported.stderr}")
        paths = sorted({path for path, _, _ in Collector.received})
        if paths != ["/v1/logs", "/v1/metrics", "/v1/traces"]:
            return fail(f"OTLP export did not reach the collector on all signals: {paths}\nstderr: {exported.stderr}")
        if any(ctype != "application/x-protobuf" for _, ctype, _ in Collector.received):
            return fail(f"unexpected OTLP content types: {[c for _, c, _ in Collector.received]}")
        traces = b"".join(body for path, _, body in Collector.received if path == "/v1/traces")
        metrics = b"".join(body for path, _, body in Collector.received if path == "/v1/metrics")
        if b"invoke_agent mhc" not in traces or b"chat offline" not in traces or b"gen_ai.client.token.usage" not in metrics:
            return fail(f"OTLP payloads lack the GenAI spans/metrics: traces={traces[:400]!r}")
        if any(TELEMETRY_PROMPT.encode() in body or TELEMETRY_MARKER.encode() in body for _, _, body in Collector.received):
            return fail("prompt or response text was exported")
        if ledger_lines(ledger) <= before:
            return fail(f"the usage ledger was not written ({ledger})\nstderr: {exported.stderr}")
        last = json.loads(ledger.read_text().splitlines()[-1])
        if last.get("model") != "offline" or last.get("input") != 1200 or TELEMETRY_PROMPT in ledger.read_text():
            return fail(f"unexpected ledger record: {last}")
        print(f"ok   telemetry: ledger record written and OTLP traces/metrics/logs exported ({len(Collector.received)} requests, no content)")

        Collector.received = []
        before = ledger_lines(ledger)
        dnt = telemetry_run({"DO_NOT_TRACK": "1"})
        if dnt.returncode != 0 or TELEMETRY_MARKER not in dnt.stdout:
            return fail(f"DO_NOT_TRACK run failed (exit {dnt.returncode})\nstderr: {dnt.stderr}")
        if Collector.received:
            return fail(f"DO_NOT_TRACK=1 still exported: {[p for p, _, _ in Collector.received]}")
        if ledger_lines(ledger) <= before:
            return fail("DO_NOT_TRACK=1 should keep the local ledger")
        before = ledger_lines(ledger)
        off = telemetry_run({"MAHO_TELEMETRY": "0"})
        telemetry_model.shutdown()
        collector.shutdown()
        if off.returncode != 0 or TELEMETRY_MARKER not in off.stdout:
            return fail(f"MAHO_TELEMETRY=0 run failed (exit {off.returncode})\nstderr: {off.stderr}")
        if Collector.received or ledger_lines(ledger) != before:
            return fail(f"MAHO_TELEMETRY=0 still recorded (exported {len(Collector.received)}, ledger {before} -> {ledger_lines(ledger)})")
        print("ok   telemetry opt-outs: DO_NOT_TRACK=1 stops export (ledger kept); MAHO_TELEMETRY=0 stops both")

        usage = subprocess.run([mhc, "usage", "--json"], cwd=work, env=env, stdin=subprocess.DEVNULL,
                               capture_output=True, text=True, timeout=60)
        try:
            report = json.loads(usage.stdout)
        except json.JSONDecodeError:
            report = {}
        models = {m.get("model") for m in report.get("byModel", [])}
        if usage.returncode != 0 or report.get("records", 0) < 2 or "offline" not in models \
                or report.get("overall", {}).get("totalInputTokens", 0) < 2400:
            return fail(f"`mhc usage --json` is incomplete (exit {usage.returncode})\nstdout: {usage.stdout[:2000]}\nstderr: {usage.stderr}")
        summary = subprocess.run([mhc, "usage"], cwd=work, env=env, stdin=subprocess.DEVNULL,
                                 capture_output=True, text=True, timeout=60)
        status = subprocess.run([mhc, "usage", "--status"], cwd=work, env={**env, "DO_NOT_TRACK": "1"},
                                stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        if "=== AI Usage Statistics ===" not in summary.stdout or "OTLP export: off" not in status.stdout \
                or "DO_NOT_TRACK is set" not in status.stdout:
            return fail(f"`mhc usage` / `--status` output unexpected\nsummary: {summary.stdout}\nstatus: {status.stdout}\nstderr: {summary.stderr}{status.stderr}")
        print(f"ok   mhc usage: {report['records']} ledger records, --json/summary/--status")

        browser_exe = os.environ.get("MHC_SMOKE_BROWSER")
        if browser_exe:
            browser_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), BrowserModel)
            threading.Thread(target=browser_server.serve_forever, daemon=True).start()
            (agent / "models.json").write_text(models_json(browser_server.server_port))
            run = subprocess.run(
                [mhc, "-p", "--offline", "--no-session", "--model", "offline/offline", "look at the smoke page"],
                cwd=work, env={**env, "MAHO_BROWSER_PATH": browser_exe}, stdin=subprocess.DEVNULL,
                capture_output=True, text=True, timeout=180,
            )
            browser_server.shutdown()
            if run.returncode != 0 or BROWSER_MARKER not in run.stdout:
                return fail(f"browser run failed (exit {run.returncode})\nstdout: {run.stdout}\nstderr: {run.stderr}")
            snapshot = "\n".join(tool_text(p) for p in BrowserModel.requests)
            if f'heading "{BROWSER_HEADING}"' not in snapshot or "[ref=e" not in snapshot:
                return fail(f"the browser snapshot did not reach the model\ntool results: {snapshot[:4000]}\nstderr: {run.stderr}")
            # The fixture model is text-only, so the image block itself may be withheld from it;
            # the screenshot's text line must arrive either way.
            if "Screenshot " not in snapshot or "(browser)" not in snapshot:
                return fail(f"the computer screenshot result did not reach the model\ntool results: {snapshot[:4000]}")
            print("ok   browser tool: opened a page, its snapshot with refs and a computer screenshot reached the model")
        else:
            print("skip browser run (set MHC_SMOKE_BROWSER to a Chromium executable)")

        failure = skills_mcp_smoke(mhc, home, work, agent, env)
        if failure:
            return fail(failure)

        failure = gateway_smoke(mhc, home, work, agent, env)
        if failure:
            return fail(failure)

        (home / ".omo").mkdir(exist_ok=True)
        (home / ".omo" / "omo.jsonc").write_text('{ "model_profile": "offline/offline" }\n')
        doctor = subprocess.run([mhc, "doctor"], cwd=work, env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=60)
        expected = ("~/.omo/omo.jsonc  (loaded)", "PASS configuration has no problems",
                    "PASS offline/offline resolves to offline/offline")
        missing_lines = [line for line in expected if line not in doctor.stdout]
        if doctor.returncode != 0 or missing_lines:
            return fail(f"`mhc doctor` report is incomplete (missing {missing_lines})\n"
                        f"exit {doctor.returncode}\nstdout: {doctor.stdout}\nstderr: {doctor.stderr}")
        print("ok   mhc doctor: config sources and model profile resolution")

        for argv in (["ulw-loop", "--help"], ["--memory-supervisor"]):
            result = subprocess.run([mhc, *argv], cwd=work, env=env, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=60)
            if result.returncode == 0 or UNAVAILABLE not in result.stderr:
                return fail(f"`mhc {' '.join(argv)}` should report it is unavailable\n"
                            f"exit {result.returncode}\nstderr: {result.stderr}")
            print(f"ok   mhc {' '.join(argv)}: unavailable in the MIT profile")
    print("mit-profile-smoke: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
