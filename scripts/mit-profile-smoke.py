#!/usr/bin/env python3
"""Smoke-test an MIT-profile `mhc` binary (maho-cli built with `--no-default-features`).

Runs `mhc -p` against a loopback OpenAI-compatible model that first calls the builtin `write` tool
and then answers in text, and checks that:

- basic chat works (the final text reaches stdout, exit 0);
- builtin tools are offered (read, write, edit, and the shell: `bash` or the terminal's `bash_*`
  tools) and run (the file is written);
- no omo tool is offered (task, team_*, memory, ...); the MIT LSP tools (`maho-lsp`) are allowed;
- `mhc doctor` reports the configuration sources and resolves a pinned model profile;
- the omo-only entries report that they are unavailable (`mhc ulw-loop`).

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
MIT_LSP_TOOLS = {"lsp_diagnostics", "lsp_goto_definition", "lsp_find_references", "lsp_symbols", "lsp_prepare_rename", "lsp_rename"}
UNAVAILABLE = "is not available in this build"


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
        omo = sorted(name for name in offered if name.startswith(OMO_TOOL_PREFIXES))
        if omo:
            return fail(f"omo tools offered in the MIT profile: {omo}")
        lsp_missing = MIT_LSP_TOOLS - offered
        if lsp_missing:
            return fail(f"MIT LSP tools not offered: {sorted(lsp_missing)} (offered: {sorted(offered)})")
        if len(Model.requests) < 2:
            return fail("the write tool result never went back to the model")
        written = work / "smoke.txt"
        if not written.is_file() or written.read_text() != MARKER:
            return fail(f"builtin write tool did not write {written}\nstderr: {chat.stderr}")
        print(f"ok   chat + builtin write tool ({len(offered)} tools offered incl. MIT LSP, none from omo)")

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

        for argv in (["ulw-loop", "--help"],):
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
