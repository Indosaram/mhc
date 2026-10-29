#!/usr/bin/env python3
"""Gate the MIT build profile of the `mhc` binary (maho-cli built with `--no-default-features`).

Two checks over the normal (linked) dependency graph of each MIT-profile binary package (maho-cli
without default features, and the MIT sibling binary maho-senpi-desktop-engine), for every target
platform:

1. no package resolves to a path under crates/omo/ (the oh-my-openagent port, LicenseRef-SUL-1.0);
2. no package carries a license expression naming SUL (LicenseRef-SUL-1.0), and every package
   states a license (workspace crates included: no `[licenses.private] ignore`).

Usage: python3 scripts/check-mit-profile.py [--self-test]
Exit 0 = pass, 1 = SUL code in the MIT profile, 2 = tooling error.
See docs/MIT-PROFILE.md.
"""

from __future__ import annotations

import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
# (package, cargo feature arguments) for every binary the MIT profile builds and stages.
PROFILE = [
    ("maho-cli", ["--no-default-features"]),
    ("maho-senpi-desktop-engine", []),
]
FORBIDDEN_LICENSE = re.compile(r"SUL", re.IGNORECASE)
# `{p}` prints `name vX.Y.Z (path-or-source)`; `{l}` the package's license expression.
TREE_FORMAT = "{p}\t{l}"
LINE = re.compile(r"^(?P<name>\S+) v(?P<version>\S+)(?: \((?P<source>[^)]*)\))?\t(?P<license>.*)$")
# Annotations cargo tree appends after the package id.
ANNOTATIONS = (" (*)", " (proc-macro)")


def cargo_tree(package: str, feature_args: list[str]) -> list[str]:
    command = [
        "cargo", "tree", "--locked", "-p", package, *feature_args,
        "-e", "normal", "--target", "all", "--prefix", "none", "--format", TREE_FORMAT,
    ]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        raise SystemExit(2)
    return [line for line in result.stdout.splitlines() if line.strip()]


def is_omo_path(source: str | None) -> bool:
    if not source:
        return False
    normalized = source.replace("\\", "/")
    return "/crates/omo/" in normalized or normalized.endswith("/crates/omo")


def audit(lines: list[str]) -> tuple[list[str], int]:
    errors: list[str] = []
    seen: set[tuple[str, str, str]] = set()
    for line in lines:
        head, _, tail = line.partition("\t")
        stripped = True
        while stripped:
            stripped = False
            for annotation in ANNOTATIONS:
                if head.endswith(annotation):
                    head, stripped = head[: -len(annotation)], True
        match = LINE.match(f"{head}\t{tail}")
        if not match:
            errors.append(f"unparsed cargo tree line: {line!r}")
            continue
        name, version, source, license_ = match["name"], match["version"], match["source"], match["license"].strip()
        key = (name, version, source or "")
        if key in seen:
            continue
        seen.add(key)
        label = f"{name} v{version}" + (f" ({source})" if source else "")
        if is_omo_path(source):
            errors.append(f"{label}: resolves under crates/omo/ (SUL-1.0 oh-my-openagent port)")
        if not license_:
            errors.append(f"{label}: no license expression")
        elif FORBIDDEN_LICENSE.search(license_):
            errors.append(f"{label}: license {license_!r} is not allowed in the MIT profile")
    return errors, len(seen)


def self_test() -> int:
    ok = [
        "maho-cli v0.0.0 (/repo/crates/maho-cli)\tMIT",
        "serde v1.0.228\tMIT OR Apache-2.0",
        "maho-senpi-desktop-core v0.0.0 (/repo/crates/desktop/senpi-desktop-core)\tMIT",
        "serde v1.0.228\tMIT OR Apache-2.0",
    ]
    errors, count = audit(ok)
    assert errors == [] and count == 3, (errors, count)
    bad_path = ["maho-omo v0.0.0 (/repo/crates/omo/maho-omo)\tMIT"]
    assert any("crates/omo" in error for error in audit(bad_path)[0])
    bad_windows_path = ["maho-utils v0.0.0 (C:\\repo\\crates\\omo\\utils)\tMIT"]
    assert any("crates/omo" in error for error in audit(bad_windows_path)[0])
    bad_license = ["x v0.0.0 (/repo/crates/x)\tLicenseRef-SUL-1.0"]
    assert any("not allowed" in error for error in audit(bad_license)[0])
    no_license = ["x v0.0.0 (/repo/crates/x)\t"]
    assert any("no license" in error for error in audit(no_license)[0])
    deduped = ["serde v1.0.228\tMIT OR Apache-2.0", "serde v1.0.228 (*)\tMIT OR Apache-2.0"]
    assert audit(deduped) == ([], 1)
    macro = ["serde_derive v1.0.228 (proc-macro) (*)\tMIT OR Apache-2.0", "maho-x v0.0.0 (/repo/crates/omo/x) (proc-macro)\tMIT"]
    errors, count = audit(macro)
    assert count == 2 and len(errors) == 1 and "crates/omo" in errors[0], (errors, count)
    print("check-mit-profile self-test: ok")
    return 0


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    if sys.argv[1:]:
        sys.stderr.write(__doc__ or "")
        return 2
    failed = False
    for package, feature_args in PROFILE:
        profile = " ".join([package, *feature_args])
        errors, count = audit(cargo_tree(package, feature_args))
        if errors:
            failed = True
            print(f"MIT profile ({profile}) links SUL code:")
            for error in errors:
                print(f"  - {error}")
        else:
            print(f"MIT profile ({profile}): {count} linked packages, none under crates/omo/, no SUL license")
    return 1 if failed else 0

if __name__ == "__main__":
    raise SystemExit(main())
