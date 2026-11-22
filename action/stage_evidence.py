"""Stage independently trusted scan output and safe review-coordinate metadata."""

import json
import os
from pathlib import Path
import subprocess
import sys


def blob(repository, revision, path):
    result = subprocess.run(
        ["git", "--no-replace-objects", "-C", repository, "rev-parse", "--verify", f"{revision}:{path}"],
        capture_output=True, check=False, text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def main():
    report_path, repository = sys.argv[1:]
    report = json.loads(Path(report_path).read_bytes())
    paths = set()
    for finding in report.get("findings", []):
        locations = finding.get("occurrences", []) if finding.get("rule") == "source-duplication" else [finding]
        paths.update(item.get("path") for item in locations if isinstance(item.get("path"), str))

    revision = os.environ["VERIFIED_REVISION"]
    head = os.environ["VERIFIED_HEAD"]
    base = os.environ["VERIFIED_BASE"]
    compatible = sorted(path for path in paths
                        if blob(repository, revision, path) is not None
                        and blob(repository, revision, path) == blob(repository, head, path))
    output = Path(os.environ["RUNNER_TEMP"]) / "lint-gate-findings"
    output.mkdir(parents=True, exist_ok=True)
    (output / "report.json").write_bytes(Path(report_path).read_bytes())
    name = f"lint-gate-verified-findings-{os.environ['GITHUB_RUN_ID']}-{os.environ['GITHUB_RUN_ATTEMPT']}"
    manifest = {
        "format_version": 1, "revision": revision, "base": base, "head": head,
        "source_run": os.environ["SOURCE_RUN"],
        "source_attempt": os.environ["SOURCE_ATTEMPT"],
        "artifact_name": name,
        "compatible_paths": compatible,
    }
    (output / "anchors.json").write_text(json.dumps(manifest, sort_keys=True), encoding="utf-8")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as handle:
        handle.write(f"artifact_name={name}\n")


if __name__ == "__main__":
    main()
