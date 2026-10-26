"""Python twin acceptance checks. Run: python3 tests/test_dag_lib.py"""
import json
import os
import sys
import tempfile
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "scripts"))
import dag_lib  # noqa: E402

EXAMPLE = json.load(open(os.path.join(HERE, "..", "references", "example-nightly-check.json")))


def box():
    root = tempfile.mkdtemp()
    cwd, home = os.path.join(root, "repo"), os.path.join(root, "home")
    os.makedirs(cwd), os.makedirs(home)
    return root, cwd, home


class Mock:
    def __init__(self):
        self.seen, self.n = {}, 0

    def __call__(self, args):
        if args["action"] == "start":
            sig = args["definition"]["key"] + json.dumps(args["definition"]["nodes"])
            if sig in self.seen:
                return {"run_id": self.seen[sig], "reused": True, "warnings": []}
            self.n += 1
            self.seen[sig] = f"run-{self.n}"
            return {"run_id": self.seen[sig], "reused": False, "warnings": []}
        if args["action"] == "wait":
            assert args["detach"] is False
            return {"status": "completed"}
        raise AssertionError(args)


def main():
    root, cwd, home = box()
    o = dict(cwd=cwd, home=home, env={})
    r = dag_lib.save("nightly-check", EXAMPLE, "project", cwd=cwd)
    assert r["path"] == os.path.join(cwd, ".mhc", "dags", "nightly-check.json")
    m = Mock()
    a = dag_lib.start("nightly-check", m, now=datetime(2026, 10, 26, 8, 0, 0, tzinfo=timezone.utc), **o)
    b = dag_lib.start("nightly-check", m, now=datetime(2026, 10, 26, 8, 0, 1, tzinfo=timezone.utc), **o)
    assert a["run_id"] != b["run_id"] and not a["reused"] and not b["reused"]
    c = dag_lib.start("nightly-check", m, suffix="s1", **o)
    d = dag_lib.start("nightly-check", m, suffix="s1", **o)
    assert not c["reused"] and d["reused"] and c["run_id"] == d["run_id"]
    assert dag_lib.load("nightly-check", suffix="", **o)["definition"]["key"] == "nightly-check"
    ld = dag_lib.load("nightly-check", **o)
    txt = json.dumps(ld["definition"])
    assert "{{" not in txt and f"reports/{ld['key']}/" in txt
    assert m(dag_lib.wait_args(a["run_id"]))["status"] == "completed"
    for bad in ["../x", "a/b", "", ".."]:
        try:
            dag_lib.check_name(bad)
            raise SystemExit(f"accepted {bad!r}")
        except ValueError:
            pass
    dirs = dag_lib.search_dirs(env={"MHC_DAG_LIBRARY": "C:\\a;D:\\b"}, cwd=cwd, home=home, delimiter=";")
    assert len(dirs) == 4
    try:
        dag_lib.load("missing", **o)
        raise SystemExit("no error")
    except FileNotFoundError as e:
        assert os.path.join(home, ".mhc", "dags") in str(e)
    print("python: all checks passed")


if __name__ == "__main__":
    main()
