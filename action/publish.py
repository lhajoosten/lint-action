"""Publish PR conversations from an independently verified pull_request scan."""

import hashlib
import json
import os
from pathlib import Path
import re
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import HTTPRedirectHandler, Request, build_opener

SHA = r"[0-9a-f]{40}"
REPO = r"[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+"
CANDIDATE = rf"Record candidate ([1-9][0-9]*) ({SHA}) ({SHA})"
MAX_INLINE_THREADS = 50
MAX_SUMMARY_FINDINGS = 50


class Skip(Exception):
    """No safely attributable conversation can be published."""


class ApiError(Exception):
    def __init__(self, operation, status):
        self.status = status
        super().__init__(f"GitHub rejected {operation} (HTTP {status}).")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, newurl):
        return None


class GitHub:
    def __init__(self, base_url, repository, token):
        self.base_url = base_url.rstrip("/")
        self.root = f"{base_url}/repos/{repository}"
        self.token = token

    def request(self, path, method="GET", payload=None, api_root=False):
        request = Request(
            (self.base_url if api_root else self.root) + path,
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

    def json_api(self, path, method="GET", payload=None):
        try:
            with self.request(path, method, payload, api_root=True) as response:
                data = response.read(4 * 1024**2 + 1)
            if len(data) > 4 * 1024**2:
                raise ValueError("GitHub API response exceeded its size limit.")
            return json.loads(data)
        except HTTPError as error:
            raise ApiError(f"{method} {path}", error.code) from None

    def graphql(self, query, variables=None):
        request = Request(
            f"{self.base_url}/graphql",
            data=json.dumps({"query": query, "variables": variables or {}}).encode(),
            headers={
                "Authorization": f"Bearer {self.token}",
                "Accept": "application/vnd.github+json",
                "Content-Type": "application/json",
                "X-GitHub-Api-Version": "2022-11-28",
            },
            method="POST",
        )
        try:
            with build_opener(NoRedirect).open(request, timeout=20) as response:
                data = response.read(4 * 1024**2 + 1)
            if len(data) > 4 * 1024**2:
                raise ValueError("GitHub API response exceeded its size limit.")
            result = json.loads(data)
            if result.get("errors"):
                errors = result["errors"]
                if any("not accessible" in error.get("message", "").lower()
                       or (error.get("extensions") or {}).get("type") == "FORBIDDEN"
                       for error in errors):
                    raise ApiError("review-thread operation", 403)
                raise ValueError("GitHub rejected a review-thread operation.")
            return result["data"]
        except HTTPError as error:
            raise ApiError("GraphQL review-thread operation", error.code) from None

    def pages(self, path, key):
        separator = "&" if "?" in path else "?"
        for page in range(1, 21):
            result = self.json(f"{path}{separator}per_page=100&page={page}")
            items = result if isinstance(result, list) else result[key]
            yield from items
            if len(items) < 100:
                return
            if isinstance(result, dict) and page * 100 >= result.get("total_count", page * 100):
                return
        raise ValueError("GitHub API result exceeded its pagination limit.")

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


def verified_scan_target(api, event, workflow_file, source, job_name, repository_id):
    """Bind the independent scan to the current PR merge and trusted source run."""
    run = verified_run(api, event, workflow_file, source, repository_id)
    number, revision, base, steps = candidate_job(api, run, job_name)
    target = target_head(api, run, number, revision, base, repository_id)
    path = source.rsplit("@", 1)[0]
    references = [reference for reference in run.get("referenced_workflows", [])
                  if source_matches(reference, source)]
    scanner_sha = references[0]["sha"]
    scanner_repository = path.split("/.github/workflows/", 1)[0]
    return run, number, revision, base, target, scanner_repository, scanner_sha, steps


def scan_outcome(steps):
    if steps.get("Record policy bootstrap") == "success" and steps.get("Scan source") == "skipped":
        return "bootstrap"
    if steps.get("Scan source") == "success":
        return "passed"
    if steps.get("Scan source") == "failure" and steps.get("Record policy violations") == "success":
        return "blocking"
    return "incomplete"


def validate_scan_report(path):
    if Path(path).stat().st_size > 16 * 1024 * 1024:
        raise ValueError("Trusted scan report exceeded its size limit.")
    report = json.loads(Path(path).read_bytes())
    if (not isinstance(report, dict) or type(report.get("format_version")) is not int
            or report["format_version"] not in (1, 2)
            or type(report.get("scoring_version")) is not int
            or type(report.get("files_scanned")) is not int
            or report["files_scanned"] < 0 or report.get("diagnostics") != []
            or not isinstance(report.get("findings"), list)):
        raise ValueError("Trusted scan report is incomplete or invalid.")
    for finding in report["findings"]:
        if (not isinstance(finding, dict)
                or finding.get("status") not in ("reported", "baselined", "blocking")
                or not isinstance(finding.get("rule"), str)
                or type(finding.get("score")) is not int
                or type(finding.get("limit")) is not int):
            raise ValueError("Trusted scan report contains an invalid finding.")
        severity = finding.get("severity")
        if (report["format_version"] >= 2 and severity not in ("error", "warn", "pass")
                or severity is not None and severity not in ("error", "warn", "pass")
                or finding["status"] == "blocking" and severity not in (None, "error")
                or finding["status"] == "baselined" and severity not in (None, "error")
                or finding["status"] == "reported" and severity == "pass"):
            raise ValueError("Trusted scan report contains an invalid severity disposition.")
        if finding["rule"] == "source-duplication":
            if not isinstance(finding.get("occurrences"), list) or len(finding["occurrences"]) != 2:
                raise ValueError("Trusted scan report contains an invalid duplication finding.")
        elif not all(key in finding for key in ("path", "range", "name", "scope", "language")):
            raise ValueError("Trusted scan report contains an invalid complexity finding.")
    return report


def code(value):
    if isinstance(value, str) and len(value) > 200:
        value = value[:197] + "..."
    safe = json.dumps(value, ensure_ascii=False)
    safe = safe.replace("`", r"\u0060").replace("@", r"\u0040").replace("<", r"\u003c")
    return "`" + safe + "`"


def finding_identity(finding):
    if finding["rule"] == "source-duplication":
        identities = [
            {"path": occurrence.get("path"), "range": occurrence.get("range")}
            for occurrence in finding["occurrences"]
        ]
        payload = {"rule": finding["rule"], "occurrences": sorted(identities, key=lambda x: json.dumps(x, sort_keys=True))}
    else:
        payload = {key: finding.get(key) for key in ("rule", "path", "language", "scope", "name")}
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest(), payload


def line_anchor(finding, files, compatible_paths=None):
    locations = finding["occurrences"] if finding["rule"] == "source-duplication" else [finding]
    for location in locations:
        path = location.get("path")
        if compatible_paths is not None and path not in compatible_paths:
            continue
        start = location.get("range", {}).get("start", {}).get("line")
        if (not isinstance(path, str) or path.startswith("/") or "\\" in path
                or any(part in ("", ".", "..") for part in path.split("/"))
                or type(start) is not int or start < 1):
            continue
        for file in files:
            if file.get("filename") != path or file.get("status") == "removed":
                continue
            patch = file.get("patch")
            if not isinstance(patch, str):
                break
            current = 0
            for line in patch.splitlines():
                match = re.match(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", line)
                if match:
                    current = int(match.group(1))
                    count = int(match.group(2) or "1")
                    if current <= start < current + count:
                        return path, start
                elif line.startswith("+") and not line.startswith("+++"):
                    current += 1
                elif line.startswith(" "):
                    current += 1
            break
    return None


def review_anchor(finding, files, compatible_paths=None):
    line = line_anchor(finding, files, compatible_paths)
    if line:
        return ("line", *line)
    locations = finding["occurrences"] if finding["rule"] == "source-duplication" else [finding]
    for location in locations:
        path = location.get("path")
        if (isinstance(path, str) and not path.startswith("/") and "\\" not in path
                and not any(part in ("", ".", "..") for part in path.split("/"))
                and any(file.get("filename") == path and file.get("status") != "removed" for file in files)):
            return ("file", path)
    return None


def verified_app_actor(api, app_slug):
    if not app_slug:
        return None
    login = f"{app_slug}[bot]"
    actor = api.json_api(f"/users/{quote(login, safe='')}")
    if actor.get("login") != login or actor.get("type") != "Bot" or type(actor.get("id")) is not int:
        raise ValueError("GitHub did not resolve the configured App bot actor.")
    return actor["id"]


def owned(comment, app_id, app_slug, app_actor_id):
    app = comment.get("performed_via_github_app") or {}
    if app.get("id") == app_id:
        return True
    user = comment.get("user") or {}
    return bool(app_actor_id and user.get("type") == "Bot" and user.get("id") == app_actor_id
                and user.get("login") == f"{app_slug}[bot]")


def thread_data(api, number):
    owner, name = os.environ["GITHUB_REPOSITORY"].split("/", 1)
    query = """query($owner:String!, $name:String!, $number:Int!, $cursor:String) {
      repository(owner:$owner, name:$name) {
        pullRequest(number:$number) {
          reviewThreads(first:100, after:$cursor) {
            nodes { id isResolved comments(first:100) { nodes { databaseId body } } }
            pageInfo { hasNextPage endCursor }
          }
        }
      }
    }"""
    cursor = None
    for _ in range(20):
        data = api.graphql(query, {"owner": owner, "name": name, "number": number, "cursor": cursor})
        connection = data["repository"]["pullRequest"]["reviewThreads"]
        yield from connection["nodes"]
        if not connection["pageInfo"]["hasNextPage"]:
            return
        cursor = connection["pageInfo"]["endCursor"]
    raise ValueError("Review threads exceeded their pagination limit.")


def set_thread_resolved(api, thread_id, resolved):
    mutation_name = "resolveReviewThread" if resolved else "unresolveReviewThread"
    mutation = f"""mutation($id:ID!) {{ {mutation_name}(input:{{threadId:$id}}) {{ thread {{ id isResolved }} }} }}"""
    api.graphql(mutation, {"id": thread_id})


FEEDBACK_ORDER = re.compile(r"<!-- lint-gate-feedback:v1:([0-9]+):([0-9]+):([0-9]+) -->")


def feedback_order_marker(order):
    return f"<!-- lint-gate-feedback:v1:{order[0]}:{order[1]}:{order[2]} -->"


def reconcile_feedback(api, app_id, app_slug, app_actor_id, number, head, details_url,
                       report, verify_current, compatible_paths, order):
    findings = report["findings"]
    blockers = {}
    summary_items = []
    files = list(api.pages(f"/pulls/{number}/files", "files"))
    for finding in findings:
        if finding["status"] == "baselined":
            continue
        identity, _ = finding_identity(finding)
        if finding["status"] == "reported":
            summary_items.append(("Warning", finding))
        else:
            blockers[identity] = finding

    rest_comments = list(api.pages(f"/pulls/{number}/comments", "comments"))
    issue_comments = list(api.pages(f"/issues/{number}/comments", "comments"))
    owned_rest = [comment for comment in rest_comments if owned(comment, app_id, app_slug, app_actor_id)]
    owned_issue = [comment for comment in issue_comments if owned(comment, app_id, app_slug, app_actor_id)]
    recorded_orders = [tuple(map(int, match.groups()))
                       for comment in owned_rest + owned_issue
                       for match in [FEEDBACK_ORDER.search(comment.get("body", ""))] if match]
    if recorded_orders and max(recorded_orders) > order:
        raise Skip("A newer verified scan already published PR feedback.")
    summary_comments = [comment for comment in owned_issue
                        if "<!-- lint-gate-warning-summary -->" in comment.get("body", "")]
    verify_current()
    order_marker = feedback_order_marker(order)
    if summary_comments:
        first = summary_comments[0]
        reserved_body = FEEDBACK_ORDER.sub("", first.get("body", "")).strip()
        reserved_body = f"{order_marker}\n{reserved_body}"
        if first.get("body") != reserved_body:
            first.update(api.json(f"/issues/comments/{first['id']}", "PATCH", {"body": reserved_body}))
    else:
        reserved = api.json(f"/issues/{number}/comments", "POST", {
            "body": f"{order_marker}\n<!-- lint-gate-warning-summary -->",
        })
        summary_comments = [reserved]
    by_marker = {}
    for comment in owned_rest:
        match = re.search(r"<!-- lint-gate-finding:([0-9a-f]{64}) -->", comment.get("body", ""))
        if match:
            by_marker.setdefault(match.group(1), []).append(comment)
    threads = list(thread_data(api, number))
    verify_current()
    active_by_identity = {}
    for thread in threads:
        comment_ids = {node["databaseId"] for node in thread["comments"]["nodes"]}
        marker = next((key for key, comments in by_marker.items()
                       if any(comment["id"] in comment_ids for comment in comments)), None)
        if marker:
            current = active_by_identity.get(marker)
            if current is None or current["isResolved"] and not thread["isResolved"]:
                active_by_identity[marker] = thread

    active_count = sum(
        identity in blockers and not thread["isResolved"]
        for identity, thread in active_by_identity.items()
    )
    for identity, finding in blockers.items():
        thread = active_by_identity.get(identity)
        anchor = review_anchor(finding, files, compatible_paths)
        if thread:
            thread_ids = {node["databaseId"] for node in thread["comments"]["nodes"]}
            existing = next((comment for comment in by_marker[identity]
                             if comment["id"] in thread_ids), None)
            existing_anchor = (("line", existing.get("path"), existing.get("line"))
                               if existing and existing.get("line") else
                               ("file", existing.get("path")) if existing else None)
            if anchor and existing and existing_anchor == anchor:
                body = (f"{order_marker}\n<!-- lint-gate-finding:{identity} -->\n**Lint Gate blocking finding**\n\n"
                    f"Rule {code(finding['rule'])} {code(finding.get('name', ''))} "
                    f"({code(finding.get('severity', 'error'))}) at {code(anchor[1])}"
                        f"{':' + str(anchor[2]) if anchor[0] == 'line' else ''}; score {code(finding['score'])}, "
                        f"limit {code(finding['limit'])}.\n\n[Open the verified scan]({details_url})")
                if thread["isResolved"]:
                    verify_current()
                    set_thread_resolved(api, thread["id"], False)
                if existing.get("body") != body:
                    verify_current()
                    api.json(f"/pulls/comments/{existing['id']}", "PATCH", {"body": body})
                continue
            if not anchor:
                if not thread["isResolved"]:
                    verify_current()
                    set_thread_resolved(api, thread["id"], True)
                summary_items.append(("Unanchored blocking error", finding))
                continue
            if active_count >= MAX_INLINE_THREADS:
                summary_items.append(("Blocking error (thread limit reached)", finding))
                continue
            if not thread["isResolved"]:
                verify_current()
                set_thread_resolved(api, thread["id"], True)
                active_count -= 1
        elif active_count >= MAX_INLINE_THREADS:
            summary_items.append(("Blocking error (thread limit reached)", finding))
            continue
        if anchor:
            path = anchor[1]
            body = (f"{order_marker}\n<!-- lint-gate-finding:{identity} -->\n**Lint Gate blocking finding**\n\n"
                    f"Rule {code(finding['rule'])} {code(finding.get('name', ''))} "
                    f"({code(finding.get('severity', 'error'))}) at {code(path)}"
                    f"{':' + str(anchor[2]) if anchor[0] == 'line' else ''}; score "
                    f"{code(finding['score'])}, limit {code(finding['limit'])}.\n\n"
                    f"[Open the verified scan]({details_url})")
            payload = {"body": body, "commit_id": head, "path": path}
            if anchor[0] == "line":
                payload.update(line=anchor[2], side="RIGHT")
            else:
                payload["subject_type"] = "file"
            try:
                verify_current()
                api.json(f"/pulls/{number}/comments", "POST", payload)
                active_count += 1
                continue
            except ApiError as error:
                if error.status not in (404, 422):
                    raise
        summary_items.append(("Unanchored blocking error", finding))

    for identity, thread in active_by_identity.items():
        if identity not in blockers and not thread["isResolved"]:
            verify_current()
            set_thread_resolved(api, thread["id"], True)

    parts = [order_marker, "<!-- lint-gate-warning-summary -->", "## Lint Gate findings",
             f"[Open the verified scan]({details_url})"]
    for heading, finding in summary_items[:MAX_SUMMARY_FINDINGS]:
        location = finding.get("path")
        if not location and finding.get("occurrences"):
            location = ", ".join(item.get("path", "") for item in finding["occurrences"])
        severity = finding.get("severity", "error" if finding["status"] == "blocking" else "warn")
        parts.append(f"- **{heading}** · {code(finding['rule'])} · {code(severity)} · "
                     f"{code(finding.get('name', ''))} · "
                     f"{code(location or 'location unavailable')} · "
                     f"score {code(finding['score'])}, limit {code(finding['limit'])}")
    if len(summary_items) > MAX_SUMMARY_FINDINGS:
        parts.append(f"\nShowing {MAX_SUMMARY_FINDINGS} of {len(summary_items)} findings; use the scan link for the full report.")
    if not summary_items:
        parts.append("No reported warnings or unanchored blocking findings remain.")
    payload = {"body": "\n\n".join(parts)}
    if summary_comments:
        first = summary_comments[0]
        verify_current()
        api.json(f"/issues/comments/{first['id']}", "PATCH", payload)
    else:
        verify_current()
        api.json(f"/issues/{number}/comments", "POST", payload)


def publish(api, writes, event, workflow_file, source, job_name, repository_id, app_id, repository_url):
    run, number, revision, base, target, scanner_repository, scanner_sha, steps = verified_scan_target(
        api, event, workflow_file, source, job_name, repository_id,
    )
    expected = {
        "PULL_REQUEST": str(number), "VERIFIED_REVISION": revision,
        "VERIFIED_BASE": base, "VERIFIED_HEAD": target, "SOURCE_RUN": str(run["id"]),
        "SOURCE_ATTEMPT": str(run["run_attempt"]),
        "VERIFIED_SCANNER_REPOSITORY": scanner_repository,
        "VERIFIED_SCANNER_SHA": scanner_sha,
    }
    if any(os.environ.get(key) != value for key, value in expected.items()):
        raise Skip("The verified scan target changed before publication.")
    outcome = scan_outcome(steps)
    source_outcome = os.environ.get("SOURCE_OUTCOME")
    if source_outcome != outcome:
        raise Skip("The native scan result disagreed with the verified publication job.")
    details_url = f"{repository_url}/actions/runs/{run['id']}/attempts/{run['run_attempt']}"
    if os.environ.get("DETAILS_URL") != details_url:
        raise Skip("The verified scan link did not match the current source run.")
    def verify_current():
        current = verified_scan_target(api, event, workflow_file, source, job_name, repository_id)
        current_run, current_number, current_revision, current_base, current_head, current_repository, current_sha, _ = current
        if (identity(current_run) != identity(run) or
                (current_number, current_revision, current_base, current_head,
                 current_repository, current_sha) !=
                (number, revision, base, target, scanner_repository, scanner_sha)):
            raise Skip("The PR or scanner changed before feedback was written.")

    if outcome in ("bootstrap", "incomplete"):
        verify_current()
        return "No complete verified scan is available for PR conversations."
    if outcome not in ("passed", "blocking"):
        raise Skip("The source run has no publishable analysis outcome.")
    if os.environ.get("SCAN_COMPLETE") != "true":
        verify_current()
        return "No complete verified scan is available for PR conversations."
    report = validate_scan_report(os.environ["FINDINGS_REPORT"])
    has_blocking = any(item["status"] == "blocking" for item in report["findings"])
    if has_blocking != (outcome == "blocking"):
        raise Skip("The independent scan disagreed with the native policy gate result.")

    order = (run["run_number"], run["run_attempt"], run["id"])

    manifest_path = os.environ.get("FINDINGS_MANIFEST")
    compatible_paths = None
    if manifest_path:
        manifest = json.loads(Path(manifest_path).read_bytes())
        if ((manifest.get("revision"), manifest.get("base"), manifest.get("head"),
             manifest.get("source_run"), manifest.get("source_attempt"), manifest.get("artifact_name")) !=
                (revision, base, target, str(run["id"]), str(run["run_attempt"]),
                 os.environ.get("FINDINGS_ARTIFACT_NAME"))):
            raise Skip("The source coordinate manifest does not match the verified PR target.")
        compatible_paths = set(manifest.get("compatible_paths", []))
    app_slug = os.environ.get("APP_SLUG", "")
    app_actor_id = verified_app_actor(api, app_slug)
    reconcile_feedback(writes, app_id, app_slug, app_actor_id, number, target, details_url,
                       report, verify_current, compatible_paths, order)
    return "Published PR conversations from the verified scan."


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
    writes = GitHub(env["GITHUB_API_URL"], repository, env["APP_TOKEN"])
    try:
        message = publish(
            api, writes, json.loads(Path(env["GITHUB_EVENT_PATH"]).read_bytes()),
            workflow, source, env["ANALYSIS_JOB"], int(env["GITHUB_REPOSITORY_ID"]),
            int(env["APP_ID"]), f"{env['GITHUB_SERVER_URL']}/{repository}",
        )
    except Skip as error:
        message = f"Skipped App publication. {error}"
    except ApiError as error:
        raise SystemExit(f"{error} Verify the workflow read permissions and the App installation's Pull requests write permission.")
    except (URLError, TimeoutError):
        raise SystemExit("GitHub could not be reached. Rerun the publisher to retry.")
    except (KeyError, TypeError, ValueError):
        raise SystemExit("Unexpected GitHub metadata or publishing configuration. Verify the publisher inputs.")
    print(message)


if __name__ == "__main__":
    main()
