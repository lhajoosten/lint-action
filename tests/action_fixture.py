"""Build two fixture checkouts with the same trusted baseline history."""

import json
from pathlib import Path
import shutil
import subprocess
import sys

CONFIG = ".lint-action.json"
BASELINE = ".lint-action-baseline.json"
UNUSUAL = "new,:%\nname.ts"


def git(repository, *args):
    return subprocess.check_output(["git", "-C", repository, *args], text=True).strip()


def build(workspace, binary):
    passing = workspace / "action-fixture-pass"
    failing = workspace / "action-fixture-fail"
    passing.mkdir()
    git(passing, "init", "-q")
    git(passing, "config", "user.name", "Fixture")
    git(passing, "config", "user.email", "fixture@example.invalid")
    (passing / CONFIG).write_text('{"mode":"baseline","limits":{"typescript":1}}\n')
    (passing / "legacy.ts").write_text("export const old = () => true ? 1 : 0;\n")
    (passing / BASELINE).write_bytes(subprocess.check_output(
        [binary, "baseline", "--root", passing]
    ))
    git(passing, "add", "--", ".")
    git(passing, "commit", "-qm", "Trusted policy and existing debt")
    revision = git(passing, "rev-parse", "HEAD")
    shutil.copytree(passing, failing)
    (failing / UNUSUAL).write_text("export const fresh = () => false || true;\n")
    (failing / BASELINE).write_bytes(subprocess.check_output(
        [binary, "baseline", "--root", failing]
    ))
    git(failing, "add", "--", ".")
    git(failing, "commit", "-qm", "Candidate adds a violation and waives it in its baseline")
    return revision


def verify(passing, failing):
    reports = [json.loads(path.read_bytes()) for path in (passing, failing)]
    assert list(map(lambda report: report["mode"], reports)) == ["baseline", "baseline"]
    assert [(item["path"], item["status"]) for item in reports[0]["findings"]] == [
        ("legacy.ts", "baselined"),
    ]
    assert [(item["path"], item["status"]) for item in reports[1]["findings"]] == [
        ("legacy.ts", "baselined"), (UNUSUAL, "blocking"),
    ]


if __name__ == "__main__":
    if sys.argv[1] == "verify":
        verify(Path(sys.argv[2]), Path(sys.argv[3]))
    else:
        print(build(Path(sys.argv[2]), Path(sys.argv[3]).resolve()))
