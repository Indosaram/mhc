#!/usr/bin/env python3
"""init-deep Phase 5: write .mhc/init-deep.json.

Usage: snapshot.py [local|committed]
With no argument the mode falls back to: committed if root AGENTS.md is tracked by git, else local.
An explicit argument always wins.
"""
import json
import os
import subprocess
import sys
import time

STATE_DIR = ".mhc"
SNAPSHOT = os.path.join(STATE_DIR, "init-deep.json")
LOC_EXT = {"ts", "tsx", "js", "jsx", "py", "go", "rs", "java", "kt", "swift", "rb", "php",
           "c", "cpp", "cs", "scala", "lua", "ex", "exs", "zig", "dart"}


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, check=True).stdout


def tracked():
    # NUL-separated: names containing newlines and pipe chunk boundaries cannot skew the count
    raw = git("ls-files", "-z")
    return [p for p in raw.split(b"\0") if p]


def count_loc(files):
    total = 0
    try:
        for f in files:
            name = f.decode("utf-8", "surrogateescape")
            ext = name.rsplit(".", 1)[-1] if "." in name.rsplit("/", 1)[-1] else ""
            if ext in LOC_EXT and os.path.isfile(name):
                with open(name, "rb") as fh:
                    total += fh.read().count(b"\n")
    except Exception:
        return 0
    return total


def main():
    explicit = sys.argv[1] if len(sys.argv) > 1 else None
    if explicit not in (None, "local", "committed"):
        sys.exit("mode must be local or committed")
    if explicit:
        mode = explicit
    else:
        is_tracked = subprocess.run(["git", "ls-files", "--error-unmatch", "AGENTS.md"],
                                    capture_output=True).returncode == 0
        mode = "committed" if is_tracked else "local"
    files = tracked()
    snap = {
        "commitSha": git("rev-parse", "HEAD").decode().strip(),
        "fileCount": len(files),
        "loc": count_loc(files),
        "timestamp": int(time.time() * 1000),
        "mode": mode,
    }
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(SNAPSHOT, "w") as fh:
        json.dump(snap, fh)
        fh.write("\n")
    print(json.dumps(snap))


if __name__ == "__main__":
    main()
