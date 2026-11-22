"""Resolve trusted policy, run the CLI, and render GitHub workflow output."""

import html
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from urllib.parse import quote

CHECK = "lint-gate quality gate"
CONFIG = ".lint-action.json"
BASELINE = ".lint-action-baseline.json"
ANNOTATION_LIMIT = 10
SUMMARY_LIMIT = 10
PR_EVENTS = ("pull_request", "pull_request_review", "pull_request_review_comment")
OBJECT_ID = r"[0-9a-f]{40}|[0-9a-f]{64}"


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


def git_command(repository):
    return ["git", "--no-optional-locks", "--no-replace-objects", "-C", repository]


def git(repository, *arguments):
    return subprocess.run(
        [*git_command(repository), *arguments],
        check=False,
        capture_output=True,
    )


def copy_base_policy(repository, revision, directory):
    if not re.fullmatch(OBJECT_ID, revision):
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


def materialize_base(repository, revision, directory):
    if not revision or not re.fullmatch(OBJECT_ID, revision):
        raise ValueError("an exact pull request base SHA is unavailable")
    if git(repository, "cat-file", "-e", f"{revision}^{{commit}}").returncode:
        raise ValueError(f"cannot read PR base {revision}; check out with fetch-depth: 0")
    entries = git(repository, "ls-tree", "-rz", revision)
    if entries.returncode:
        raise ValueError(f"cannot list PR base {revision}")

    # Read committed bytes directly so checkout filters and archive attributes
    # cannot change the source selection. The scanner skips symlinks and Git metadata.
    directory.mkdir()
    with subprocess.Popen([*git_command(repository), "cat-file", "--batch"],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE) as blobs:
        for entry in entries.stdout.split(b"\0")[:-1]:
            metadata, name = entry.split(b"\t", 1)
            mode, _, object_id = metadata.split()
            if mode not in (b"100644", b"100755"):
                continue
            source_path = os.fsdecode(name)
            relative = Path(source_path)
            if (relative.anchor or relative.as_posix() != source_path
                    or any(part in ("..", ".git") for part in relative.parts)):
                raise ValueError("the PR base contains an invalid source path")
            target = directory / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            blobs.stdin.write(object_id + b"\n")
            blobs.stdin.flush()
            header = blobs.stdout.readline().split()
            if len(header) != 3 or header[:2] != [object_id, b"blob"]:
                raise ValueError(f"cannot read a source blob from PR base {revision}")
            with target.open("wb") as output:
                remaining = int(header[2])
                while remaining:
                    chunk = blobs.stdout.read(min(remaining, 1024**2))
                    if not chunk:
                        raise ValueError(f"incomplete source blob from PR base {revision}")
                    output.write(chunk)
                    remaining -= len(chunk)
            if blobs.stdout.read(1) != b"\n":
                raise ValueError(f"invalid source blob from PR base {revision}")
        blobs.stdin.close()
        if blobs.wait():
            raise ValueError(f"cannot finish reading PR base {revision}")


def candidate_revision(repository):
    head = git(repository, "rev-parse", "--verify", "HEAD")
    if head.returncode:
        return None
    status = git(repository, "status", "--porcelain", "--untracked-files=all", "--ignored=matching")
    if status.returncode or status.stdout:
        return None
    return head.stdout.decode("ascii").strip()


def repository_path(workspace):
    selection = Path(os.environ.get("LINT_GATE_REPOSITORY", "."))
    repository = (workspace / selection).resolve()
    if selection.is_absolute() or not repository.is_relative_to(workspace) or not repository.is_dir():
        raise ValueError("repository must be a directory within GITHUB_WORKSPACE")
    return repository


def trusted_directory(workspace, repository, temporary):
    event = os.environ.get("GITHUB_EVENT_NAME", "")
    if event == "pull_request_target":
        raise ValueError("use the pull_request event for candidate source analysis")
    directory_input = os.environ.get("LINT_GATE_POLICY_DIRECTORY")
    revision = os.environ.get("LINT_GATE_POLICY_REF")
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
    revision = revision or os.environ.get("LINT_GATE_PR_BASE")
    if event in PR_EVENTS and not revision:
        raise ValueError("pull request base SHA is unavailable")
    if revision:
        copy_base_policy(repository, revision, temporary)
        return temporary
    return repository


def text_for_summary(value):
    quoted = json.dumps(value, ensure_ascii=False) if isinstance(value, str) else str(value)
    return "<code>" + html.escape(quoted) + "</code>"


def format_occurrence(occurrence):
    start, end = (occurrence["range"][point] for point in ("start", "end"))
    return (f'{occurrence["path"]}:{start["line"]}:{start["column"]}'
            f'-{end["line"]}:{end["column"]}')


def source_link(item, revision, repository_url, label=None):
    path = item["path"]
    start, end = (item["range"][point]["line"] for point in ("start", "end"))
    display = text_for_summary(label or format_occurrence(item))
    if not revision or not repository_url:
        return display
    fragment = f"L{start}" if start == end else f"L{start}-L{end}"
    url = f"{repository_url}/blob/{revision}/{quote(path)}#{fragment}"
    return f'<a href="{html.escape(url, quote=True)}">{display}</a>'


def change_summary(change, comparison, repository_url):
    previous, current = (change[key] for key in ("previous", "current"))
    active = current or previous
    score = str(active["score"])
    if previous and current:
        score = f'{previous["score"]} → {current["score"]}'
    limit_label = "minimum" if change["rule"] == "source-duplication" else "limit"
    status = active["status"] or "passing"
    if change["match_status"] != "matched":
        status += (", ambiguous identity" if change["match_status"] == "ambiguous"
                   else ", no matched finding")
    locations = []
    for side, item in (("base", previous), ("candidate", current)):
        if item is None:
            continue
        revision = comparison[f"{side}_revision"]
        if "occurrences" in item:
            location = " and ".join(source_link(part, revision, repository_url)
                                    for part in item["occurrences"])
        else:
            location = source_link(item, revision, repository_url,
                                   f'{item["name"]} at {format_occurrence(item)}')
        locations.append(f"{side}: {location}")
    return (f'- {change["rule"]} {score} ({limit_label} {change["limit"]}, {status})'
            f' · {" · ".join(locations)}')


def comparison_summary(comparison, relative_repository, base_error):
    lines = ["", "### Pull request finding comparison", ""]
    for side in ("base", "candidate"):
        if revision := comparison[f"{side}_revision"]:
            lines.append(f"{side.capitalize()} revision: {text_for_summary(revision)}")
    if comparison["status"] != "complete":
        reasons = {
            "candidate_analysis_failed": "Candidate analysis failed.",
            "base_analysis_failed": "Base analysis failed.",
            "base_root_unavailable": "The exact base source tree is unavailable.",
        }
        lines.append(f'**Comparison unavailable.** {reasons[comparison["reason"]]}')
        if base_error:
            lines.append(f"- {text_for_summary(base_error[:500])}")
        for diagnostic in comparison["base_diagnostics"][:SUMMARY_LIMIT]:
            detail = f'{diagnostic["kind"]}: {diagnostic["message"]}'
            if path := diagnostic.get("path"):
                detail = f"{path} — {detail}"
            lines.append(f"- {text_for_summary(detail[:500])}")
        return lines

    lines.append(f'Unchanged findings: **{comparison["unchanged"]}**')
    server = os.environ.get("GITHUB_SERVER_URL", "").rstrip("/")
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    repository_url = None
    if relative_repository == Path(".") and server and re.fullmatch(r"[\w.-]+/[\w.-]+", repository):
        repository_url = f"{server}/{repository}"
    changes = comparison["changes"]
    for category in ("new", "worsened", "improved", "fixed", "removed"):
        items = [change for change in changes if change["category"] == category]
        if items:
            lines.extend(["", f"#### {category.capitalize()} ({len(items)})", ""])
            lines.extend(change_summary(item, comparison, repository_url)
                         for item in items[:SUMMARY_LIMIT])
            if len(items) > SUMMARY_LIMIT:
                lines.append(f"\nShowing {SUMMARY_LIMIT} of {len(items)} {category} findings.")
    return lines


def render(report, code, relative_repository, base_error=None):
    findings = report["findings"]
    diagnostics = report["diagnostics"]
    totals = {status: sum(item["status"] == status for item in findings)
              for status in ("reported", "baselined", "blocking")}
    severities = {level: sum(item.get("severity", "error") == level for item in findings)
                  for level in ("warn", "error")}
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
            rule = item["rule"]
            severity = item.get("severity", "error")
            if rule == "source-duplication":
                primary, secondary = item["occurrences"]
                message = (f'{severity} {rule}: {item["score"]} shared tokens (minimum {item["limit"]}, {status}) '
                           f'at {format_occurrence(primary)} and {format_occurrence(secondary)}')
            else:
                primary = item
                rule = rule.replace("-", " ")
                message = (f'{item["name"]} has {severity} {rule} {item["score"]} '
                           f'(limit {item["limit"]}, {status})')
                contributions = sorted(item.get("contributions", []), key=lambda part: -part["points"])
                if contributions:
                    details = ", ".join(
                        f'+{part["points"]} {part["construct"]} at line {part["range"]["start"]["line"]}'
                        for part in contributions[:3]
                    )
                    message += f". Largest contributions: {details}"
            messages.append(("error" if status == "blocking" else "warning",
                             message, primary["path"], primary["range"]["start"]))
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
        f"Warning severity: {severities['warn']} · Error severity: {severities['error']}",
    ]
    comparison = report.get("comparison")
    if comparison:
        summary.extend(comparison_summary(comparison, relative_repository, base_error))
        if comparison["status"] == "complete":
            messages = []
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
    base_revision = os.environ.get("LINT_GATE_PR_BASE")
    comparing = os.environ.get("GITHUB_EVENT_NAME") in PR_EVENTS or bool(base_revision)
    base_error = None
    try:
        workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve()
        repository = repository_path(workspace)
        with tempfile.TemporaryDirectory(prefix="lint-gate-policy-", dir=os.environ["RUNNER_TEMP"]) as directory:
            temporary = Path(directory)
            policy = trusted_directory(workspace, repository, temporary)
            command = [binary, "compare" if comparing else "scan",
                       "--root", repository, "--config", policy / CONFIG]
            if comparing:
                if base_revision and re.fullmatch(OBJECT_ID, base_revision):
                    command.extend(["--base-revision", base_revision])
                if revision := candidate_revision(repository):
                    command.extend(["--candidate-revision", revision])
                base_root = temporary / "base"
                try:
                    materialize_base(repository, base_revision, base_root)
                    command.extend(["--base-root", base_root])
                except (OSError, ValueError) as error:
                    base_error = str(error)
            result = subprocess.run(command, capture_output=True, check=False)
        if result.returncode not in (0, 1, 2):
            raise ValueError(f"lint-gate process exited unexpectedly ({result.returncode})")
        report = json.loads(result.stdout)
        with tempfile.NamedTemporaryFile(prefix="lint-gate-report-", suffix=".json",
                                         dir=os.environ["RUNNER_TEMP"], delete=False) as output:
            output.write(result.stdout)
        append_env_file("GITHUB_OUTPUT", f"report={output.name}\n")
        render(report, result.returncode, repository.relative_to(workspace), base_error)
        append_env_file("GITHUB_OUTPUT", f"outcome={result.returncode}\n")
        return result.returncode
    except (OSError, KeyError, ValueError) as error:
        annotation("error", str(error))
        comparison_note = "\n\n**Comparison unavailable.**" if comparing else ""
        append_env_file("GITHUB_STEP_SUMMARY", f"## {CHECK}\n\nOutcome: **analysis failed**"
                        f"{comparison_note}\n\n{text_for_summary(str(error)[:500])}\n")
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
