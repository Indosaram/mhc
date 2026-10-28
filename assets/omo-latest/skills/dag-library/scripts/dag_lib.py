"""dag_lib.py: Python twin of dag-lib.mjs. Store workflow graph definitions by name and start them again.

MIT License (c) 2026 Indo Yoon. See ../LICENSE.

From a code-mode Python cell:
    import importlib.util, os
    spec = importlib.util.spec_from_file_location("dag_lib", os.path.join(SKILL_DIR, "scripts", "dag_lib.py"))
    dag_lib = importlib.util.module_from_spec(spec); spec.loader.exec_module(dag_lib)
    loaded = dag_lib.load("nightly-check")              # rotated + filled, not started
    started = tool.workflow({"action": "start", "definition": loaded["definition"]})
    done = tool.workflow({"action": "wait", "run_id": started["run_id"], "detach": False})
(If the kernel's tool helpers are coroutines, put `await` in front of each tool call.)
"""

from __future__ import annotations

import copy
import inspect
import json
import os
import re
from datetime import datetime, timezone
from pathlib import Path

ENV_VAR = "MHC_DAG_LIBRARY"
LIB_SUBDIR = os.path.join(".mhc", "dags")
_NAME_RE = re.compile(r"^[A-Za-z0-9._-]+$")
_PH_RE = re.compile(r"\{\{(key|date|datetime)\}\}")


def check_name(name) -> str:
    s = "" if name is None else str(name)
    if not s or "/" in s or "\\" in s or ".." in s or not _NAME_RE.match(s):
        raise ValueError(
            f'dag-library: invalid definition name {json.dumps(s)}. Use only letters, digits, ".", "-" and "_"; '
            'path separators and ".." are not allowed.'
        )
    return s


def search_dirs(env=None, cwd=None, home=None, delimiter=None) -> list[str]:
    env = os.environ if env is None else env
    cwd = os.getcwd() if cwd is None else cwd
    home = str(Path.home()) if home is None else home
    delimiter = os.pathsep if delimiter is None else delimiter
    dirs: list[str] = []
    raw = env.get(ENV_VAR)
    if raw:
        for part in raw.split(delimiter):
            p = part.strip()
            if p:
                dirs.append(p if os.path.isabs(p) else os.path.normpath(os.path.join(cwd, p)))
    dirs += [os.path.join(cwd, LIB_SUBDIR), os.path.join(home, LIB_SUBDIR)]
    return dirs


def locate(name, **opts) -> str:
    check_name(name)
    dirs = search_dirs(**opts)
    for d in dirs:
        p = os.path.join(d, f"{name}.json")
        if os.path.isfile(p):
            return p
    listing = "\n".join(f"  - {d}" for d in dirs)
    raise FileNotFoundError(f'dag-library: no definition named "{name}". Searched:\n{listing}')


def utc_date(now: datetime) -> str:
    return now.strftime("%Y%m%d")


def utc_stamp(now: datetime) -> str:
    return now.strftime("%Y%m%d-%H%M%S")


def rotate_key(base: str, suffix=None, now: datetime | None = None) -> str:
    """None -> base-<UTC stamp> (fresh); "" -> base (reuse); "x" -> base-x (stable per suffix)."""
    if suffix == "":
        return base
    if suffix is None:
        return f"{base}-{utc_stamp(now or datetime.now(timezone.utc))}"
    return f"{base}-{suffix}"


def fill_placeholders(value, vars: dict):
    if isinstance(value, str):
        return _PH_RE.sub(lambda m: vars[m.group(1)], value)
    if isinstance(value, list):
        return [fill_placeholders(v, vars) for v in value]
    if isinstance(value, dict):
        return {k: fill_placeholders(v, vars) for k, v in value.items()}
    return value


def validate_definition(d) -> list[str]:
    problems: list[str] = []
    if not isinstance(d, dict):
        return ["definition must be an object"]
    for f in ("key", "name"):
        if not isinstance(d.get(f), str) or not d.get(f):
            problems.append(f'"{f}" must be a non-empty string')
    nodes = d.get("nodes")
    if not isinstance(nodes, list) or not nodes:
        problems.append('"nodes" must be a non-empty array')
        return problems
    ids: set[str] = set()
    for i, n in enumerate(nodes):
        at = f"nodes[{i}]"
        if not isinstance(n, dict):
            problems.append(f"{at} must be an object")
            continue
        nid = n.get("id")
        if not isinstance(nid, str) or not nid:
            problems.append(f"{at}.id is required")
        elif nid in ids:
            problems.append(f'{at}.id "{nid}" is duplicated')
        else:
            ids.add(nid)
        if not isinstance(n.get("prompt"), str) or not n.get("prompt"):
            problems.append(f"{at}.prompt is required")
        if n.get("category") and n.get("subagent_type"):
            problems.append(f"{at} sets both category and subagent_type")
        if n.get("model") and not n.get("subagent_type"):
            problems.append(f"{at}.model needs subagent_type")
    for i, n in enumerate(nodes):
        for dep in (n.get("dependsOn") or []) if isinstance(n, dict) else []:
            if dep not in ids:
                problems.append(f'nodes[{i}].dependsOn names unknown node "{dep}"')
    return problems


def load(name, suffix=None, now: datetime | None = None, **opts) -> dict:
    """Return {"definition", "path", "base_key", "key"}; nothing is started."""
    path = locate(name, **opts)
    with open(path, encoding="utf-8") as fh:
        stored = json.load(fh)
    problems = validate_definition(stored)
    if problems:
        raise ValueError(f"dag-library: {path} is not a valid definition:\n  - " + "\n  - ".join(problems))
    now = now or datetime.now(timezone.utc)
    key = rotate_key(stored["key"], suffix, now)
    d = copy.deepcopy(stored)
    d["key"] = key
    d = fill_placeholders(d, {"key": key, "date": utc_date(now), "datetime": utc_stamp(now)})
    return {"definition": d, "path": path, "base_key": stored["key"], "key": key}


def _summ(raw, loaded):
    raw = raw or {}
    return {
        "run_id": raw.get("run_id"),
        "reused": bool(raw.get("reused")),
        "warnings": raw.get("warnings") or [],
        "key": loaded["key"],
        "path": loaded["path"],
        "raw": raw,
    }


def start(name, call, suffix=None, **opts) -> dict:
    """call: the kernel's tool.workflow (sync). For an async kernel use `await start_async(...)`."""
    loaded = load(name, suffix=suffix, **opts)
    raw = call({"action": "start", "definition": loaded["definition"]})
    if inspect.isawaitable(raw):
        raise RuntimeError("dag-library: tool.workflow is async in this kernel; use `await dag_lib.start_async(...)`.")
    return _summ(raw, loaded)


async def start_async(name, call, suffix=None, **opts) -> dict:
    loaded = load(name, suffix=suffix, **opts)
    raw = call({"action": "start", "definition": loaded["definition"]})
    if inspect.isawaitable(raw):
        raw = await raw
    return _summ(raw, loaded)


def wait_args(run_id: str) -> dict:
    """Arguments for a blocking wait; the bare wait action detaches against a live run."""
    return {"action": "wait", "run_id": run_id, "detach": False}


def save(name, definition, scope: str, force: bool = False, cwd=None, home=None) -> dict:
    check_name(name)
    problems = validate_definition(definition)
    if problems:
        raise ValueError("dag-library: refusing to save an invalid definition:\n  - " + "\n  - ".join(problems))
    if scope not in ("user", "project"):
        raise ValueError('dag-library: scope must be "user" or "project"')
    root = (home or str(Path.home())) if scope == "user" else (cwd or os.getcwd())
    d = os.path.join(root, LIB_SUBDIR)
    p = os.path.join(d, f"{name}.json")
    existed = os.path.exists(p)
    if existed and not force:
        raise FileExistsError(f"dag-library: {p} already exists; pass force=True to overwrite.")
    os.makedirs(d, exist_ok=True)
    with open(p, "w", encoding="utf-8") as fh:
        json.dump(definition, fh, indent=2)
        fh.write("\n")
    return {"path": p, "scope": scope, "overwrote": existed}
