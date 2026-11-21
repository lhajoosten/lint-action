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
    "source-duplication: 10 shared tokens (minimum 10, blocking) "
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
                reports.append(Path(Path(env["GITHUB_OUTPUT"]).read_text().splitlines()[-1][7:]))
                if code:
                    self.assertIn("file=action-fixture-fail/copy.tsx,line=1,col=19::"
                                  + DUPLICATION_MESSAGE, process.stdout)
                    self.assertIn(
                        "file=action-fixture-fail/new%2C%3A%25%0Aname.ts,line=1,col=22"
                        "::fresh has cyclomatic complexity 2 (limit 1, blocking)",
                        process.stdout,
                    )
                    self.assertIn("nested has cognitive complexity 3 (limit 1, blocking)", process.stdout)
                else:
                    self.assertIn("::warning title=lint-gate quality gate,file=action-fixture-pass/legacy.ts",
                                  process.stdout)
            verify(*reports)
            summary = Path(env["GITHUB_STEP_SUMMARY"]).read_text()
            self.assertIn("Outcome: **passed**", summary)
            self.assertIn("Outcome: **violations**", summary)
            self.assertIn("Baselined: 1 · Blocking: 4", summary)
            self.assertIn(DUPLICATION_MESSAGE, summary)

            failing = workspace / env["LINT_GATE_REPOSITORY"]
            candidate_baseline = json.loads((failing / BASELINE).read_bytes())
            self.assertEqual(candidate_baseline["duplication_entries"][0]["score"], 10)
            candidate_policy = failing / CONFIG
            candidate_policy.write_text('{"mode":"report-only","limits":{"typescript":999}}')
            process = subprocess.run(
                [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(process.returncode, 1, process.stdout)
            self.assertIn("complexity 2 (limit 1, blocking)", process.stdout)
            self.assertIn(DUPLICATION_MESSAGE, process.stdout)
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
                '{"mode":"report-only","limits":{"typescript":1},"cognitive_limits":{"typescript":1}}'
            )
            env["LINT_GATE_POLICY_DIRECTORY"] = str(trusted)
            process = subprocess.run(
                [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(process.returncode, 0, process.stdout)
            output = Path(Path(env["GITHUB_OUTPUT"]).read_text().splitlines()[-1][7:])
            self.assertEqual(json.loads(output.read_bytes())["mode"], "report-only")
            self.assertIn("nested has cognitive complexity 3 (limit 1, reported)", process.stdout)
            self.assertIn("Largest contributions: +2 if at line 1", process.stdout)

            env["LINT_GATE_POLICY_DIRECTORY"] = str(failing)
            process = subprocess.run(
                [sys.executable, "-I", PROJECT / "action/run.py", BIN],
                env=env, capture_output=True, text=True, check=False,
            )
            self.assertEqual(process.returncode, 2, process.stdout)
            self.assertIn("outside GITHUB_WORKSPACE", process.stdout)


if __name__ == "__main__":
    unittest.main()
