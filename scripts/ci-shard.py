#!/usr/bin/env python3
"""Print the `-p <crate>` arguments for one CI test shard.

Usage: scripts/ci-shard.py <shards> <index>   (index is 1-based)

Every workspace member lands in exactly one shard, so a crate added later is never skipped.
Members are weighted by how many of their .rs files contain a test attribute and assigned
greedily (heaviest first, to the lightest shard) so the shards finish at similar times.
"""
import json
import pathlib
import re
import subprocess
import sys

TEST_ATTR = re.compile(r"#\[(tokio::)?test\b")


def weight(manifest: pathlib.Path) -> int:
    count = 0
    for path in manifest.parent.rglob("*.rs"):
        if "target" in path.parts:
            continue
        try:
            if TEST_ATTR.search(path.read_text(errors="ignore")):
                count += 1
        except OSError:
            pass
    return count


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    shards, index = int(sys.argv[1]), int(sys.argv[2])
    if not 1 <= index <= shards:
        print(f"index {index} is outside 1..{shards}", file=sys.stderr)
        return 2
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    members = set(metadata["workspace_members"])
    packages = sorted(
        ((weight(pathlib.Path(p["manifest_path"])), p["name"]) for p in metadata["packages"] if p["id"] in members),
        key=lambda item: (-item[0], item[1]),
    )
    loads = [0] * shards
    assigned: list[list[str]] = [[] for _ in range(shards)]
    for cost, name in packages:
        target = min(range(shards), key=lambda i: (loads[i], i))
        loads[target] += max(cost, 1)
        assigned[target].append(name)
    print(" ".join(f"-p {name}" for name in sorted(assigned[index - 1])))
    return 0


if __name__ == "__main__":
    sys.exit(main())
