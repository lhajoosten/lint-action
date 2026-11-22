"""Exercise the exact workflow preparation and report staging handoff."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parent.parent


class EvidenceHandoffTests(unittest.TestCase):
    def test_prepare_job_command_imports_trusted_module_in_isolated_mode(self):
        result = subprocess.run(
            ["python3", "-Es", "action/prepare_pr_scan.py"], cwd=PROJECT,
            env={**os.environ, "GITHUB_REPOSITORY": "invalid", "SOURCE_WORKFLOW": "scan.yml",
                 "SCANNER_WORKFLOW": "owner/repo/.github/workflows/source-complexity.yml@" + "a" * 40,
                 "ANALYSIS_JOB": "complexity"},
            capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Invalid analysis workflow configuration", result.stderr)
        self.assertNotIn("ModuleNotFoundError", result.stderr)

    def test_real_report_producer_filename_is_staged_as_writer_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / "candidate"
            repo.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.invalid"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / ".lint-action.json").write_text("{}", encoding="utf-8")
            (repo / "sample.ts").write_text("function sample() { return true; }\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "fixture"], check=True)
            revision = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
            fake_cli = root / "lint-gate"
            fake_cli.write_text(
                "#!/usr/bin/env python3\nimport json\nprint(json.dumps({"
                "'format_version': 2, 'scoring_version': 1, 'mode': 'baseline',"
                "'files_scanned': 1, 'files_skipped': 0, 'diagnostics': [],"
                "'findings': [{'rule':'cyclomatic-complexity','path':'sample.ts','range':"
                "{'start':{'line':1,'column':1}},'name':'sample','scope':[],'language':'typescript',"
                "'score':1,'limit':1,'severity':'warn','status':'reported'}]}))\n",
                encoding="utf-8",
            )
            fake_cli.chmod(0o755)
            runner_temp = root / "runner"
            runner_temp.mkdir()
            output = root / "step-output"
            env = {**os.environ, "GITHUB_WORKSPACE": str(repo), "RUNNER_TEMP": str(runner_temp),
                   "GITHUB_OUTPUT": str(output), "GITHUB_EVENT_NAME": "pull_request",
                   "LINT_GATE_PR_BASE": revision}
            subprocess.run(["python3", "-Es", "action/run.py", str(fake_cli)], cwd=PROJECT,
                           env=env, check=True, capture_output=True, text=True)
            report_path = Path(dict(line.split("=", 1) for line in output.read_text().splitlines())["report"])
            self.assertRegex(report_path.name, r"^lint-gate-report-.+\.json$")

            manifest_output = root / "stage-output"
            stage_env = {**os.environ, "RUNNER_TEMP": str(runner_temp), "GITHUB_OUTPUT": str(manifest_output),
                         "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "2",
                         "VERIFIED_REVISION": revision, "VERIFIED_BASE": revision,
                         "VERIFIED_HEAD": revision, "SOURCE_RUN": "99", "SOURCE_ATTEMPT": "1"}
            subprocess.run(["python3", "-Es", "action/stage_evidence.py", str(report_path), str(repo)],
                           cwd=PROJECT, env=stage_env, check=True)
            staged = runner_temp / "lint-gate-findings"
            self.assertEqual(json.loads((staged / "report.json").read_bytes()), json.loads(report_path.read_bytes()))
            manifest = json.loads((staged / "anchors.json").read_bytes())
            self.assertEqual(manifest["compatible_paths"], ["sample.ts"])
            self.assertEqual(manifest["artifact_name"], "lint-gate-verified-findings-123-2")
            self.assertEqual(manifest_output.read_text(), "artifact_name=lint-gate-verified-findings-123-2\n")


if __name__ == "__main__":
    unittest.main()
