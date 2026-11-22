"""Normalize scan output for transport across the workflow boundary."""

import json
import os
from pathlib import Path

REPORT_NAME = "report.json"
REPORT_LIMIT = 256 * 1024
SAMPLE_LIMIT = 8
STATUSES = ("blocking", "reported", "baselined")


def normalize(report, run_id, attempt, revision):
    findings = report["findings"]
    diagnostics = report["diagnostics"]
    samples = [
        f'{item["kind"]}: {item["message"]}'[:300] for item in diagnostics[:SAMPLE_LIMIT]
    ]
    counts = dict.fromkeys(STATUSES, 0)
    for status in STATUSES:
        for item in findings:
            if item["status"] != status:
                continue
            counts[status] += 1
            if len(samples) < SAMPLE_LIMIT:
                location = item["occurrences"][0] if item["rule"] == "source-duplication" else item
                samples.append(
                    f'{location["path"]}:{location["range"]["start"]["line"]} '
                    f'({item["rule"]}, {status}, score {item["score"]}, limit {item["limit"]})'[:300]
                )
    return {
        "version": 1,
        "run_id": run_id,
        "attempt": attempt,
        "revision": revision,
        "files": report["files_scanned"],
        "errors": len(diagnostics),
        "counts": counts,
        "samples": samples,
    }


def main():
    report = json.loads(Path(os.environ["SCAN_REPORT"]).read_bytes())
    normalized = normalize(report, int(os.environ["GITHUB_RUN_ID"]),
                           int(os.environ["GITHUB_RUN_ATTEMPT"]), os.environ["GITHUB_SHA"])
    path = Path(os.environ["RUNNER_TEMP"]) / "lint-gate-app" / REPORT_NAME
    path.parent.mkdir()
    path.write_text(json.dumps(normalized), encoding="utf-8")


if __name__ == "__main__":
    main()
