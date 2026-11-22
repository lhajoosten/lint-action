"""Publish an advisory check from a completed pull_request workflow."""

from io import BytesIO
import json
import os
from pathlib import Path
import re
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener, urlopen
from zipfile import BadZipFile, ZipFile
from zlib import error as CompressionError

from app_report import REPORT_LIMIT, REPORT_NAME, SAMPLE_LIMIT, STATUSES

CHECK = "Lint Gate (advisory)"
CHECK_KEY = "lint-gate-advisory-v1"
SHA = r"[0-9a-f]{40}"
REPO = r"[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+"
CANDIDATE = rf"Record candidate ([1-9][0-9]*) ({SHA}) ({SHA})"


class Skip(Exception):
    """No safely attributable check can be published."""


class ApiError(Exception):
    def __init__(self, operation, status):
        self.status = status
        super().__init__(f"GitHub rejected {operation} (HTTP {status}).")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, newurl):
        return None


class GitHub:
    def __init__(self, base_url, repository, token):
        self.root = f"{base_url}/repos/{repository}"
        self.token = token

    def request(self, path, method="GET", payload=None):
        request = Request(
            self.root + path,
            data=json.dumps(payload).encode() if payload is not None else None,
            headers={
                "Authorization": f"Bearer {self.token}",
                "Accept": "application/vnd.github+json",
                "Content-Type": "application/json",
                "X-GitHub-Api-Version": "2022-11-28",
            },
            method=method,
        )
        return build_opener(NoRedirect).open(request, timeout=20)

    def json(self, path, method="GET", payload=None):
        try:
            with self.request(path, method, payload) as response:
                data = response.read(4 * 1024**2 + 1)
            if len(data) > 4 * 1024**2:
                raise ValueError("GitHub API response exceeded its size limit.")
            return json.loads(data)
        except HTTPError as error:
            raise ApiError(f"{method} {path}", error.code) from None

    def pages(self, path, key):
        separator = "&" if "?" in path else "?"
        for page in range(1, 21):
            result = self.json(f"{path}{separator}per_page=100&page={page}")
            yield from result[key]
            if len(result[key]) < 100 or page * 100 >= result["total_count"]:
                return
        raise ValueError("GitHub API result exceeded its pagination limit.")

    def artifact(self, artifact_id):
        # GitHub redirects to a short-lived signed URL. Only the API gets the token.
        try:
            self.request(f"/actions/artifacts/{artifact_id}/zip").close()
        except HTTPError as error:
            with error:
                location = error.headers.get("Location")
                if error.code != 302 or not location or urlsplit(location).scheme != "https":
                    raise ApiError("report download", error.code) from None
            with urlopen(location, timeout=20) as response:
                return response.read(REPORT_LIMIT + 1)
        raise ValueError("GitHub did not return a signed report download URL.")


def identity(run):
    return tuple(run[key] for key in ("id", "run_attempt", "workflow_id", "head_sha", "event"))


def source_matches(reference, configured):
    path, ref = configured.rsplit("@", 1)
    if re.fullmatch(SHA, ref):
        return reference.get("path") == configured and reference.get("sha") == ref
    return (
        ref.startswith("refs/heads/")
        and reference.get("path") in (configured, f"{path}@{ref.removeprefix('refs/heads/')}")
        and reference.get("ref") == ref
        and re.fullmatch(SHA, reference.get("sha", "")) is not None
    )


def verified_run(api, event, workflow_file, source, repository_id):
    incoming = event["workflow_run"]
    run_id, attempt, *_ = identity(incoming)
    if not all(type(number) is int and number > 0 for number in (run_id, attempt)):
        raise Skip("The completed event has no usable run identity.")
    workflow = api.json(f"/actions/workflows/{workflow_file}")
    run = api.json(f"/actions/runs/{run_id}")
    if run["run_attempt"] != attempt or run["status"] != "completed":
        raise Skip("The source run has advanced to a different attempt.")
    if (
        identity(incoming) != identity(run)
        or event["repository"]["id"] != repository_id
        or run["repository"]["id"] != repository_id
        or run["workflow_id"] != workflow["id"]
        or run["path"].split("@", 1)[0] != f".github/workflows/{workflow_file}"
        or run["event"] != "pull_request"
    ):
        raise Skip("The completed run is outside the configured PR analysis workflow.")
    references = run.get("referenced_workflows") or []
    if len([reference for reference in references if source_matches(reference, source)]) != 1:
        raise Skip("The run did not identify the configured reusable scanner revision.")
    return run


def candidate_job(api, run, job_name):
    path = f"/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs"
    jobs = [job for job in api.pages(path, "jobs") if job["name"] == job_name]
    if len(jobs) != 1:
        raise Skip("The source attempt did not identify a single analysis job.")
    job = jobs[0]
    if job["run_id"] != run["id"] or job.get("run_attempt", run["run_attempt"]) != run["run_attempt"]:
        raise Skip("The analysis job belongs to a different run attempt.")
    steps = {step["name"]: step["conclusion"] for step in job["steps"]}
    candidates = [re.fullmatch(CANDIDATE, name) for name, conclusion in steps.items()
                  if conclusion == "success" and name.startswith("Record candidate ")]
    if len(candidates) != 1 or candidates[0] is None:
        raise Skip("The analysis job did not record a verifiable candidate.")
    number, revision, base = candidates[0].groups()
    return int(number), revision, base, steps


def target_head(api, run, number, revision, base, repository_id):
    pr = api.json(f"/pulls/{number}")
    head = pr["head"]
    head_repository = head["repo"]
    run_head = run.get("head_repository")
    if (
        pr["number"] != number
        or pr["state"] != "open"
        or pr["base"]["repo"]["id"] != repository_id
        or pr["base"]["sha"] != base
        or pr["merge_commit_sha"] != revision
        or not head_repository
        or not run_head
        or head_repository["id"] != run_head["id"]
        or head["ref"] != run["head_branch"]
        or not re.fullmatch(SHA, head["sha"])
        or run["head_sha"] not in (head["sha"], revision)
    ):
        raise Skip("The recorded synthetic merge no longer matches the open PR and source run.")
    # The synthetic merge and both parents must match GitHub's current PR metadata.
    commit = api.json(f"/git/commits/{revision}")
    if commit["sha"] != revision or [parent["sha"] for parent in commit["parents"]] != [base, head["sha"]]:
        raise Skip("GitHub could not bind the synthetic merge to the PR head and base.")
    return head["sha"]


def scan_outcome(steps):
    if steps.get("Record policy bootstrap") == "success" and steps.get("Scan source") == "skipped":
        return "bootstrap"
    if steps.get("Scan source") == "success":
        return "passed"
    if steps.get("Scan source") == "failure" and steps.get("Record policy violations") == "success":
        return "blocking"
    return "incomplete"


def unpack_report(archive):
    if len(archive) > REPORT_LIMIT:
        raise ValueError("Report archive exceeds the size limit.")
    with ZipFile(BytesIO(archive)) as package:
        entries = package.infolist()
        if len(entries) != 1 or entries[0].filename != REPORT_NAME or entries[0].file_size > REPORT_LIMIT:
            raise ValueError("Report archive has an unexpected file or size.")
        with package.open(entries[0]) as stream:
            data = stream.read(REPORT_LIMIT + 1)
    if len(data) > REPORT_LIMIT:
        raise ValueError("Report exceeds the size limit.")
    return json.loads(data)


def validate_report(report, run, revision, outcome):
    if not isinstance(report, dict) or (report.get("version"), report.get("run_id"),
                                       report.get("attempt"), report.get("revision")) != (
        1, run["id"], run["run_attempt"], revision
    ):
        raise ValueError("Report identity mismatch.")
    counts = report["counts"]
    if not isinstance(counts, dict) or set(counts) != set(STATUSES):
        raise ValueError("Invalid finding counts.")
    for count in (report["files"], report["errors"], report["version"], report["run_id"],
                  report["attempt"], *counts.values()):
        if type(count) is not int or not 0 <= count <= 2**63 - 1:
            raise ValueError("Invalid report count or identity.")
    samples = report["samples"]
    if not isinstance(samples, list) or len(samples) > SAMPLE_LIMIT or not all(
        isinstance(sample, str) and len(sample) <= 300 for sample in samples
    ):
        raise ValueError("Invalid report samples.")
    if outcome in ("passed", "blocking") and (
        report["errors"] or bool(counts["blocking"]) != (outcome == "blocking")
    ):
        raise ValueError("Report disagrees with the recorded scan outcome.")
    return report


def load_report(api, run, revision, outcome):
    if outcome == "bootstrap":
        return None, None
    name = f"lint-gate-report-{run['id']}-{run['run_attempt']}"
    query = urlencode({"name": name})
    path = f"/actions/runs/{run['id']}/artifacts?{query}"
    try:
        artifacts = [item for item in api.pages(path, "artifacts") if item["name"] == name]
        if not artifacts:
            return None, "The analysis report is missing."
        artifact, = artifacts
        metadata = artifact["workflow_run"]
        if (
            artifact["expired"]
            or type(artifact["id"]) is not int or artifact["id"] <= 0
            or not 0 < artifact["size_in_bytes"] <= REPORT_LIMIT
            or (metadata["id"], metadata["repository_id"], metadata["head_sha"])
            != (run["id"], run["repository"]["id"], run["head_sha"])
        ):
            raise ValueError("Artifact metadata mismatch.")
        return validate_report(unpack_report(api.artifact(artifact["id"])), run, revision, outcome), None
    except (ApiError, BadZipFile, CompressionError, EOFError, KeyError, OSError,
            RecursionError, RuntimeError, TypeError, ValueError, NotImplementedError):
        return None, "The analysis report is unusable."


def check_output(outcome, report, report_error, revision, target, details_url):
    title, conclusion = {
        "bootstrap": ("Policy bootstrap", "skipped"),
        "passed": ("Clean", "success"),
        "blocking": ("Blocking policy findings", "neutral"),
        "incomplete": ("Analysis incomplete", "neutral"),
    }[outcome]
    summary = [f"[Open the analysis run]({details_url})",
               f"PR head `{target}` · GitHub synthetic merge tree `{revision}`"]
    if outcome == "bootstrap":
        summary.append("The PR base has no Lint Gate policy yet. The scan was skipped for bootstrap.")
    if report_error:
        summary.append(report_error)
        if outcome == "passed":
            title, conclusion = "Report unavailable", "neutral"
    if report:
        counts = report["counts"]
        if outcome == "passed" and (counts["reported"] or counts["baselined"]):
            title, conclusion = "Warnings", "neutral"
        summary.append(
            f'Files scanned {report["files"]} · Analysis errors {report["errors"]} · '
            f'Reported {counts["reported"]} · Baselined {counts["baselined"]} · Blocking {counts["blocking"]}'
        )
        if report["samples"]:
            samples = [json.dumps(sample).replace("`", r"\u0060") for sample in report["samples"]]
            summary.append("```text\n" + "\n".join(samples) + "\n```")
    return {
        "name": CHECK, "status": "completed", "conclusion": conclusion, "details_url": details_url,
        "output": {"title": title, "summary": "\n\n".join(summary)},
    }


def upsert(api, app_id, run, target, payload):
    prefix = f"{CHECK_KEY}:{run['repository']['id']}:{target}:"
    order = (run["run_number"], run["run_attempt"])
    query = urlencode({"check_name": CHECK, "app_id": app_id, "filter": "all"})
    matches = []
    for check in api.pages(f"/commits/{target}/check-runs?{query}", "check_runs"):
        if check["name"] != CHECK or check["head_sha"] != target or check["app"]["id"] != app_id:
            continue
        external_id = check.get("external_id") or ""
        version = re.fullmatch(r"([0-9]+):([0-9]+):([0-9]+)", external_id.removeprefix(prefix))
        if external_id.startswith(prefix) and version:
            number, attempt, run_id = map(int, version.groups())
            matches.append(((number, attempt), run_id, check["id"]))

    version = f"{order[0]}:{order[1]}:{run['id']}"
    payload = {**payload, "external_id": prefix + version}
    if matches:
        recorded, recorded_run, check_id = max(matches)
        if recorded > order or recorded == order and recorded_run == run["id"]:
            return "The advisory check already holds this result or a newer one."
        check = api.json(f"/check-runs/{check_id}", "PATCH", payload)
    else:
        check = api.json("/check-runs", "POST", {**payload, "head_sha": target})
    if (check["head_sha"], check["app"]["id"], check["name"]) != (target, app_id, CHECK):
        raise ValueError("GitHub returned a check with an unexpected App, name, or commit.")
    return f"Published {CHECK} on PR head {target}."


def publish(api, checks, event, workflow_file, source, job_name, repository_id, app_id, repository_url):
    run = verified_run(api, event, workflow_file, source, repository_id)
    number, revision, base, steps = candidate_job(api, run, job_name)
    try:
        target = target_head(api, run, number, revision, base, repository_id)
    except ApiError as error:
        if error.status == 404:
            raise Skip("GitHub can no longer resolve the PR or its synthetic merge.") from None
        raise
    outcome = scan_outcome(steps)
    report, report_error = load_report(api, run, revision, outcome)
    details_url = f"{repository_url}/actions/runs/{run['id']}/attempts/{run['run_attempt']}"
    payload = check_output(outcome, report, report_error, revision, target, details_url)
    try:
        return upsert(checks, app_id, run, target, payload)
    except ApiError as error:
        if error.status in (404, 422):
            raise Skip(f"GitHub could not attach the check to this PR head. {error} "
                       "Verify the installation has Checks write for this repository.") from None
        raise


def main():
    env = os.environ
    repository, source, workflow = (env[key] for key in ("GITHUB_REPOSITORY", "SCANNER_WORKFLOW", "SOURCE_WORKFLOW"))
    if not (
        re.fullmatch(REPO, repository)
        and re.fullmatch(r"[\w.-]+\.ya?ml", workflow, re.ASCII)
        and re.fullmatch(rf"{REPO}/\.github/workflows/source-complexity\.yml@(?:{SHA}|refs/heads/[\w./-]+)", source)
    ):
        raise SystemExit("Invalid analysis workflow configuration. Check the publisher inputs.")
    api = GitHub(env["GITHUB_API_URL"], repository, env["GITHUB_TOKEN"])
    checks = GitHub(env["GITHUB_API_URL"], repository, env["APP_TOKEN"])
    try:
        message = publish(
            api, checks, json.loads(Path(env["GITHUB_EVENT_PATH"]).read_bytes()),
            workflow, source, env["ANALYSIS_JOB"], int(env["GITHUB_REPOSITORY_ID"]),
            int(env["APP_ID"]), f"{env['GITHUB_SERVER_URL']}/{repository}",
        )
    except Skip as error:
        message = f"Skipped App publication. {error}"
    except ApiError as error:
        raise SystemExit(f"{error} Verify the workflow read permissions and the App installation's Checks write permission.")
    except (URLError, TimeoutError):
        raise SystemExit("GitHub could not be reached. Rerun the publisher to retry.")
    except (KeyError, TypeError, ValueError):
        raise SystemExit("Unexpected GitHub metadata or publishing configuration. Verify the publisher inputs.")
    print(message)


if __name__ == "__main__":
    main()
