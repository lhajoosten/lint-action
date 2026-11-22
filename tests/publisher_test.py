"""Exercise the publisher with GitHub metadata and real Action fixture reports."""

from copy import deepcopy
from io import BytesIO
import json
from pathlib import Path
import subprocess
import sys
import unittest
from zipfile import ZipFile

PROJECT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT / "action"))

from app_report import normalize, REPORT_NAME
from publish import ApiError, CHECK, Skip, publish

HEAD = "a" * 40
BASE = "b" * 40
MERGE = "c" * 40
WORKFLOW = "own-source-complexity.yml"
SCANNER = "owner/tool/.github/workflows/source-complexity.yml"
JOB = "complexity / complexity"
REPOSITORY_URL = "https://github.com/owner/repo"


class FixtureGitHub:
    def __init__(self):
        self.run = {
            "id": 100, "run_attempt": 1, "run_number": 4, "workflow_id": 30, "event": "pull_request",
            "path": f".github/workflows/{WORKFLOW}@refs/pull/7/merge", "status": "completed",
            "repository": {"id": 10}, "head_repository": {"id": 20},
            "head_sha": HEAD, "head_branch": "feature",
            "referenced_workflows": [
                {"path": f"{SCANNER}@main", "ref": "refs/heads/main", "sha": "d" * 40},
            ],
            "pull_requests": [],
        }
        self.pr = {
            "number": 7, "state": "open", "merge_commit_sha": MERGE,
            "head": {"repo": {"id": 20}, "sha": HEAD, "ref": "feature"},
            "base": {"repo": {"id": 10}, "sha": BASE},
        }
        self.steps = {f"Record candidate 7 {MERGE} {BASE}": "success", "Scan source": "success"}
        self.report = None
        self.checks = []
        self.writes = []
        self.attach_error = None

    def artifact(self, identifier):
        assert identifier == 50
        if isinstance(self.report, bytes):
            return self.report
        output = BytesIO()
        with ZipFile(output, "w") as archive:
            archive.writestr(REPORT_NAME, json.dumps(self.report))
        return output.getvalue()

    def pages(self, path, key):
        run_id = self.run["id"]
        attempt = self.run["run_attempt"]
        if key == "jobs":
            assert path == f"/actions/runs/{run_id}/attempts/{attempt}/jobs"
            return [{
                "name": JOB, "run_id": run_id, "run_attempt": attempt,
                "steps": [{"name": name, "conclusion": value} for name, value in self.steps.items()],
            }]
        if key == "artifacts":
            assert path == f"/actions/runs/{run_id}/artifacts?name=lint-gate-report-{run_id}-{attempt}"
            if self.report is None:
                return []
            return [{
                "name": f"lint-gate-report-{run_id}-{attempt}", "id": 50, "size_in_bytes": 100,
                "expired": False, "workflow_run": {"id": run_id, "repository_id": 10, "head_sha": HEAD},
            }]
        assert key == "check_runs" and path.startswith(f"/commits/{HEAD}/check-runs?")
        return self.checks

    def json(self, path, method="GET", payload=None):
        if method != "GET":
            if self.attach_error:
                raise ApiError("check write", self.attach_error)
            check = {"id": 60, "head_sha": HEAD, "app": {"id": 42}, **payload}
            if method == "POST":
                assert path == "/check-runs" and payload["head_sha"] == HEAD
                self.checks.append(check)
            else:
                assert path == "/check-runs/60" and "head_sha" not in payload
                self.checks[0] = check
            self.writes.append(method)
            return check
        return {
            f"/actions/workflows/{WORKFLOW}": {"id": 30},
            f"/actions/runs/{self.run['id']}": self.run,
            "/pulls/7": self.pr,
            f"/git/commits/{MERGE}": {"sha": MERGE, "parents": [{"sha": BASE}, {"sha": HEAD}]},
        }[path]

    def receive(self, incoming=None):
        event = {"repository": {"id": 10}, "workflow_run": incoming or deepcopy(self.run)}
        return publish(self, self, event, WORKFLOW, f"{SCANNER}@refs/heads/main",
                       JOB, 10, 42, REPOSITORY_URL)


def empty_report():
    return {"files_scanned": 1, "diagnostics": [], "findings": []}


def verify_candidate_reports(passing, failing):
    for path, expected in ((passing, "Warnings"), (failing, "Blocking policy findings")):
        api = FixtureGitHub()
        if expected.startswith("Blocking"):
            api.steps.update({"Scan source": "failure", "Record policy violations": "success"})
        api.report = normalize(json.loads(path.read_bytes()), 100, 1, MERGE)
        api.receive()
        check, = api.checks
        assert check["name"] == CHECK
        assert check["output"]["title"] == expected
        assert check["head_sha"] == HEAD
        assert f"GitHub synthetic merge tree `{MERGE}`" in check["output"]["summary"]
        assert check["details_url"] == f"{REPOSITORY_URL}/actions/runs/100/attempts/1"
        if expected.startswith("Blocking"):
            assert r"new,:%\nname.ts" in check["output"]["summary"]


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.api = FixtureGitHub()
        self.api.report = normalize(empty_report(), 100, 1, MERGE)

    def test_clean_and_redelivery(self):
        self.api.receive()
        self.api.receive()
        self.assertEqual(self.api.writes, ["POST"])
        self.assertEqual(self.api.checks[0]["output"]["title"], "Clean")
        self.assertEqual(self.api.checks[0]["conclusion"], "success")

    def test_rerun_and_stale_delivery(self):
        initial = deepcopy(self.api.run)
        self.api.receive()
        self.api.run["run_attempt"] = 2
        self.api.report["attempt"] = 2
        self.api.receive()
        with self.assertRaisesRegex(Skip, "advanced"):
            self.api.receive(initial)
        self.api.run.update(id=99, run_number=3, run_attempt=9)
        self.api.report.update(run_id=99, attempt=9)
        self.api.receive()
        self.assertEqual(self.api.writes, ["POST", "PATCH"])
        self.assertEqual(len(self.api.checks), 1)
        self.assertTrue(self.api.checks[0]["details_url"].endswith("/100/attempts/2"))

    def test_analysis_failure_even_with_report(self):
        self.api.steps["Scan source"] = "failure"
        failed = {**empty_report(), "diagnostics": [{"kind": "parse_error", "message": "invalid"}]}
        self.api.report = normalize(failed, 100, 1, MERGE)
        self.api.receive()
        self.assertEqual(self.api.checks[0]["output"]["title"], "Analysis incomplete")
        self.assertIn("parse_error", self.api.checks[0]["output"]["summary"])

    def test_bootstrap_does_not_need_a_report(self):
        self.api.steps.update({"Scan source": "skipped", "Record policy bootstrap": "success"})
        self.api.report = None
        self.api.receive()
        self.assertEqual(self.api.checks[0]["conclusion"], "skipped")
        self.assertEqual(self.api.checks[0]["output"]["title"], "Policy bootstrap")

    def test_wrong_source_or_revision_is_skipped(self):
        with self.assertRaisesRegex(Skip, "configured PR analysis workflow"):
            self.api.receive({**self.api.run, "id": 100, "workflow_id": 999})
        with self.assertRaisesRegex(Skip, "configured PR analysis workflow"):
            self.api.receive({**self.api.run, "head_sha": "f" * 40})
        self.api.pr["merge_commit_sha"] = "e" * 40
        with self.assertRaisesRegex(Skip, "synthetic merge"):
            self.api.receive()
        self.api.pr["merge_commit_sha"] = MERGE
        self.api.run["referenced_workflows"][0]["ref"] = "refs/tags/main"
        with self.assertRaisesRegex(Skip, "reusable scanner"):
            self.api.receive()
        self.assertEqual(self.api.writes, [])

    def test_wrong_or_malformed_report_is_unusable(self):
        valid = self.api.report
        for report in (b"invalid zip", {**valid, "revision": HEAD}, {**valid, "run_id": 91},
                       {**valid, "attempt": 3}, {**valid, "samples": ["x" * 301]}):
            with self.subTest(report=report):
                self.api.checks.clear()
                self.api.report = report
                self.api.receive()
                self.assertEqual(self.api.checks[0]["output"]["title"], "Report unavailable")
                self.assertIn("unusable", self.api.checks[0]["output"]["summary"])
        self.api.checks.clear()
        self.api.report = None
        self.api.receive()
        self.assertIn("missing", self.api.checks[0]["output"]["summary"])

    def test_report_text_is_rendered_as_literal_data(self):
        self.api.report["samples"] = ["```\n<a href='https://example.invalid'>@everyone</a>"]
        self.api.receive()
        summary = self.api.checks[0]["output"]["summary"]
        self.assertIn(r"\u0060\u0060\u0060\n<a", summary)
        self.assertEqual(summary.count("```"), 2)

    def test_unattachable_fork_keeps_actions_as_fallback(self):
        self.api.attach_error = 422
        with self.assertRaisesRegex(Skip, "could not attach"):
            self.api.receive()
        self.assertEqual(self.api.writes, [])

    def test_disabled_workflow_step(self):
        workflow = (PROJECT / ".github/workflows/publish-app-check.yml").read_text()
        step = workflow.split("      - name: Report disabled publishing\n", 1)[1].split("      - name:", 1)[0]
        self.assertIn("if: '!inputs.enabled'", step)
        command = step.split("run: ", 1)[1].strip()
        result = subprocess.check_output(["bash", "-c", command], text=True)
        self.assertIn("publishing is disabled", result)


if __name__ == "__main__":
    if len(sys.argv) == 3 and all(Path(path).is_file() for path in sys.argv[1:]):
        verify_candidate_reports(*(Path(path) for path in sys.argv[1:]))
    else:
        unittest.main()
