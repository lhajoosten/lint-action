"""Bind a privileged workflow run to the still-current PR scan target."""

import json
import os
from pathlib import Path
import re

from publish import GitHub, REPO, SHA, Skip, verified_scan_target


def main():
    env = os.environ
    repository = env["GITHUB_REPOSITORY"]
    workflow_file = env["SOURCE_WORKFLOW"]
    source = env["SCANNER_WORKFLOW"]
    job_name = env["ANALYSIS_JOB"]
    if not (
        re.fullmatch(REPO, repository)
        and re.fullmatch(r"[\w.-]+\.ya?ml", workflow_file, re.ASCII)
        and re.fullmatch(rf"{REPO}/\.github/workflows/source-complexity\.yml@(?:{SHA}|refs/heads/[\w./-]+)", source)
    ):
        raise SystemExit("Invalid analysis workflow configuration.")

    api = GitHub(env["GITHUB_API_URL"], repository, env["GITHUB_TOKEN"])
    event = json.loads(Path(env["GITHUB_EVENT_PATH"]).read_bytes())
    output = Path(env["GITHUB_OUTPUT"])
    try:
        run, number, revision, base, target, scanner_repository, scanner_sha, steps = verified_scan_target(
            api, event, workflow_file, source, job_name,
            int(env["GITHUB_REPOSITORY_ID"]),
        )
    except Skip as error:
        output.write_text("ready=false\n", encoding="utf-8")
        print(f"Skipped conversation publication. {error}")
        return

    # The native scan's state decides whether the conversation scan is complete.
    complete = (steps.get("Scan source") == "success" or
                (steps.get("Scan source") == "failure" and
                 steps.get("Record policy violations") == "success"))
    outcome = (
        "bootstrap" if steps.get("Record policy bootstrap") == "success" and
        steps.get("Scan source") == "skipped" else
        "passed" if steps.get("Scan source") == "success" else
        "blocking" if complete else "incomplete"
    )
    details = f"{env['GITHUB_SERVER_URL']}/{repository}/actions/runs/{run['id']}/attempts/{run['run_attempt']}"
    values = {
        "ready": "true",
        "pull_request": str(number),
        "revision": revision,
        "base": base,
        "head": target,
        "source_run": str(run["id"]),
        "source_attempt": str(run["run_attempt"]),
        "scanner_repository": scanner_repository,
        "scanner_sha": scanner_sha,
        "details_url": details,
        "scan_complete": str(complete).lower(),
        "source_outcome": outcome,
    }
    output.write_text("".join(f"{key}={value}\n" for key, value in values.items()), encoding="utf-8")


if __name__ == "__main__":
    main()
