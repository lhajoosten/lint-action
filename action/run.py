"""Resolve trusted policy, run the CLI, and render GitHub workflow output."""

import html
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

CHECK = "lint-action quality gate"
CONFIG = ".lint-action.json"
BASELINE = ".lint-action-baseline.json"
ANNOTATION_LIMIT = 10
SUMMARY_LIMIT = 10


def workflow_escape(value, property_value=False):
    safe = re.sub(r"[\x00-\x09\x0b\x0c\x0e-\x1f\x7f]", lambda match: f"\\x{ord(match[0]):02x}", str(value))
    escaped = safe.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    if property_value:
        escaped = escaped.replace(":", "%3A").replace(",", "%2C")
    return escaped


def annotation(level, message, path=None, location=None):
    properties = {"title": CHECK}
    if path:
        properties["file"] = path
    if path and location:
        properties.update(line=location["line"], col=location["column"])
    fields = ",".join(f"{key}={workflow_escape(value, True)}" for key, value in properties.items())
    print(f"::{level} {fields}::{workflow_escape(message[:1000])}")


def append_env_file(variable, text):
    path = os.environ.get(variable)
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(text)


def git(repository, *arguments):
    return subprocess.run(
        ["git", "--no-replace-objects", "-C", repository, *arguments],
        check=False,
        capture_output=True,
    )


def copy_base_policy(repository, revision, directory):
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", revision):
        raise ValueError("the trusted policy revision must be a full Git object ID")
    if git(repository, "cat-file", "-e", f"{revision}^{{commit}}").returncode:
        raise ValueError(f"cannot read trusted policy revision {revision}; check out with fetch-depth: 0")

    for name, limit in ((CONFIG, 4 * 1024**2), (BASELINE, 16 * 1024**2)):
        entry = git(repository, "ls-tree", revision, "--", name)
        if entry.returncode:
            raise ValueError(f"cannot read {name} at trusted revision {revision}")
        if not entry.stdout and name == BASELINE:
            continue
        if not entry.stdout:
            raise ValueError(f"{name} is missing from trusted revision {revision}")
        if not entry.stdout.startswith((b"100644 blob ", b"100755 blob ")):
            raise ValueError(f"{name} at trusted revision {revision} must be a regular file")
        spec = f"{revision}:{name}"
        size = git(repository, "cat-file", "-s", spec)
        if size.returncode or int(size.stdout) > limit:
            raise ValueError(f"cannot read {name} at trusted revision {revision} within {limit} bytes")
        blob = git(repository, "cat-file", "blob", spec)
        if blob.returncode:
            raise ValueError(f"cannot read {name} at trusted revision {revision}")
        (directory / name).write_bytes(blob.stdout)


def repository_path(workspace):
    selection = Path(os.environ.get("LINT_ACTION_REPOSITORY", "."))
    repository = (workspace / selection).resolve()
    if selection.is_absolute() or not repository.is_relative_to(workspace) or not repository.is_dir():
        raise ValueError("repository must be a directory within GITHUB_WORKSPACE")
    return repository


def trusted_directory(workspace, repository, temporary):
    event = os.environ.get("GITHUB_EVENT_NAME", "")
    if event == "pull_request_target":
        raise ValueError("use the pull_request event for candidate source analysis")
    directory_input = os.environ.get("LINT_ACTION_POLICY_DIRECTORY")
    revision = os.environ.get("LINT_ACTION_POLICY_REF")
    if directory_input:
        if revision:
            raise ValueError("policy-directory and policy-ref are mutually exclusive")
        directory = Path(directory_input)
        if not directory.is_absolute() or not directory.is_dir():
            raise ValueError("policy-directory must be an existing absolute directory")
        directory = directory.resolve()
        for path in (directory, directory / CONFIG, directory / BASELINE):
            if path.resolve().is_relative_to(workspace):
                raise ValueError("policy-directory and its policy files must be outside GITHUB_WORKSPACE")
        return directory
    revision = revision or os.environ.get("LINT_ACTION_PR_BASE")
    if event in ("pull_request", "pull_request_review", "pull_request_review_comment") and not revision:
        raise ValueError("pull request base SHA is unavailable")
    if revision:
        copy_base_policy(repository, revision, temporary)
        return temporary
    return repository


def text_for_summary(value):
    quoted = json.dumps(value, ensure_ascii=False) if isinstance(value, str) else str(value)
    return "<code>" + html.escape(quoted) + "</code>"


def render(report, code, relative_repository):
    findings = report["findings"]
    diagnostics = report["diagnostics"]
    totals = {status: sum(item["status"] == status for item in findings)
              for status in ("reported", "baselined", "blocking")}
    messages = []
    for item in diagnostics:
        path = item.get("path")
        # Policy paths may point outside the candidate checkout.
        if path and (Path(path).is_absolute() or ".." in Path(path).parts):
            path = None
        messages.append(("error", f'{item["kind"]}: {item["message"]}', path,
                         item.get("range", {}).get("start")))
    for status in ("blocking", "reported", "baselined"):
        for item in findings:
            if item["status"] != status:
                continue
            message = (f'{item["name"]} has cyclomatic complexity {item["score"]} '
                       f'(limit {item["limit"]}, {status})')
            messages.append(("error" if status == "blocking" else "warning",
                             message, item["path"], item["range"]["start"]))
    for level, message, path, location in messages[:ANNOTATION_LIMIT]:
        file_path = (relative_repository / path).as_posix() if path else None
        annotation(level, message, file_path, location)

    outcome = {0: "passed", 1: "violations", 2: "analysis failed"}[code]
    summary = [
        f"## {CHECK}\n",
        f"Outcome: **{outcome}** · Mode: **{report['mode'] or 'unavailable'}**",
        f"Files scanned: {report['files_scanned']} · Files skipped: {report['files_skipped']}",
        f"Analysis errors: {len(diagnostics)} · Reported: {totals['reported']} · "
        f"Baselined: {totals['baselined']} · Blocking: {totals['blocking']}",
    ]
    if messages:
        summary.append("")
    for _, message, path, location in messages[:SUMMARY_LIMIT]:
        prefix = f"{path}:{location['line']}" if path and location else path
        display = f"{prefix} — {message}" if prefix else message
        summary.append(f"- {text_for_summary(display[:500])}")
    if len(messages) > SUMMARY_LIMIT:
        summary.append(f"\nShowing {SUMMARY_LIMIT} of {len(messages)} diagnostics and findings.")
    append_env_file("GITHUB_STEP_SUMMARY", "\n".join(summary) + "\n")


def main(binary):
    try:
        workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
        repository = repository_path(workspace)
        with tempfile.TemporaryDirectory(prefix="lint-action-policy-", dir=os.environ["RUNNER_TEMP"]) as directory:
            policy = trusted_directory(workspace, repository, Path(directory))
            command = [binary, "scan", "--root", repository, "--config", policy / CONFIG]
            result = subprocess.run(command, capture_output=True, check=False)
        if result.returncode not in (0, 1, 2):
            raise ValueError(f"lint-action process exited unexpectedly ({result.returncode})")
        report = json.loads(result.stdout)
        with tempfile.NamedTemporaryFile(prefix="lint-action-report-", suffix=".json",
                                         dir=os.environ["RUNNER_TEMP"], delete=False) as output:
            output.write(result.stdout)
        append_env_file("GITHUB_OUTPUT", f"report={output.name}\n")
        render(report, result.returncode, repository.relative_to(workspace))
        return result.returncode
    except (OSError, KeyError, ValueError) as error:
        annotation("error", str(error))
        append_env_file("GITHUB_STEP_SUMMARY", f"## {CHECK}\n\nOutcome: **analysis failed**\n\n"
                        f"{text_for_summary(str(error)[:500])}\n")
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
