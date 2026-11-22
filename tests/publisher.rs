mod common;

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

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
        }
    }

    fn dispatch(
        &mut self,
        method: &str,
        path: &str,
        token: &str,
        mut payload: Value,
    ) -> (u16, Value, Option<&'static str>) {
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
        let tools = common::without_python();
        let dir = tempfile::tempdir().unwrap();
        let event = dir.path().join("event.json");
        let run = incoming.unwrap_or_else(|| self.state.lock().unwrap().run.clone());
        fs::write(
            &event,
            json!({"repository": {"id": 10}, "workflow_run": run}).to_string(),
        )
        .unwrap();
        let output = common::command(&tools)
            .arg("github-publish")
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
            .env_remove("all_proxy")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
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
        state.blocking = true;
        state.run["run_attempt"] = json!(3);
    }
    mock.run(None, "refs/heads/main");
    {
        let mut state = mock.state.lock().unwrap();
        assert_eq!(state.check["output"]["title"], "Blocking policy findings");
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
