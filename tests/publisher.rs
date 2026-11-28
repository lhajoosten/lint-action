mod common;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::{
    fs,
    process::{Command, Output},
};

const WORKFLOW: &str = "own-source-complexity.yml";
const SCANNER: &str = "owner/tool/.github/workflows/source-complexity.yml";
const JOB: &str = "complexity / complexity";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MERGE: &str = "cccccccccccccccccccccccccccccccccccccccc";
const BRANCH_SHA: &str = "dddddddddddddddddddddddddddddddddddddddd";

struct State {
    run: Value,
    pr: Value,
    bootstrap: bool,
    scan_failure: bool,
    blocking: bool,
    bad_artifact: bool,
    rejected: bool,
    check: Value,
    writes: Vec<String>,
    files: Vec<Value>,
    reviews: Vec<Value>,
    issue: Value,
    thread_updates: usize,
    stop_after_reservation: bool,
    forbid_threads: bool,
    forbid_mutation: bool,
    mutation_errors: Option<Value>,
    can_resolve: bool,
    can_reopen: bool,
    reject_review: bool,
}

impl State {
    fn new() -> Self {
        Self {
            run: json!({
                "id": 100, "run_attempt": 1, "run_number": 4, "workflow_id": 30, "event": "pull_request",
                "path": format!(".github/workflows/{WORKFLOW}@refs/pull/7/merge"), "status": "completed",
                "repository": {"id": 10}, "head_repository": {"id": 20},
                "head_sha": HEAD, "head_branch": "feature",
                "referenced_workflows": [{"path": format!("{SCANNER}@main"), "ref": "refs/heads/main", "sha": BRANCH_SHA}],
            }),
            pr: json!({
                "number": 7, "state": "open", "merge_commit_sha": MERGE,
                "head": {"repo": {"id": 20}, "sha": HEAD, "ref": "feature"},
                "base": {"repo": {"id": 10}, "sha": BASE},
            }),
            bootstrap: false,
            scan_failure: false,
            blocking: false,
            bad_artifact: false,
            rejected: false,
            check: Value::Null,
            writes: Vec::new(),
            files: Vec::new(),
            reviews: Vec::new(),
            issue: Value::Null,
            thread_updates: 0,
            stop_after_reservation: false,
            forbid_threads: false,
            forbid_mutation: false,
            mutation_errors: None,
            can_resolve: true,
            can_reopen: true,
            reject_review: false,
        }
    }

    fn dispatch(
        &mut self,
        method: &str,
        path: &str,
        token: &str,
        mut payload: Value,
    ) -> (u16, Value, Option<&'static str>) {
        if path == "/users/lint-gate%5Bbot%5D" {
            assert_eq!((method, token), ("GET", "Bearer app-token"));
            return (
                200,
                json!({"login": "lint-gate[bot]", "id": 77, "type": "Bot"}),
                None,
            );
        }
        if path == "/graphql" {
            assert_eq!((method, token), ("POST", "Bearer app-token"));
            if self.forbid_threads {
                return (
                    200,
                    json!({"errors": [{"extensions": {"type": "FORBIDDEN"}}]}),
                    None,
                );
            }
            let query = payload["query"].as_str().unwrap();
            let data = if query.contains("mutation") {
                if let Some(errors) = &self.mutation_errors {
                    return (
                        200,
                        json!({"data": {"resolveReviewThread": null}, "errors": errors}),
                        None,
                    );
                }
                if self.forbid_mutation {
                    return (
                        200,
                        json!({"errors": [{"extensions": {"type": "FORBIDDEN"}}]}),
                        None,
                    );
                }
                let id = payload["variables"]["id"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
                let review = self
                    .reviews
                    .iter_mut()
                    .find(|review| review["id"] == id)
                    .unwrap();
                review["resolved"] = json!(!query.contains("unresolveReviewThread"));
                self.thread_updates += 1;
                let operation = if query.contains("unresolveReviewThread") {
                    "unresolveReviewThread"
                } else {
                    "resolveReviewThread"
                };
                json!({operation: {"thread": {"id": id.to_string(), "isResolved": review["resolved"]}}})
            } else {
                let nodes: Vec<_> = self
                    .reviews
                    .iter()
                    .map(|review| {
                        json!({
                            "id": review["id"].to_string(), "isResolved": review["resolved"],
                            "viewerCanResolve": self.can_resolve, "viewerCanUnresolve": self.can_reopen,
                            "comments": {"nodes": [{"databaseId": review["id"]}]}
                        })
                    })
                    .collect();
                json!({"repository": {"pullRequest": {"reviewThreads": {
                    "nodes": nodes, "pageInfo": {"hasNextPage": false}
                }}}})
            };
            return (200, json!({"data": data}), None);
        }
        let scoped = path.strip_prefix("/repos/owner/repo").unwrap();
        if scoped.starts_with("/issues/")
            || scoped.starts_with("/pulls/7/files?")
            || scoped.starts_with("/pulls/7/comments")
            || scoped.starts_with("/pulls/comments/")
        {
            assert_eq!(token, "Bearer app-token");
            if scoped == "/pulls/7/comments" && method == "POST" && self.reject_review {
                return (422, Value::Null, None);
            }
            return (200, self.feedback(method, scoped, payload), None);
        }
        let path = path.strip_prefix("/repos/owner/repo").unwrap();
        let is_check = path.contains("/check-runs");
        assert_eq!(
            token,
            if is_check {
                "Bearer app-token"
            } else {
                "Bearer read-token"
            }
        );
        let id = &self.run["id"];
        let attempt = &self.run["run_attempt"];
        let result = match method {
            "POST" | "PATCH" => {
                if self.rejected {
                    return (422, Value::Null, None);
                }
                if method == "POST" {
                    assert_eq!(path, "/check-runs");
                    assert_eq!(payload["head_sha"], HEAD);
                } else {
                    assert_eq!(path, "/check-runs/60");
                    assert!(payload.get("head_sha").is_none());
                    payload["head_sha"] = json!(HEAD);
                }
                payload["id"] = json!(60);
                payload["app"] = json!({"id": 42});
                self.check = payload.clone();
                self.writes.push(method.to_owned());
                payload
            }
            "GET" if path == format!("/actions/workflows/{WORKFLOW}") => json!({"id": 30}),
            "GET" if path == format!("/actions/runs/{id}") => self.run.clone(),
            "GET" if path == "/pulls/7" => self.pr.clone(),
            "GET" if path == format!("/git/commits/{MERGE}") => {
                json!({"sha": MERGE, "parents": [{"sha": BASE}, {"sha": HEAD}]})
            }
            "GET"
                if path
                    == format!(
                        "/actions/runs/{id}/attempts/{attempt}/jobs?per_page=100&page=1"
                    ) =>
            {
                let outcome = if self.bootstrap {
                    "skipped"
                } else if self.scan_failure {
                    "failure"
                } else {
                    "success"
                };
                let mut steps = vec![
                    json!({"name": format!("Record candidate 7 {MERGE} {BASE}"), "conclusion": "success"}),
                    json!({"name": "Scan source", "conclusion": outcome}),
                ];
                if self.bootstrap {
                    steps.push(json!({"name": "Record policy bootstrap", "conclusion": "success"}));
                }
                if self.blocking {
                    steps
                        .push(json!({"name": "Record policy violations", "conclusion": "success"}));
                }
                json!({"total_count": 1, "jobs": [{"name": JOB, "run_id": id, "run_attempt": attempt, "steps": steps}]})
            }
            "GET"
                if path
                    == format!(
                        "/actions/runs/{id}/artifacts?name=lint-gate-report-{id}-{attempt}&per_page=100&page=1"
                    ) =>
            {
                assert!(!self.bootstrap, "bootstrap should not fetch an artifact");
                if self.bad_artifact {
                    json!({"total_count": 1, "artifacts": [{
                        "name": format!("lint-gate-report-{id}-{attempt}"), "id": 50, "size_in_bytes": 100,
                        "expired": false, "workflow_run": {"id": id, "repository_id": 10, "head_sha": HEAD},
                    }]})
                } else {
                    json!({"total_count": 0, "artifacts": []})
                }
            }
            "GET" if path == "/actions/artifacts/50/zip" => {
                return (302, Value::Null, Some("http://127.0.0.1/unsafe-report"));
            }
            "GET"
                if path
                    == format!(
                        "/commits/{HEAD}/check-runs?check_name=Lint+Gate+%28advisory%29&app_id=42&filter=all&per_page=100&page=1"
                    ) =>
            {
                if self.check.is_null() {
                    json!({"total_count": 0,"check_runs": []})
                } else {
                    json!({"total_count": 1,"check_runs": [self.check]})
                }
            }
            _ => panic!("unexpected request {method} {path}"),
        };
        (200, result, None)
    }
    fn feedback(&mut self, method: &str, path: &str, mut payload: Value) -> Value {
        let actor = json!({"login":"lint-gate[bot]", "id": 77, "type":"Bot"});
        match method {
            "GET" if path.starts_with("/pulls/7/files?") => json!(self.files),
            "GET" if path.starts_with("/pulls/7/comments?") => json!(self.reviews),
            "GET" if path.starts_with("/issues/7/comments?") => {
                if self.issue.is_null() {
                    json!([])
                } else {
                    json!([self.issue])
                }
            }
            "POST" if path == "/issues/7/comments" => {
                self.issue = json!({"id": 1, "user": actor, "body": payload["body"]});
                if self.stop_after_reservation {
                    self.pr["merge_commit_sha"] = json!(HEAD);
                }
                self.issue.clone()
            }
            "PATCH" if path == "/issues/comments/1" => {
                self.issue["body"] = payload["body"].clone();
                self.issue.clone()
            }
            "POST" if path == "/pulls/7/comments" => {
                assert_eq!(payload["commit_id"], HEAD);
                payload["id"] = json!(1000 + self.reviews.len());
                payload["user"] = actor;
                payload["resolved"] = json!(false);
                self.reviews.push(payload.clone());
                payload
            }
            "PATCH" if path.starts_with("/pulls/comments/") => {
                let id = path.rsplit('/').next().unwrap().parse::<u64>().unwrap();
                let item = self
                    .reviews
                    .iter_mut()
                    .find(|item| item["id"] == id)
                    .unwrap();
                item["body"] = payload["body"].clone();
                item.clone()
            }
            _ => panic!("unexpected feedback request {method} {path}"),
        }
    }
}

fn respond(stream: &mut TcpStream, state: &Mutex<State>) {
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap() == 0 {
        return;
    }
    let mut parts = line.split_ascii_whitespace();
    let method = parts.next().unwrap().to_owned();
    let path = parts.next().unwrap().to_owned();
    let mut token = String::new();
    let mut size = 0;
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let header = line.trim();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap();
        if name.eq_ignore_ascii_case("authorization") {
            token = value.trim().to_owned();
        } else if name.eq_ignore_ascii_case("content-length") {
            size = value.trim().parse().unwrap();
        }
    }
    let mut payload = vec![0; size];
    reader.read_exact(&mut payload).unwrap();
    let payload = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap()
    };
    let (code, response, location) = state
        .lock()
        .unwrap()
        .dispatch(&method, &path, &token, payload);
    let response = response.to_string();
    write!(
        stream,
        "HTTP/1.1 {code} Result\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.len()
    )
    .unwrap();
    if let Some(location) = location {
        write!(stream, "Location: {location}\r\n").unwrap();
    }
    write!(stream, "\r\n{response}").unwrap();
}

struct Mock {
    state: Arc<Mutex<State>>,
    address: std::net::SocketAddr,
    server: Option<thread::JoinHandle<()>>,
}

impl Mock {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::new()));
        let shared = state.clone();
        let server = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut first = [0];
                if stream.peek(&mut first).unwrap() == 0 {
                    return;
                }
                respond(&mut stream, &shared);
            }
        });
        Self {
            state,
            address,
            server: Some(server),
        }
    }

    fn run(&self, incoming: Option<Value>, pin: &str) -> String {
        let output = self.step(incoming, pin, "github-publish", |_| {});
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn step(
        &self,
        incoming: Option<Value>,
        pin: &str,
        name: &str,
        configure: impl FnOnce(&mut Command),
    ) -> Output {
        let tools = common::without_python();
        let dir = tempfile::tempdir().unwrap();
        let event = dir.path().join("event.json");
        let run = incoming.unwrap_or_else(|| self.state.lock().unwrap().run.clone());
        fs::write(
            &event,
            json!({"repository": {"id": 10}, "workflow_run": run}).to_string(),
        )
        .unwrap();
        let mut command = common::command(&tools);
        command
            .arg(name)
            .env("GITHUB_EVENT_PATH", event)
            .env("GITHUB_API_URL", format!("http://{}", self.address))
            .envs([
                ("GITHUB_REPOSITORY", "owner/repo"),
                ("SOURCE_WORKFLOW", WORKFLOW),
                ("ANALYSIS_JOB", JOB),
                ("GITHUB_REPOSITORY_ID", "10"),
                ("APP_ID", "42"),
                ("GITHUB_SERVER_URL", "https://github.com"),
                ("GITHUB_TOKEN", "read-token"),
                ("APP_TOKEN", "app-token"),
            ])
            .env("SCANNER_WORKFLOW", format!("{SCANNER}@{pin}"))
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env_remove("HTTP_PROXY")
            .env_remove("http_proxy")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy");
        configure(&mut command);
        command.output().unwrap()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        drop(TcpStream::connect(self.address));
        let result = self.server.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

#[test]
fn publishes_once_updates_reruns_and_skips_stale_delivery() {
    let mock = Mock::new();
    let initial = mock.state.lock().unwrap().run.clone();
    assert!(
        mock.run(None, "refs/heads/main")
            .contains("Published Lint Gate (advisory)")
    );
    assert!(mock.run(None, "refs/heads/main").contains("already holds"));
    {
        let mut state = mock.state.lock().unwrap();
        assert_eq!(state.check["output"]["title"], "Report unavailable");
        assert_eq!(state.check["conclusion"], "neutral");
        assert!(
            state.check["output"]["summary"]
                .as_str()
                .unwrap()
                .contains("report is missing")
        );
        assert_eq!(
            state.check["details_url"],
            "https://github.com/owner/repo/actions/runs/100/attempts/1"
        );
        state.run["run_attempt"] = json!(2);
        state.bad_artifact = true;
    }
    assert!(mock.run(None, "refs/heads/main").contains("Published"));
    assert!(
        mock.run(Some(initial), "refs/heads/main")
            .contains("advanced")
    );
    {
        let mut state = mock.state.lock().unwrap();
        assert!(
            state.check["output"]["summary"]
                .as_str()
                .unwrap()
                .contains("report is unusable")
        );
        state.run["id"] = json!(99);
        state.run["run_number"] = json!(3);
        state.run["run_attempt"] = json!(9);
    }
    assert!(mock.run(None, "refs/heads/main").contains("newer one"));
    assert_eq!(mock.state.lock().unwrap().writes, ["POST", "PATCH"]);
}

#[test]
fn bootstrap_incomplete_blocking_and_unattachable_checks_keep_their_outcomes() {
    let mock = Mock::new();
    mock.state.lock().unwrap().bootstrap = true;
    mock.run(None, "refs/heads/main");
    {
        let mut state = mock.state.lock().unwrap();
        assert_eq!(state.check["conclusion"], "skipped");
        assert_eq!(state.check["output"]["title"], "Policy bootstrap");
        state.bootstrap = false;
        state.scan_failure = true;
        state.run["run_attempt"] = json!(2);
    }
    mock.run(None, "refs/heads/main");
    {
        let mut state = mock.state.lock().unwrap();
        assert_eq!(state.check["output"]["title"], "Analysis incomplete");
        assert_eq!(state.check["conclusion"], "failure");
        state.blocking = true;
        state.run["run_attempt"] = json!(3);
    }
    mock.run(None, "refs/heads/main");
    {
        let mut state = mock.state.lock().unwrap();
        assert_eq!(state.check["output"]["title"], "Blocking policy findings");
        assert_eq!(state.check["conclusion"], "failure");
        state.rejected = true;
        state.run["run_attempt"] = json!(4);
    }
    assert!(
        mock.run(None, "refs/heads/main")
            .contains("could not attach")
    );
    assert_eq!(
        mock.state.lock().unwrap().writes,
        ["POST", "PATCH", "PATCH"]
    );
}

#[test]
fn writes_require_a_verified_run_and_current_pr() {
    let mock = Mock::new();
    let mut incorrect = mock.state.lock().unwrap().run.clone();
    incorrect["workflow_id"] = json!(999);
    assert!(
        mock.run(Some(incorrect), "refs/heads/main")
            .contains("configured PR analysis workflow")
    );
    mock.state.lock().unwrap().pr["merge_commit_sha"] = json!(HEAD);
    assert!(
        mock.run(None, "refs/heads/main")
            .contains("synthetic merge")
    );
    {
        let mut state = mock.state.lock().unwrap();
        state.pr["merge_commit_sha"] = json!(MERGE);
        state.run["referenced_workflows"][0]["ref"] = json!("refs/tags/main");
    }
    assert!(
        mock.run(None, "refs/heads/main")
            .contains("reusable scanner revision")
    );
    assert!(mock.state.lock().unwrap().writes.is_empty());
    mock.state.lock().unwrap().run["referenced_workflows"][0]["path"] =
        json!(format!("{SCANNER}@{BRANCH_SHA}"));
    assert!(mock.run(None, BRANCH_SHA).contains("Published"));
}

struct PublicationFixture {
    root: tempfile::TempDir,
    mock: Mock,
}

impl PublicationFixture {
    fn new() -> Self {
        let mock = Mock::new();
        mock.state.lock().unwrap().files = vec![json!({
            "filename": "flow.ts", "status": "modified", "patch":"@@ -1,8 +1,8 @@\n function flow() {}"
        })];
        Self {
            root: tempfile::tempdir().unwrap(),
            mock,
        }
    }

    fn blocking(&self, blocking: bool) {
        let mut state = self.mock.state.lock().unwrap();
        state.blocking = blocking;
        state.scan_failure = blocking;
    }

    fn publish(
        &self,
        version: u8,
        findings: &[Value],
        scan_complete: bool,
        compatible: &[&str],
        configure: impl FnOnce(&mut Command),
    ) -> Output {
        let outputs = self.root.path().join("prepare");
        fs::write(&outputs, "").unwrap();
        let prepared = self.mock.step(
            None,
            "refs/heads/main",
            "github-prepare-conversation",
            |cmd| {
                cmd.env_remove("APP_TOKEN")
                    .env_remove("APP_ID")
                    .env("GITHUB_OUTPUT", &outputs);
            },
        );
        assert!(prepared.status.success(), "{prepared:?}");
        let prepared = fs::read_to_string(outputs).unwrap();
        assert!(prepared.contains("ready=true"));
        let values: std::collections::BTreeMap<_, _> = prepared
            .lines()
            .map(|line| line.split_once('=').unwrap())
            .collect();
        let artifact = "lint-gate-verified-findings-321-1";
        let report = self.root.path().join("report.json");
        fs::write(
            &report,
            json!({"format_version": version, "scoring_version": 1,
            "files_scanned": 1, "diagnostics": [], "findings": findings})
            .to_string(),
        )
        .unwrap();
        let manifest = self.root.path().join("anchors.json");
        fs::write(
            &manifest,
            json!({"format_version": 1,
            "revision": values["revision"], "base": values["base"], "head": values["head"],
            "source_run": values["source_run"], "source_attempt": values["source_attempt"],
            "artifact_name": artifact, "compatible_paths": compatible})
            .to_string(),
        )
        .unwrap();
        self.mock.step(
            None,
            "refs/heads/main",
            "github-publish-conversation",
            |cmd| {
                cmd.envs([
                    ("APP_SLUG", "lint-gate"),
                    ("FINDINGS_ARTIFACT_NAME", artifact),
                    (
                        "SCAN_COMPLETE",
                        if scan_complete { "true" } else { "false" },
                    ),
                ])
                .env("FINDINGS_REPORT", report)
                .env("FINDINGS_MANIFEST", manifest);
                for (key, env) in [
                    ("pull_request", "PULL_REQUEST"),
                    ("revision", "VERIFIED_REVISION"),
                    ("base", "VERIFIED_BASE"),
                    ("head", "VERIFIED_HEAD"),
                    ("source_run", "SOURCE_RUN"),
                    ("source_attempt", "SOURCE_ATTEMPT"),
                    ("scanner_repository", "VERIFIED_SCANNER_REPOSITORY"),
                    ("scanner_sha", "VERIFIED_SCANNER_SHA"),
                    ("details_url", "DETAILS_URL"),
                    ("source_outcome", "SOURCE_OUTCOME"),
                ] {
                    cmd.env(env, values[key]);
                }
                configure(cmd);
            },
        )
    }

    fn deliver(&self, findings: &[Value], compatible: &[&str]) -> String {
        let result = self.publish(2, findings, true, compatible, |_| {});
        assert!(result.status.success(), "{result:?}");
        String::from_utf8(result.stdout).unwrap()
    }
}

fn finding(name: &str, line: u64, status: &str) -> Value {
    json!({"rule":"cyclomatic-complexity", "path":"flow.ts", "language":"typescript", "name":name,
        "scope":[{"name":name,"kind":"function"}], "range":{"start":{"line":line,"column":1}},
        "score":3,"limit":1,"severity":if status == "reported" {"warn"} else {"error"},"status":status})
}

#[test]
fn conversation_publishes_unscored_finally_findings_and_rejects_legacy_version() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let mut blocker = finding("flow", 2, "blocking");
    blocker["rule"] = json!("finally-exit");
    blocker["range"]["end"] = json!({"line": 2, "column": 7});
    blocker["transfer"] = json!("return");
    blocker["message"] = json!("return exits finally");
    blocker.as_object_mut().unwrap().remove("score");
    blocker.as_object_mut().unwrap().remove("limit");
    let invalid = fixture.publish(2, &[blocker.clone()], true, &["flow.ts"], |_| {});
    assert!(!invalid.status.success());
    let mut malformed = blocker.clone();
    malformed["range"].as_object_mut().unwrap().remove("end");
    let invalid = fixture.publish(3, &[malformed], true, &["flow.ts"], |_| {});
    assert!(!invalid.status.success());
    let valid = fixture.publish(3, &[blocker.clone()], true, &["flow.ts"], |_| {});
    assert!(valid.status.success(), "{valid:?}");
    let state = fixture.mock.state.lock().unwrap();
    let body = state.reviews[0]["body"].as_str().unwrap();
    assert!(body.contains("return exits finally"));
    assert!(!body.contains("null"));
    assert!(!body.contains("score"));
    drop(state);
    blocker["transfer"] = json!("break");
    let valid = fixture.publish(3, &[blocker], true, &["flow.ts"], |_| {});
    assert!(valid.status.success(), "{valid:?}");
    let state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.reviews.len(), 2);
}

#[test]
fn conversation_keeps_existing_thread_identity_and_fixes_resolve_only_app_threads() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let blocker = finding("flow", 2, "blocking");
    let warn = finding("@everyone <details> `warning`", 4, "reported");
    let debt = finding("existing debt", 5, "baselined");
    let findings = [blocker.clone(), warn, debt];
    assert!(
        fixture
            .deliver(&findings, &["flow.ts"])
            .contains("Published")
    );
    fixture.deliver(&findings, &["flow.ts"]);
    {
        let mut state = fixture.mock.state.lock().unwrap();
        assert_eq!(state.reviews.len(), 1);
        assert_eq!(state.reviews[0]["line"], 2);
        assert_eq!(state.reviews[0]["side"], "RIGHT");
        assert!(state.reviews[0]["body"].as_str().unwrap().contains(
            "<!-- lint-gate-finding:e70b2329e2f09e1ec4cbf37ecf035ddfda6f7a2d762c077c66e5159236ddfb9e -->"
        ));
        let summary = state.issue["body"].as_str().unwrap();
        assert!(summary.contains(" · score `3`, limit `1`"), "{summary}");
        assert!(summary.contains(r"\u0040everyone \u003cdetails> \u0060warning\u0060"));
        assert!(!summary.contains("existing debt"));
        state
            .reviews
            .push(json!({"id": 900, "body": "<!-- lint-gate-finding:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa -->",
            "resolved":false,"user":{"type":"User","id":90,"login":"human"}}));
    }
    let moved = finding("flow", 3, "blocking");
    fixture.deliver(&[moved], &["flow.ts"]);
    {
        let mut state = fixture.mock.state.lock().unwrap();
        assert_eq!(state.reviews.len(), 3);
        assert_eq!(state.reviews[0]["resolved"], true);
        assert_eq!(state.reviews[2]["line"], 3);
        state.run["run_attempt"] = json!(2);
    }
    fixture.blocking(false);
    fixture.deliver(&[], &["flow.ts"]);
    let state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.reviews[1]["resolved"], false);
    assert_eq!(state.reviews[2]["resolved"], true);
}

#[test]
fn conversation_supports_legacy_reports_and_file_anchors_and_rejects_unsafe_inputs() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let mut legacy = finding("flow", 2, "blocking");
    legacy.as_object_mut().unwrap().remove("severity");
    let report = [legacy];
    let result = fixture.publish(3, &report, true, &[], |_| {});
    assert!(!result.status.success());
    assert!(fixture.mock.state.lock().unwrap().issue.is_null());
    let result = fixture.publish(1, &report, true, &[], |cmd| {
        cmd.env("VERIFIED_HEAD", BASE);
    });
    assert!(String::from_utf8_lossy(&result.stdout).contains("target changed"));
    let result = fixture.publish(1, &report, false, &[], |_| {});
    assert!(String::from_utf8_lossy(&result.stdout).contains("No complete verified scan"));
    assert!(fixture.mock.state.lock().unwrap().issue.is_null());
    let result = fixture.publish(1, &report, true, &[], |_| {});
    assert!(result.status.success(), "{result:?}");
    let state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.reviews[0]["subject_type"], "file");
    assert!(state.reviews[0].get("line").is_none());
}

#[test]
fn incomplete_evidence_preserves_existing_app_thread_and_summary() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    fixture.deliver(&[finding("flow", 2, "blocking")], &["flow.ts"]);
    let (reviews, issue) = {
        let state = fixture.mock.state.lock().unwrap();
        (state.reviews.clone(), state.issue.clone())
    };
    let result = fixture.publish(2, &[], false, &["flow.ts"], |_| {});
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("No complete verified scan"));
    {
        let mut state = fixture.mock.state.lock().unwrap();
        assert_eq!(state.reviews, reviews);
        assert_eq!(state.issue, issue);
        state.blocking = false;
        state.scan_failure = true;
    }
    let result = fixture.publish(2, &[], true, &["flow.ts"], |_| {});
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("No complete verified scan"));
    let state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.reviews, reviews);
    assert_eq!(state.issue, issue);
    assert_eq!(state.thread_updates, 0);
}

#[test]
fn conversation_rechecks_pr_before_writes_and_rejects_older_feedback() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let blocker = [finding("flow", 2, "blocking")];
    fixture.mock.state.lock().unwrap().stop_after_reservation = true;
    assert!(
        fixture
            .deliver(&blocker, &["flow.ts"])
            .contains("synthetic merge")
    );
    {
        let mut state = fixture.mock.state.lock().unwrap();
        assert!(state.reviews.is_empty());
        state.pr["merge_commit_sha"] = json!(MERGE);
        state.stop_after_reservation = false;
        state.run["run_number"] = json!(3);
    }
    assert!(
        fixture
            .deliver(&blocker, &["flow.ts"])
            .contains("newer verified scan")
    );
    {
        let mut state = fixture.mock.state.lock().unwrap();
        state.run["run_number"] = json!(5);
        state.forbid_threads = true;
    }
    let result = fixture.publish(2, &blocker, true, &["flow.ts"], |_| {});
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Pull requests write"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("thread query failed"));
    assert!(fixture.mock.state.lock().unwrap().reviews.is_empty());
}

#[test]
fn thread_capabilities_and_mutation_failures_identify_the_writer_and_thread() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let blocker = [finding("flow", 2, "blocking")];
    fixture.deliver(&blocker, &["flow.ts"]);
    fixture.blocking(false);
    fixture.mock.state.lock().unwrap().can_resolve = false;
    let denied = fixture.publish(2, &[], true, &["flow.ts"], |_| {});
    assert!(denied.status.success());
    let output = String::from_utf8_lossy(&denied.stdout);
    assert!(output.contains("lint-gate[bot] (bot ID 77"));
    assert!(output.contains("viewerCanResolve=false"));
    assert!(
        fixture.mock.state.lock().unwrap().issue["body"]
            .as_str()
            .unwrap()
            .contains("#discussion_r1000")
    );
    assert_eq!(fixture.mock.state.lock().unwrap().thread_updates, 0);
    {
        let mut state = fixture.mock.state.lock().unwrap();
        state.can_resolve = true;
        state.forbid_mutation = true;
    }
    let failed = fixture.publish(2, &[], true, &["flow.ts"], |_| {});
    assert!(failed.status.success());
    assert!(
        fixture.mock.state.lock().unwrap().issue["body"]
            .as_str()
            .unwrap()
            .contains("#discussion_r1000")
    );
    fixture.mock.state.lock().unwrap().forbid_mutation = false;
    fixture.deliver(&[], &["flow.ts"]);
    fixture.blocking(true);
    fixture.mock.state.lock().unwrap().can_reopen = false;
    let denied = fixture.publish(2, &blocker, true, &["flow.ts"], |_| {});
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("reopen denied for thread"));
    {
        let mut state = fixture.mock.state.lock().unwrap();
        state.can_reopen = true;
        state.forbid_mutation = true;
    }
    let failed = fixture.publish(2, &blocker, true, &["flow.ts"], |_| {});
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("reopen mutation for thread"));
    fixture.mock.state.lock().unwrap().forbid_mutation = false;
    fixture.deliver(&blocker, &["flow.ts"]);
    let updates = fixture.mock.state.lock().unwrap().thread_updates;
    fixture.deliver(&blocker, &["flow.ts"]);
    assert_eq!(fixture.mock.state.lock().unwrap().thread_updates, updates);
}

#[test]
fn all_denied_obsolete_threads_are_reported_and_other_findings_continue() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let initial: Vec<_> = (0..18)
        .map(|i| finding(&format!("old{i}"), 2, "blocking"))
        .collect();
    fixture.deliver(&initial, &["flow.ts"]);
    {
        let mut state = fixture.mock.state.lock().unwrap();
        state.can_resolve = false;
        state.reviews.push(json!({"id": 900, "body": "<!-- lint-gate-finding:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa -->", "resolved":false,"user":{"type":"User","id":90,"login":"human"}}));
    }
    let current = [finding("new", 3, "blocking")];
    fixture.deliver(&current, &["flow.ts"]);
    let state = fixture.mock.state.lock().unwrap();
    let summary = state.issue["body"].as_str().unwrap();
    assert!(summary.contains("18 obsolete review thread(s)"));
    for id in 1000..1018 {
        assert!(summary.contains(&format!("#discussion_r{id}")));
    }
    assert!(!summary.contains("#discussion_r900"));
    assert_eq!(state.reviews.len(), 20);
    assert_eq!(state.reviews[18]["resolved"], false);
    let prior = summary.to_owned();
    drop(state);
    fixture.deliver(&current, &["flow.ts"]);
    let state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.issue["body"], prior);
    assert_eq!(state.reviews.len(), 20);
}

#[test]
fn hosted_graphql_forbidden_shape_is_recoverable_but_mixed_errors_are_fatal() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    fixture.deliver(&[finding("flow", 2, "blocking")], &["flow.ts"]);
    fixture.blocking(false);
    let forbidden = json!({"type":"FORBIDDEN", "path":["resolveReviewThread"],
        "locations":[{"line":1,"column":21}], "message":"Resource not accessible by integration"});
    fixture.mock.state.lock().unwrap().mutation_errors = Some(json!([forbidden.clone()]));
    fixture.deliver(&[], &["flow.ts"]);
    assert!(
        fixture.mock.state.lock().unwrap().issue["body"]
            .as_str()
            .unwrap()
            .contains("#discussion_r1000")
    );
    fixture.mock.state.lock().unwrap().mutation_errors =
        Some(json!([forbidden, {"type":"INTERNAL", "message":"Unexpected error"}]));
    let result = fixture.publish(2, &[], true, &["flow.ts"], |_| {});
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("resolve mutation for thread"));
    assert_eq!(fixture.mock.state.lock().unwrap().thread_updates, 0);
}

#[test]
fn moved_blocker_stays_visible_until_the_old_thread_is_resolved() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    fixture.deliver(&[finding("flow", 2, "blocking")], &["flow.ts"]);
    fixture.mock.state.lock().unwrap().can_resolve = false;
    let moved = [finding("flow", 3, "blocking")];
    fixture.deliver(&moved, &["flow.ts"]);
    fixture.deliver(&moved, &["flow.ts"]);
    {
        let mut state = fixture.mock.state.lock().unwrap();
        let summary = state.issue["body"].as_str().unwrap();
        assert!(summary.contains("Blocking error (resolve obsolete thread to update)"));
        assert!(summary.contains("#discussion_r1000"));
        assert_eq!(state.reviews.len(), 1);
        state.reviews[0]["resolved"] = json!(true);
    }
    fixture.deliver(&moved, &["flow.ts"]);
    let state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.reviews.len(), 2);
    assert_eq!(state.reviews[1]["line"], 3);
    assert!(
        !state.issue["body"]
            .as_str()
            .unwrap()
            .contains("manual resolution")
    );
}

#[test]
fn conversation_uses_second_duplication_anchor_and_disambiguates_repeated_callables() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let duplication = json!({"rule":"source-duplication", "score":12, "limit":10,
    "severity":"error", "status":"blocking", "occurrences":[
        {"path":"outside.ts", "range":{"start":{"line":1,"column":1}}},
        {"path":"flow.ts", "range":{"start":{"line":7,"column":1}}}
    ]});
    let findings = [
        finding("duplicate", 2, "blocking"),
        finding("duplicate", 3, "blocking"),
        duplication,
    ];
    fixture.deliver(&findings, &["flow.ts"]);
    let mut state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.reviews.len(), 3);
    assert_eq!(state.reviews[2]["path"], "flow.ts");
    assert_eq!(state.reviews[2]["line"], 7);
    assert_ne!(state.reviews[0]["body"], state.reviews[1]["body"]);
    state.reviews.clear();
    state.reject_review = true;
    drop(state);
    fixture.deliver(&findings, &["flow.ts"]);
    let state = fixture.mock.state.lock().unwrap();
    assert!(state.reviews.is_empty());
    assert_eq!(
        state.issue["body"]
            .as_str()
            .unwrap()
            .matches("Unanchored blocking error")
            .count(),
        3
    );
}

#[test]
fn conversation_bounds_threads_and_can_replace_a_moved_thread_at_the_limit() {
    let fixture = PublicationFixture::new();
    fixture.blocking(true);
    let mut findings: Vec<_> = (0..51)
        .map(|index| finding(&format!("flow{index}"), 2, "blocking"))
        .collect();
    fixture.deliver(&findings, &["flow.ts"]);
    {
        let state = fixture.mock.state.lock().unwrap();
        assert_eq!(state.reviews.len(), 50);
        assert!(
            state.issue["body"]
                .as_str()
                .unwrap()
                .contains("thread limit reached")
        );
    }
    findings[0] = finding("flow0", 3, "blocking");
    fixture.deliver(&findings, &["flow.ts"]);
    let state = fixture.mock.state.lock().unwrap();
    assert_eq!(state.reviews.len(), 51);
    assert_eq!(state.reviews[0]["resolved"], true);
    assert_eq!(state.reviews[50]["line"], 3);
}
