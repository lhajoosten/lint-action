"""Run the same adapter used by the composite Action with a PR event."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from action_fixture import BASELINE, CONFIG, build, git, verify

PROJECT = Path(__file__).resolve().parent.parent
BIN = Path(os.environ["LINT_GATE_BIN"]).resolve()


DUPLICATION_MESSAGE = (
    "error source-duplication: 10 shared tokens (minimum 10, blocking) "
    "at copy.tsx:1:19-1:45 and original.ts:1:23-1:49"
)


class ActionTests(unittest.TestCase):
    def test_trusted_base_report_and_annotations(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory) / "workspace"
            workspace.mkdir()
            revision = build(workspace, BIN)
            env = {
                **os.environ,
                "LINT_GATE_PR_BASE": revision,
                "LINT_GATE_POLICY_REF": "",
                "LINT_GATE_POLICY_DIRECTORY": "",
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_WORKSPACE": str(workspace),
                "RUNNER_TEMP": directory,
                "GITHUB_OUTPUT": str(Path(directory) / "output"),
                "GITHUB_STEP_SUMMARY": str(Path(directory) / "summary"),
            }
            reports = []
            for name, code in (("pass", 0), ("fail", 1)):
                env["LINT_GATE_REPOSITORY"] = f"action-fixture-{name}"
                process = subprocess.run(
                    [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                    env=env, capture_output=True, text=True, check=False,
                )
                self.assertEqual(process.returncode, code, (process.stdout, process.stderr))
                reports.append(Path(dict(line.split("=", 1) for line in
                                 Path(env["GITHUB_OUTPUT"]).read_text().splitlines())["report"]))
                if code:
                    self.assertIn("file=action-fixture-fail/copy.tsx,line=1,col=19::"
                                  + DUPLICATION_MESSAGE, process.stdout)
                    self.assertIn(
                        "file=action-fixture-fail/new%2C%3A%25%0Aname.ts,line=1,col=22"
                        "::fresh has error cyclomatic complexity 2 (limit 1, blocking)",
                        process.stdout,
                    )
                    self.assertIn("nested has error cognitive complexity 3 (limit 1, blocking)", process.stdout)
                else:
                    self.assertIn("::warning title=lint-gate quality gate,file=action-fixture-pass/legacy.ts",
                                  process.stdout)
            verify(*reports)
            summary = Path(env["GITHUB_STEP_SUMMARY"]).read_text()
            self.assertIn("Outcome: **passed**", summary)
            self.assertIn("Outcome: **violations**", summary)
            self.assertIn("Baselined: 1 · Blocking: 4", summary)
            self.assertIn("#### New (4)", summary)
            self.assertIn("Unchanged findings: **1**", summary)
            self.assertIn("source-duplication 10 (minimum 10, blocking", summary)
            self.assertIn("copy.tsx:1:19-1:45", summary)
            self.assertIn("original.ts:1:23-1:49", summary)

            failing = workspace / env["LINT_GATE_REPOSITORY"]
            candidate_baseline = json.loads((failing / BASELINE).read_bytes())
            self.assertEqual(candidate_baseline["duplication_entries"][0]["score"], 10)
            candidate_policy = failing / CONFIG
            candidate_policy.write_text(
                '{"mode":"report-only","limits":{"typescript":999},'
                '"severities":{"cyclomatic-complexity":"pass","source-duplication":"pass"}}'
            )
            process = subprocess.run(
                [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(process.returncode, 1, process.stdout)
            self.assertIn("complexity 2 (limit 1, blocking)", process.stdout)
            self.assertIn(DUPLICATION_MESSAGE, process.stdout)
            outputs = dict(line.split("=", 1) for line in Path(env["GITHUB_OUTPUT"]).read_text().splitlines())
            dirty_delta = json.loads(Path(outputs["report"]).read_bytes())["comparison"]
            clean_delta = json.loads(reports[1].read_bytes())["comparison"]
            self.assertIsNone(dirty_delta["candidate_revision"])
            self.assertEqual(dirty_delta["changes"], clean_delta["changes"])
            git(failing, "restore", "--", CONFIG)

            # A candidate cannot provide a baseline absent from the trusted commit.
            git(failing, "rm", "--cached", "--", BASELINE)
            git(failing, "commit", "-qm", "Trusted revision has no baseline")
            env["LINT_GATE_PR_BASE"] = git(failing, "rev-parse", "HEAD")
            process = subprocess.run(
                [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(process.returncode, 2, process.stdout)
            self.assertIn("invalid_baseline", process.stdout)

            # An explicit trusted directory may be used to stage initial rollout.
            trusted = Path(directory) / "trusted"
            trusted.mkdir()
            (trusted / CONFIG).write_text(
                '{"mode":"strict","limits":{"typescript":1},"cognitive_limits":{"typescript":1},'
                '"severities":{"cyclomatic-complexity":"pass","cognitive-complexity":"warn"}}'
            )
            env["LINT_GATE_POLICY_DIRECTORY"] = str(trusted)
            process = subprocess.run(
                [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(process.returncode, 0, process.stdout)
            output = Path(dict(line.split("=", 1) for line in
                                 Path(env["GITHUB_OUTPUT"]).read_text().splitlines())["report"])
            self.assertEqual(json.loads(output.read_bytes())["mode"], "strict")
            self.assertIn("::warning title=lint-gate quality gate,file=action-fixture-fail/nested.ts",
                          process.stdout)
            self.assertIn("nested has warn cognitive complexity 3 (limit 1, reported)", process.stdout)
            self.assertIn("Largest contributions: +2 if at line 1", process.stdout)
            self.assertIn("Warning severity: 1 · Error severity: 0",
                          Path(env["GITHUB_STEP_SUMMARY"]).read_text())

            env["LINT_GATE_POLICY_DIRECTORY"] = str(failing)
            process = subprocess.run(
                [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(process.returncode, 2, process.stdout)
            self.assertIn("outside GITHUB_WORKSPACE", process.stdout)

    def test_comparison_uses_event_base_and_checked_out_merge(self):
        with tempfile.TemporaryDirectory() as directory:
            workspace = Path(directory) / "workspace"
            workspace.mkdir()
            trusted = build(workspace, BIN)
            candidate = workspace / "action-fixture-fail"
            head = git(candidate, "rev-parse", "HEAD")
            git(candidate, "checkout", "-q", "--detach", trusted)
            (candidate / ".gitattributes").write_text("legacy.ts export-ignore\n")
            (candidate / "base-only.ts").write_text("export const extra = () => false ? 2 : 3;\n")
            for name, expression in (("worse", "a || b"), ("better", "a || b || c")):
                (candidate / f"{name}.ts").write_text(f"const {name} = () => {expression};\n")
            git(candidate, "add", "--", ".")
            git(candidate, "commit", "-qm", "Advance the PR base separately from the policy ref")
            base = git(candidate, "rev-parse", "HEAD")
            git(candidate, "merge", "-q", "--no-ff", "--no-commit", head)
            for name, expression in (("worse", "a || b || c"), ("better", "a || b")):
                (candidate / f"{name}.ts").write_text(f"const {name} = () => {expression};\n")
            git(candidate, "add", "--", ".")
            git(candidate, "commit", "-qm", "Synthetic merge")
            merge = git(candidate, "rev-parse", "HEAD")
            env = {
                **os.environ,
                "LINT_GATE_PR_BASE": base,
                "LINT_GATE_POLICY_REF": trusted,
                "LINT_GATE_POLICY_DIRECTORY": "",
                "LINT_GATE_REPOSITORY": ".",
                "GITHUB_EVENT_NAME": "pull_request",
                "GITHUB_WORKSPACE": str(candidate),
                "GITHUB_REPOSITORY": "example/repo",
                "GITHUB_SERVER_URL": "https://github.com",
                "RUNNER_TEMP": directory,
                "GITHUB_OUTPUT": str(Path(directory) / "output"),
                "GITHUB_STEP_SUMMARY": str(Path(directory) / "summary"),
            }

            def run(code):
                Path(env["GITHUB_STEP_SUMMARY"]).write_text("")
                process = subprocess.run(
                    [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                    env=env, capture_output=True, text=True, check=False,
                )
                self.assertEqual(process.returncode, code, (process.stdout, process.stderr))
                output = dict(line.split("=", 1) for line in Path(env["GITHUB_OUTPUT"]).read_text().splitlines())
                self.assertEqual(output["outcome"], str(code))
                return (json.loads(Path(output["report"]).read_bytes()),
                        Path(env["GITHUB_STEP_SUMMARY"]).read_text())

            report, summary = run(1)
            comparison = report.pop("comparison")
            self.assertEqual((comparison["base_revision"], comparison["candidate_revision"]), (base, merge))
            self.assertEqual(comparison["unchanged"], 2)
            self.assertEqual([item["category"] for item in comparison["changes"]],
                             ["new"] * 4 + ["worsened", "improved"])
            self.assertLess(summary.index("#### New"), summary.index("#### Worsened"))
            self.assertLess(summary.index("#### Worsened"), summary.index("#### Improved"))
            self.assertIn("cyclomatic-complexity 2 → 3 (limit 1, blocking)", summary)
            self.assertIn("cyclomatic-complexity 3 → 2 (limit 1, blocking)", summary)
            for path in ("copy.tsx", "original.ts"):
                self.assertIn(f"https://github.com/example/repo/blob/{merge}/{path}#L1", summary)
            self.assertIn(f"/blob/{merge}/new%2C%3A%25%0Aname.ts#L1", summary)
            self.assertNotIn("base-only.ts", summary)

            env["LINT_GATE_PR_BASE"] = "1" * 40
            unavailable, summary = run(1)
            delta = unavailable.pop("comparison")
            self.assertEqual(report, unavailable)
            self.assertEqual((delta["status"], delta["reason"]), ("unavailable", "base_root_unavailable"))
            self.assertIn("Comparison unavailable", summary)
            self.assertIn("fetch-depth: 0", summary)

            env["LINT_GATE_PR_BASE"] = base
            git(candidate, "checkout", "-q", "--detach", base)
            (candidate / "bad.ts").write_text("function (")
            git(candidate, "add", "--", "bad.ts")
            git(candidate, "commit", "-qm", "Historical base contains a parse error")
            env["LINT_GATE_PR_BASE"] = git(candidate, "rev-parse", "HEAD")
            git(candidate, "checkout", "-q", "--detach", merge)
            failed, summary = run(1)
            self.assertEqual(failed["comparison"]["reason"], "base_analysis_failed")
            self.assertIn("bad.ts", summary)
            self.assertIn("parse_error", summary)

            (candidate / "bad.ts").write_text("function (")
            failed, summary = run(2)
            self.assertEqual(failed["comparison"]["reason"], "candidate_analysis_failed")
            self.assertIn("Outcome: **analysis failed**", summary)


if __name__ == "__main__":
    unittest.main()
