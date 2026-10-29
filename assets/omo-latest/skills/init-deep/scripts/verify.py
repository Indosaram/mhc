#!/usr/bin/env python3
"""init-deep verifier. Prints exactly one line per AGENTS.md: `PASS <path>` or `FAIL <path>: <reason>`.

Usage:
  verify.py            # DAG path: files declared in .mhc/init-deep/digests/*.md, plus root
  verify.py --all      # inline path: every AGENTS.md in the repo
Also writes the same lines to .mhc/init-deep/verify.txt when that directory exists.
Exit code 0 when every line is PASS, 1 otherwise.
"""
import os
import re
import sys

WORK_DIR = os.path.join(".mhc", "init-deep")
SKIP = {"node_modules", ".git", "dist", "build"}
ROOT_LIMITS = (50, 150)
CHILD_LIMITS = (30, 80)
DIGEST_LINE = re.compile(r"^\s*-\s*`?(?P<path>[^`|\s]*AGENTS\.md)`?\s*\|")
JUSTIFY = re.compile(r"(score|distinct domain|why this file|earned)", re.I)


def norm(p):
    p = os.path.normpath(p)
    return p if p.endswith("AGENTS.md") else os.path.join(p, "AGENTS.md")


def declared():
    paths = {"AGENTS.md"}
    ddir = os.path.join(WORK_DIR, "digests")
    if os.path.isdir(ddir):
        for name in sorted(os.listdir(ddir)):
            with open(os.path.join(ddir, name), encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    m = DIGEST_LINE.match(line)
                    if m:
                        paths.add(norm(m.group("path")))
    return paths


def discovered():
    out = set()
    for root, dirs, files in os.walk("."):
        dirs[:] = [d for d in dirs if d not in SKIP]
        if "AGENTS.md" in files:
            out.add(os.path.normpath(os.path.join(root, "AGENTS.md")))
    return out


def blocks(text, size=3):
    """Runs of `size` consecutive non-trivial lines; used to detect copied parent content."""
    lines = [l.strip() for l in text.splitlines()]
    lines = [l for l in lines if len(l) >= 12 and not l.startswith("#") and set(l) - set("|-: `")]
    return {"\n".join(lines[i:i + size]) for i in range(len(lines) - size + 1)}


def ancestors(path):
    d = os.path.dirname(path)
    while d not in ("", "."):
        d = os.path.dirname(d)
        yield os.path.normpath(os.path.join(d or ".", "AGENTS.md"))


def check(path):
    if not os.path.isfile(path):
        return "missing on disk"
    text = open(path, encoding="utf-8", errors="replace").read()
    n = len(text.splitlines())
    is_root = path == "AGENTS.md"
    lo, hi = ROOT_LIMITS if is_root else CHILD_LIMITS
    if not lo <= n <= hi:
        return f"{n} lines, limit {lo}-{hi}"
    if is_root:
        return None
    if not JUSTIFY.search(text):
        return "does not state why this directory has its own file (score or distinct domain)"
    mine = blocks(text)
    for anc in ancestors(path):
        if os.path.isfile(anc):
            dup = mine & blocks(open(anc, encoding="utf-8", errors="replace").read())
            if dup:
                first = next(iter(dup)).splitlines()[0][:60]
                return f"repeats a block from {anc} (starts: {first!r})"
    return None


def main():
    paths = discovered() | {"AGENTS.md"} if "--all" in sys.argv else declared()
    lines = []
    for p in sorted(paths, key=lambda p: (p != "AGENTS.md", p)):
        reason = check(p)
        lines.append(f"PASS {p}" if reason is None else f"FAIL {p}: {reason}")
    report = "\n".join(lines)
    print(report)
    if os.path.isdir(WORK_DIR):
        with open(os.path.join(WORK_DIR, "verify.txt"), "w") as fh:
            fh.write(report + "\n")
    sys.exit(0 if all(l.startswith("PASS") for l in lines) else 1)


if __name__ == "__main__":
    main()
