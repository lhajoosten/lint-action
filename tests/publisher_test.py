"""Exercise trusted rescan publication at the GitHub API boundary."""

from copy import deepcopy
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

PROJECT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT / "action"))

from publish import ApiError, CHECK, Skip, line_anchor, publish, review_anchor

HEAD = "a" * 40
BASE = "b" * 40
MERGE = "c" * 40
SCANNER_SHA = "d" * 40
WORKFLOW = "own-source-complexity.yml"
SCANNER = "owner/tool/.github/workflows/source-complexity.yml"
JOB = "complexity / complexity"
REPOSITORY_URL = "https://github.com/owner/repo"


def finding(status="blocking", path="new.ts", line=1):
    return {
        "rule": "cyclomatic-complexity", "path": path, "language": "typescript",
        "scope": [{"kind": "function", "name": "fresh"}], "name": "fresh",
        "range": {"start": {"line": line, "column": 1}, "end": {"line": line, "column": 30}},
        "score": 3, "limit": 2, "severity": "warn" if status == "reported" else "error", "status": status,
    }


def report(findings=(), diagnostics=()):
    return {
        "format_version": 2, "scoring_version": 1, "mode": "baseline",
        "files_scanned": 1, "files_skipped": 0,
        "findings": list(findings), "diagnostics": list(diagnostics),
    }


def verify_candidate_reports(passing, failing):
    passing_report = json.loads(Path(passing).read_bytes())
    failing_report = json.loads(Path(failing).read_bytes())
    pass_api = FixtureGitHub()
    pass_api.report = passing_report
    pass_api.receive()
    assert not pass_api.review_comments and not pass_api.issue_comments
    assert pass_api.checks[0]["conclusion"] == "neutral"

    fail_api = FixtureGitHub()
    fail_api.report = failing_report
    fail_api.steps["Scan source"] = "failure"
    fail_api.steps["Record policy violations"] = "success"
    fail_api.receive()
    assert not fail_api.review_comments
    assert "Unanchored blocking error" in fail_api.issue_comments[0]["body"]
    assert "Baselined" in fail_api.checks[0]["output"]["summary"]
    assert fail_api.checks[0]["conclusion"] == "neutral"


class FixtureGitHub:
    def __init__(self):
        self.run = {
            "id": 100, "run_attempt": 1, "run_number": 4, "workflow_id": 30,
            "event": "pull_request", "path": f".github/workflows/{WORKFLOW}@refs/pull/7/merge",
            "status": "completed", "repository": {"id": 10}, "head_repository": {"id": 20},
            "head_sha": HEAD, "head_branch": "feature",
            "referenced_workflows": [{
                "path": f"{SCANNER}@main", "ref": "refs/heads/main", "sha": SCANNER_SHA,
            }],
            "pull_requests": [],
        }
        self.pr = {
            "number": 7, "state": "open", "merge_commit_sha": MERGE,
            "head": {"repo": {"id": 20}, "sha": HEAD, "ref": "feature"},
            "base": {"repo": {"id": 10}, "sha": BASE},
        }
        self.steps = {f"Record candidate 7 {MERGE} {BASE}": "success", "Scan source": "success"}
        self.report = report()
        self.checks = []
        self.review_comments = []
        self.issue_comments = []
        self.threads = []
        self.writes = []
        self.next_id = 60
        self.attach_error = None
        self.comment_error = None

    def pages(self, path, key):
        if key == "jobs":
            assert path == f"/actions/runs/{self.run['id']}/attempts/{self.run['run_attempt']}/jobs"
            return [{
                "name": JOB, "run_id": self.run["id"], "run_attempt": self.run["run_attempt"],
                "steps": [{"name": name, "conclusion": value} for name, value in self.steps.items()],
            }]
        if key == "check_runs":
            assert path.startswith(f"/commits/{HEAD}/check-runs?")
            return self.checks
        if key == "files":
            assert path.startswith("/pulls/7/files")
            return [{"filename": "new.ts", "status": "added",
                     "patch": "@@ -0,0 +1,2 @@\n+function fresh() { return flag ? 1 : 0; }\n+"}]
        if key == "comments":
            if path.startswith("/pulls/7/comments"):
                return self.review_comments
            if path.startswith("/issues/7/comments"):
                return self.issue_comments
        raise AssertionError((path, key))

    def json(self, path, method="GET", payload=None):
        if method == "GET":
            return {
                f"/actions/workflows/{WORKFLOW}": {"id": 30},
                f"/actions/runs/{self.run['id']}": self.run,
                "/pulls/7": self.pr,
                f"/git/commits/{MERGE}": {"sha": MERGE, "parents": [{"sha": BASE}, {"sha": HEAD}]},
            }[path]
        self.writes.append((method, path))
        if path.startswith("/check-runs"):
            if self.attach_error:
                raise ApiError("check write", self.attach_error)
            check = {"id": 60, "head_sha": HEAD, "app": {"id": 42}, **payload}
            if method == "POST":
                assert path == "/check-runs" and payload["head_sha"] == HEAD
                self.checks.append(check)
            else:
                assert path == "/check-runs/60" and "head_sha" not in payload
                self.checks[0] = check
            return check
        if path == "/pulls/7/comments":
            if self.comment_error:
                raise ApiError("review comment", self.comment_error)
            comment = {"id": self.next_id, "body": payload["body"],
                       "performed_via_github_app": {"id": 42}, **payload}
            self.next_id += 1
            self.review_comments.append(comment)
            self.threads.append({"id": f"thread-{comment['id']}", "isResolved": False,
                                 "comments": {"nodes": [{"databaseId": comment["id"], "body": comment["body"]}]}})
            return comment
        if path.startswith("/pulls/comments/") and method == "PATCH":
            comment_id = int(path.rsplit("/", 1)[1])
            comment = next(comment for comment in self.review_comments if comment["id"] == comment_id)
            comment.update(payload)
            return comment
        if path == "/issues/7/comments":
            comment = {"id": self.next_id, "body": payload["body"],
                       "performed_via_github_app": {"id": 42}}
            self.next_id += 1
            self.issue_comments.append(comment)
            return comment
        if path.startswith("/issues/comments/"):
            comment_id = int(path.rsplit("/", 1)[1])
            comment = next(comment for comment in self.issue_comments if comment["id"] == comment_id)
            comment.update(payload)
            return comment
        raise AssertionError((method, path, payload))

    def graphql(self, query, variables=None):
        if "query(" in query:
            return {"repository": {"pullRequest": {"reviewThreads": {
                "nodes": deepcopy(self.threads), "pageInfo": {"hasNextPage": False, "endCursor": None},
            }}}}
        thread = next(item for item in self.threads if item["id"] == variables["id"])
        thread["isResolved"] = "resolveReviewThread" in query
        self.writes.append(("GRAPHQL", "resolve" if thread["isResolved"] else "unresolve"))
        return {"resolveReviewThread" if thread["isResolved"] else "unresolveReviewThread": {
            "thread": {"id": thread["id"], "isResolved": thread["isResolved"]},
        }}

    def receive(self, incoming=None):
        event = {"repository": {"id": 10}, "workflow_run": incoming or deepcopy(self.run)}
        source_outcome = (
            "bootstrap" if self.steps.get("Record policy bootstrap") == "success" and
            self.steps.get("Scan source") == "skipped" else
            "passed" if self.steps.get("Scan source") == "success" else
            "blocking" if self.steps.get("Scan source") == "failure" and
            self.steps.get("Record policy violations") == "success" else "incomplete"
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            path.write_text(json.dumps(self.report), encoding="utf-8")
            env = {
                "GITHUB_REPOSITORY": "owner/repo",
                "FINDINGS_REPORT": str(path), "PULL_REQUEST": "7", "VERIFIED_REVISION": MERGE,
                "VERIFIED_BASE": BASE, "VERIFIED_HEAD": HEAD, "SOURCE_RUN": str(self.run["id"]),
                "SOURCE_ATTEMPT": str(self.run["run_attempt"]),
                "DETAILS_URL": f"{REPOSITORY_URL}/actions/runs/100/attempts/{self.run['run_attempt']}",
                "SCAN_COMPLETE": "true",
                "SOURCE_OUTCOME": source_outcome,
                "VERIFIED_SCANNER_REPOSITORY": "owner/tool", "VERIFIED_SCANNER_SHA": SCANNER_SHA,
            }
            with patch.dict(os.environ, env):
                return publish(self, self, event, WORKFLOW, f"{SCANNER}@refs/heads/main",
                               JOB, 10, 42, REPOSITORY_URL)


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.api = FixtureGitHub()

    def test_warning_summary_is_upserted_on_duplicate_delivery(self):
        self.api.report = report([finding("reported")])
        self.api.receive()
        self.api.receive()
        self.assertEqual(len(self.api.issue_comments), 1)
        self.assertIn("Warning", self.api.issue_comments[0]["body"])
        self.assertEqual(len(self.api.review_comments), 0)
        self.assertEqual([method for method, path in self.api.writes if path == "/check-runs"], ["POST"])
        self.assertIn(("PATCH", f"/issues/comments/{self.api.issue_comments[0]['id']}"), self.api.writes)

    def test_blocking_finding_creates_resolvable_thread_and_fix_resolves_it(self):
        self.api.report = report([finding()])
        self.api.steps["Scan source"] = "failure"
        self.api.steps["Record policy violations"] = "success"
        self.api.receive()
        self.assertEqual(len(self.api.review_comments), 1)
        self.assertEqual(self.api.review_comments[0]["line"], 1)
        self.assertIn("lint-gate-finding:", self.api.review_comments[0]["body"])
        self.api.report = report()
        self.api.steps["Scan source"] = "success"
        self.api.steps.pop("Record policy violations")
        self.api.run["run_attempt"] = 2
        self.api.receive()
        self.assertTrue(self.api.threads[0]["isResolved"])

    def test_retry_updates_same_finding_thread_without_duplicate(self):
        self.api.report = report([finding()])
        self.api.steps["Scan source"] = "failure"
        self.api.steps["Record policy violations"] = "success"
        self.api.receive()
        old_body = self.api.review_comments[0]["body"]
        updated = finding()
        updated["score"] = 4
        self.api.report = report([updated])
        self.api.run["run_attempt"] = 2
        self.api.receive()
        self.assertEqual(len(self.api.review_comments), 1)
        self.assertNotEqual(old_body, self.api.review_comments[0]["body"])
        self.assertIn("score `4`", self.api.review_comments[0]["body"])

    def test_rejected_or_unavailable_anchor_falls_back_to_summary(self):
        self.api.report = report([finding()])
        self.api.steps["Scan source"] = "failure"
        self.api.steps["Record policy violations"] = "success"
        self.api.comment_error = 422
        self.api.receive()
        self.assertEqual(len(self.api.review_comments), 0)
        self.assertIn("Unanchored blocking error", self.api.issue_comments[0]["body"])

    def test_duplication_anchors_second_occurrence_when_first_is_not_in_diff(self):
        duplicate = {
            "rule": "source-duplication", "severity": "error", "status": "blocking",
            "score": 20, "limit": 10,
            "occurrences": [
                {"path": "legacy.ts", "language": "typescript", "scope": [],
                 "range": {"start": {"line": 2, "column": 1}}},
                {"path": "new.ts", "language": "typescript", "scope": [],
                 "range": {"start": {"line": 2, "column": 1}}},
            ],
        }
        self.assertEqual(line_anchor(duplicate, [{"filename": "new.ts", "status": "added",
                                                  "patch": "@@ -0,0 +1,2 @@\n+a\n+b"}]), ("new.ts", 2))

    def test_file_level_anchor_is_used_when_no_line_is_in_diff(self):
        item = finding(line=99)
        files = [{"filename": "new.ts", "status": "modified",
                  "patch": "@@ -1 +1 @@\n-old\n+new"}]
        self.assertEqual(review_anchor(item, files), ("file", "new.ts"))
        self.api.report = report([item])
        self.api.steps["Scan source"] = "failure"
        self.api.steps["Record policy violations"] = "success"
        self.api.receive()
        self.assertEqual(self.api.review_comments[0]["subject_type"], "file")

    def test_thread_limit_places_overflow_in_maintained_summary(self):
        findings = [finding() for _ in range(51)]
        for index, item in enumerate(findings):
            item["name"] = f"fresh_{index}"
        self.api.report = report(findings)
        self.api.steps["Scan source"] = "failure"
        self.api.steps["Record policy violations"] = "success"
        self.api.receive()
        self.assertEqual(len(self.api.review_comments), 50)
        self.assertIn("thread limit reached", self.api.issue_comments[0]["body"])
        self.assertIn("verified scan", self.api.issue_comments[0]["body"])

    def test_baseline_entry_creates_no_required_conversation(self):
        self.api.report = report([finding("baselined")])
        self.api.receive()
        self.assertEqual(self.api.review_comments, [])
        self.assertEqual(self.api.issue_comments, [])

    def test_pre_severity_report_format_remains_supported(self):
        old = finding("reported")
        old.pop("severity")
        self.api.report = report([old])
        self.api.report["format_version"] = 1
        self.api.receive()
        self.assertIn("Warning", self.api.issue_comments[0]["body"])

    def test_unknown_report_version_cannot_drive_feedback_writes(self):
        self.api.report = report([finding("blocking")])
        self.api.steps["Scan source"] = "failure"
        self.api.steps["Record policy violations"] = "success"
        self.api.report["format_version"] = 99
        with self.assertRaisesRegex(ValueError, "incomplete or invalid"):
            self.api.receive()
        self.assertEqual(self.api.writes, [])

    def test_incomplete_analysis_leaves_existing_feedback_untouched(self):
        self.api.report = report([], [{"kind": "parse_error", "message": "invalid"}])
        self.api.steps["Scan source"] = "failure"
        self.api.receive()
        self.assertEqual(self.api.checks[0]["output"]["title"], "Analysis incomplete")
        self.assertFalse(any(path.startswith("/pulls/7/comments") or path.startswith("/issues/7/comments")
                             or method == "GRAPHQL" for method, path in self.api.writes))

    def test_stale_pr_or_scan_metadata_is_skipped(self):
        self.api.pr["merge_commit_sha"] = "e" * 40
        with self.assertRaisesRegex(Skip, "synthetic merge"):
            self.api.receive()
        self.api.pr["merge_commit_sha"] = MERGE
        with patch.dict(os.environ, {"VERIFIED_SCANNER_SHA": "f" * 40}):
            event = {"repository": {"id": 10}, "workflow_run": deepcopy(self.api.run)}
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "report.json"
                path.write_text(json.dumps(self.api.report), encoding="utf-8")
                with patch.dict(os.environ, {
                    "FINDINGS_REPORT": str(path), "PULL_REQUEST": "7", "VERIFIED_REVISION": MERGE,
                    "VERIFIED_BASE": BASE, "VERIFIED_HEAD": HEAD, "SOURCE_RUN": "100", "SOURCE_ATTEMPT": "1",
                    "DETAILS_URL": f"{REPOSITORY_URL}/actions/runs/100/attempts/1", "SOURCE_OUTCOME": "passed",
                    "VERIFIED_SCANNER_REPOSITORY": "owner/tool", "VERIFIED_SCANNER_SHA": "f" * 40,
                }):
                    self.api.run["referenced_workflows"][0]["sha"] = SCANNER_SHA
                    with self.assertRaisesRegex(Skip, "target changed"):
                        publish(self.api, self.api, event, WORKFLOW, f"{SCANNER}@refs/heads/main", JOB, 10, 42, REPOSITORY_URL)
        self.assertEqual(self.api.writes, [])

    def test_human_review_threads_are_preserved(self):
        self.api.review_comments = [{"id": 90, "body": "human review", "performed_via_github_app": {"id": 999}}]
        self.api.threads = [{"id": "human-thread", "isResolved": False,
                             "comments": {"nodes": [{"databaseId": 90, "body": "human review"}]}}]
        self.api.report = report()
        self.api.receive()
        self.assertFalse(self.api.threads[0]["isResolved"])
        self.assertFalse(any(method == "GRAPHQL" for method, _ in self.api.writes))

    def test_permission_failure_is_reported(self):
        self.api.report = report([finding("reported")])
        original = self.api.json
        def denied(path, method="GET", payload=None):
            if path == "/issues/7/comments" and method == "POST":
                raise ApiError("comment write", 403)
            return original(path, method, payload)
        self.api.json = denied
        with self.assertRaisesRegex(ApiError, "HTTP 403"):
            self.api.receive()

    def test_untrusted_human_text_is_not_used_as_markdown_structure(self):
        malicious = finding("reported", "new.ts")
        malicious["name"] = "`@everyone\n# heading"
        self.api.report = report([malicious])
        self.api.receive()
        body = self.api.issue_comments[0]["body"]
        self.assertIn(r"\u0060\u0040everyone\n# heading", body)


if __name__ == "__main__":
    if len(sys.argv) == 3 and all(Path(path).is_file() for path in sys.argv[1:]):
        verify_candidate_reports(*sys.argv[1:])
    else:
        unittest.main()
