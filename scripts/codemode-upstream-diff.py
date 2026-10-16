#!/usr/bin/env python3
"""Report upstream senpi drift for the codemode runtime assets mhc embeds.

usage: scripts/codemode-upstream-diff.py <senpi checkout> [--ref origin/main] [--diff]

Reads crates/maho-codemode/assets/UPSTREAM.json and, for every verbatim/patched asset, compares
the file at its pinned upstreamCommit with the same file at --ref. Also lists runtime files
(non-.ts) that exist upstream but are missing from the manifest. Exits 1 when anything drifted,
so a port can be scheduled; it never edits files.
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "crates/maho-codemode/assets/UPSTREAM.json"


def git(checkout, *args, check=True):
    result = subprocess.run(["git", "-C", str(checkout), *args], capture_output=True, text=True)
    if check and result.returncode != 0:
        sys.exit(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result


def blob(checkout, rev, path):
    result = git(checkout, "rev-parse", f"{rev}:{path}", check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("senpi", type=Path, help="path to a senpi git checkout")
    parser.add_argument("--ref", default="origin/main", help="upstream revision to compare against (default: origin/main)")
    parser.add_argument("--diff", action="store_true", help="print the upstream diff for each drifted asset")
    args = parser.parse_args()

    manifest = json.loads(MANIFEST.read_text())
    prefix = manifest["upstream"]["packagePath"]
    git(args.senpi, "rev-parse", "--verify", f"{args.ref}^{{commit}}")

    drifted = []
    for entry in manifest["assets"]:
        if entry["status"] not in ("verbatim", "patched"):
            continue
        path = f"{prefix}/{entry['path']}"
        pinned, current = blob(args.senpi, entry["upstreamCommit"], path), blob(args.senpi, args.ref, path)
        if pinned is None:
            sys.exit(f"{entry['path']}: pinned commit {entry['upstreamCommit']} not found in {args.senpi}; fetch upstream first")
        if pinned == current:
            continue
        state = "removed upstream" if current is None else "changed upstream"
        drifted.append(entry["path"])
        print(f"{state}: {entry['path']} ({entry['status']}, pinned {entry['upstreamCommit'][:12]})")
        if args.diff and current is not None:
            print(git(args.senpi, "diff", entry["upstreamCommit"], args.ref, "--", path).stdout)

    known = {entry["path"] for entry in manifest["assets"]}
    listed = git(args.senpi, "ls-tree", "-r", "--name-only", args.ref, f"{prefix}/kernels", f"{prefix}/skill").stdout.split()
    new = sorted(
        relative for relative in (name[len(prefix) + 1:] for name in listed)
        if not relative.endswith(".ts") and not relative.endswith("AGENTS.md") and relative not in known
    )
    for relative in new:
        print(f"new upstream runtime file: {relative}")

    if drifted or new:
        print(f"\n{len(drifted)} drifted, {len(new)} new against {args.ref}")
        return 1
    print(f"all {sum(entry['status'] in ('verbatim', 'patched') for entry in manifest['assets'])} pinned assets match {args.ref}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
