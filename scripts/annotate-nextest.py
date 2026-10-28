#!/usr/bin/env python3
"""Run a command (cargo nextest ...), tee its output, and on failure report the failing tests as one
GitHub workflow annotation so they are readable without the job log.

Usage: python3 scripts/annotate-nextest.py <title> -- <command...>
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

FAIL = re.compile(r"^\s*(?:FAIL|SIGSEGV|SIGABRT|TIMEOUT|LEAK-FAIL)\s+\[[^\]]*\]\s+(?:\(\d+/\d+\)\s+)?(.+?)\s*$")
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
PANIC = re.compile(r"panicked at |^error(\[|:)|assertion .*failed|^\s+left: |^\s+right: ")


def escape(text: str) -> str:
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def main() -> int:
    if "--" not in sys.argv or sys.argv.index("--") < 2:
        sys.stderr.write(__doc__ or "")
        return 2
    title = sys.argv[1]
    command = sys.argv[sys.argv.index("--") + 1:]
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace")
    lines: list[str] = []
    assert process.stdout is not None
    for line in process.stdout:
        sys.stdout.write(line)
        lines.append(ANSI.sub("", line.rstrip("\n")))
    code = process.wait()
    if code == 0:
        return 0
    failed = sorted({match.group(1) for line in lines if (match := FAIL.match(line))})
    details = [line.strip() for line in lines if PANIC.search(line)][:60]
    summary = [f"exit {code}; {len(failed)} failing test(s):", *failed[:80]]
    if details:
        summary += ["", "first panic/error lines:", *details]
    if not failed and not details:
        summary += ["", "last output lines:", *lines[-40:]]
    message = "\n".join(summary)[-8000:]
    if os.environ.get("GITHUB_ACTIONS"):
        print(f"::error title={title}::{escape(message)}")
    else:
        print(message)
    return code


if __name__ == "__main__":
    sys.exit(main())
