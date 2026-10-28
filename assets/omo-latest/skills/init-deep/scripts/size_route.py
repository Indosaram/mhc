#!/usr/bin/env python3
"""init-deep Phase 1: measure source size, compute node counts, route, plan chunks.

Usage:
  size_route.py [--max-depth=N] [--max-nodes=N] [--write-plan]   # measure + route
  size_route.py --owner <path>                                     # which writer owns a path

All measurement and arithmetic happens here so the main session never does it
by hand. Prints a short summary; with --write-plan (DAG path only) it also
writes .mhc/init-deep/plan.json and creates the reports/ and digests/ dirs.
"""
import json
import math
import os
import subprocess
import sys

STATE_DIR = ".mhc"
WORK_DIR = os.path.join(STATE_DIR, "init-deep")
CHUNK = 400 * 1024          # ~100k tokens of source per scanner
REPORTS_PER_WRITER = 12     # one writer absorbs ~12 reports (~60k tokens)
INLINE_BELOW = 4            # N_quick < 4 -> inline path
DEFAULT_MAX_NODES = 64      # task.dag.max_nodes_per_run default

EXCLUDED_DIRS = {"node_modules", ".git", "dist", "build", "out", "vendor", "target", "coverage"}
LOCKFILES = {
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock", "Cargo.lock",
    "poetry.lock", "Pipfile.lock", "uv.lock", "composer.lock", "Gemfile.lock", "go.sum",
    "mix.lock", "pubspec.lock", "Podfile.lock", "flake.lock", "packages.lock.json",
}
DOC_NAMES = {"AGENTS.md", "CLAUDE.md"}


def arg(name, default=None):
    for a in sys.argv[1:]:
        if a.startswith(f"--{name}="):
            return a.split("=", 1)[1]
    return default


def list_files():
    """Tracked files (NUL-separated) when in git; otherwise a filesystem walk."""
    try:
        raw = subprocess.run(["git", "ls-files", "-z"], capture_output=True, check=True).stdout
        return [p for p in raw.decode("utf-8", "surrogateescape").split("\0") if p]
    except (subprocess.CalledProcessError, FileNotFoundError):
        out = []
        for root, dirs, files in os.walk("."):
            dirs[:] = [d for d in dirs if d not in EXCLUDED_DIRS]
            for f in files:
                out.append(os.path.relpath(os.path.join(root, f), "."))
        return out


def excluded(path):
    parts = path.split("/")
    if any(p in EXCLUDED_DIRS for p in parts[:-1]):
        return True
    name = parts[-1]
    if name in LOCKFILES or name.endswith(".lock"):
        return True
    if ".min." in name:
        return True
    return False


def is_binary(path):
    try:
        with open(path, "rb") as fh:
            return b"\0" in fh.read(8192)
    except OSError:
        return True


def fold(dirpath, max_depth):
    """Directories deeper than max_depth are folded into their ancestor at max_depth."""
    if dirpath == ".":
        return "."
    parts = dirpath.split("/")
    return "/".join(parts[:max_depth]) if len(parts) > max_depth else dirpath


def measure(max_depth):
    own = {}                       # dir -> bytes of files directly in it (after folding)
    docs = []
    deepest = 0
    for path in list_files():
        name = path.rsplit("/", 1)[-1]
        if name in DOC_NAMES and not excluded(path):
            docs.append(path)
            continue
        if excluded(path) or not os.path.isfile(path) or is_binary(path):
            continue
        d = os.path.dirname(path) or "."
        deepest = max(deepest, 0 if d == "." else d.count("/") + 1)
        d = fold(d, max_depth)
        own[d] = own.get(d, 0) + os.path.getsize(path)
    # make every ancestor exist
    for d in list(own):
        while d != ".":
            d = os.path.dirname(d) or "."
            own.setdefault(d, 0)
    own.setdefault(".", 0)
    children = {d: [] for d in own}
    for d in own:
        if d != ".":
            children[os.path.dirname(d) or "."].append(d)
    subtree = {}

    def total(d):
        if d not in subtree:
            subtree[d] = own[d] + sum(total(c) for c in children[d])
        return subtree[d]

    total(".")
    return own, children, subtree, sorted(docs), min(deepest, max_depth)


def split(d, cap, own, children, subtree, keep_empty_own=False):
    """Whole directories up to cap; a bigger directory is split along its children.
    Returns units: (dir, mode, bytes) with mode 'subtree' or 'own' (files directly in dir)."""
    if subtree[d] <= cap or not children[d]:
        return [(d, "subtree", subtree[d])]
    # keep_empty_own: a split directory still needs exactly one owner for its own AGENTS.md
    units = [(d, "own", own[d])] if own[d] > 0 or keep_empty_own else []
    for c in sorted(children[d]):
        if subtree[c] > 0:
            units += split(c, cap, own, children, subtree, keep_empty_own)
    return units


def lpt(units, k):
    """Largest-first onto the least-loaded of exactly k bins."""
    bins = [{"units": [], "bytes": 0} for _ in range(k)]
    for u in sorted(units, key=lambda u: -u[2]):
        b = min(bins, key=lambda b: b["bytes"])
        b["units"].append(u)
        b["bytes"] += u[2]
    return bins


def slug(path):
    return "root" if path == "." else path.replace("/", "-").replace(".", "_")


def plan(own, children, subtree, n_quick, n_high):
    s = subtree["."]
    # writer ownership: top-level subtrees, split if one is too big for a single writer
    wunits = []
    for c in sorted(children["."]):
        if subtree[c] > 0:
            wunits += split(c, REPORTS_PER_WRITER * CHUNK, own, children, subtree, keep_empty_own=True)
    if not wunits:
        wunits = [(".", "own", own["."])]
    wbins = [b for b in lpt(wunits, n_high) if b["units"]]
    # distribute scanner chunks over writers in proportion to bytes (>=1 each, sum == n_quick)
    tot = sum(b["bytes"] for b in wbins) or 1
    alloc = [max(1, round(n_quick * b["bytes"] / tot)) for b in wbins]
    while sum(alloc) > n_quick:
        i = max(range(len(alloc)), key=lambda i: alloc[i])
        alloc[i] -= 1
    while sum(alloc) < n_quick:
        i = max(range(len(alloc)), key=lambda i: wbins[i]["bytes"] / alloc[i])
        alloc[i] += 1
    writers, chunks = [], []
    for wi, (wb, k) in enumerate(zip(wbins, alloc)):
        roots = [{"path": u[0], "mode": u[1]} for u in wb["units"]]
        wid = f"writer-{wi + 1:02d}"
        writers.append({"id": wid, "slug": "-".join(slug(r["path"]) for r in roots)[:80] or wid,
                        "roots": roots, "bytes": wb["bytes"], "chunks": []})
        sunits = []
        for u in wb["units"]:
            if u[1] == "own":
                sunits += [u] if u[2] > 0 else []
            else:
                sunits += split(u[0], CHUNK, own, children, subtree)
        k = max(1, min(k, len(sunits))) if sunits else 0
        for cb in lpt(sunits, k):
            if not cb["units"]:
                continue
            cid = f"chunk-{len(chunks) + 1:03d}"
            chunks.append({"id": cid, "writer": wid, "bytes": cb["bytes"],
                           "dirs": [{"path": u[0], "mode": u[1]} for u in cb["units"]]})
            writers[-1]["chunks"].append(cid)
    # files sitting directly at repo root: scanned by the smallest chunk, documented by the root writer
    if own["."] > 0 and chunks:
        c = min(chunks, key=lambda c: c["bytes"])
        c["dirs"].append({"path": ".", "mode": "own"})
        c["bytes"] += own["."]
    return writers, chunks


def owner(path):
    with open(os.path.join(WORK_DIR, "plan.json")) as fh:
        p = json.load(fh)
    path = path.removesuffix("/AGENTS.md").removesuffix("AGENTS.md").strip("/") or "."
    if path == ".":
        return "root-writer"
    best, best_len = "root-writer", -1
    for w in p["writers"]:
        for r in w["roots"]:
            rp = r["path"]
            hit = path == rp if r["mode"] == "own" else (path == rp or path.startswith(rp + "/"))
            if hit and len(rp) > best_len:
                best, best_len = w["id"], len(rp)
    return best


def main():
    if "--owner" in sys.argv:
        print(owner(sys.argv[sys.argv.index("--owner") + 1]))
        return
    max_depth = int(arg("max-depth", "3"))
    max_nodes = int(arg("max-nodes", str(DEFAULT_MAX_NODES)))
    own, children, subtree, docs, depth = measure(max_depth)
    s = subtree["."]
    n_quick = math.ceil(s / CHUNK) if s else 0
    n_high = math.ceil(n_quick / REPORTS_PER_WRITER) if n_quick else 0
    if n_quick < INLINE_BELOW:
        route = "inline"
    elif n_quick > max_nodes:
        route = "multi-run"
    else:
        route = "dag"
    summary = {
        "S_bytes": s, "S_MB": round(s / 1048576, 2), "N_quick": n_quick, "N_high": n_high,
        "total_nodes": (n_quick + n_high + 2) if route != "inline" else 0,
        "max_depth": depth, "route": route, "existing_docs": docs,
        "top_dirs": sorted(((d, subtree[d]) for d in children["."]), key=lambda x: -x[1])[:15],
    }
    if route != "inline" and "--write-plan" in sys.argv:
        writers, chunks = plan(own, children, subtree, n_quick, n_high)
        os.makedirs(os.path.join(WORK_DIR, "reports"), exist_ok=True)
        os.makedirs(os.path.join(WORK_DIR, "digests"), exist_ok=True)
        with open(os.path.join(WORK_DIR, "plan.json"), "w") as fh:
            json.dump({**summary, "chunk_bytes": CHUNK, "writers": writers, "chunks": chunks}, fh, indent=1)
        summary["plan"] = os.path.join(WORK_DIR, "plan.json")
        summary["writers"] = [(w["id"], len(w["chunks"]), [r["path"] for r in w["roots"]][:6]) for w in writers]
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
