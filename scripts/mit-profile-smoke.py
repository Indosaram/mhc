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
                     if name.startswith(OMO_TOOL_PREFIXES) and name not in MIT_TASK_TOOLS | MIT_MEMORY_TOOLS)
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
        memory_missing = MIT_MEMORY_TOOLS - offered
        if memory_missing:
            return fail(f"MIT memory tools not offered: {sorted(memory_missing)} (offered: {sorted(offered)})")
        if len(Model.requests) < 2:
            return fail("the write tool result never went back to the model")
        written = work / "smoke.txt"
        if not written.is_file() or written.read_text() != MARKER:
            return fail(f"builtin write tool did not write {written}\nstderr: {chat.stderr}")
        print(f"ok   chat + builtin write tool ({len(offered)} tools offered incl. MIT LSP, task, memory and browser, none from omo)")

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
