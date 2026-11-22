use super::{
    Candidate, Config, Event, Outcome, Run, app_id, candidate_job, context, print_result, required,
    scan_outcome, source_matches, target_head, verified_run,
};
use crate::github::api::{Error, GitHub, Result};
use crate::github::{append, env_path, evidence, optional};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

const MAX_THREADS: usize = 50;
const MAX_SUMMARY: usize = 50;
const SUMMARY_MARKER: &str = "<!-- lint-gate-warning-summary -->";

struct Target {
    run: Run,
    candidate: Candidate,
    head: String,
    scanner_repository: String,
    scanner_sha: String,
    details_url: String,
}

impl Target {
    fn verify(api: &GitHub, event: &Event, config: &Config) -> Result<Self> {
        let run = verified_run(api, event, config)?;
        let candidate = candidate_job(api, &run.identity, &config.job)?;
        let head = target_head(api, &run, &candidate, config.repository_id)?;
        let scanner_sha = run
            .referenced_workflows
            .iter()
            .flatten()
            .find(|item| source_matches(item, &config.scanner))
            .ok_or(Error::Metadata)?
            .sha
            .clone();
        let (scanner_repository, _) = config
            .scanner
            .split_once("/.github/workflows/")
            .ok_or(Error::Metadata)?;
        let details_url = format!(
            "{}/actions/runs/{}/attempts/{}",
            config.repository_url, run.identity.id, run.identity.run_attempt
        );
        Ok(Self {
            run,
            candidate,
            head,
            scanner_repository: scanner_repository.to_owned(),
            scanner_sha,
            details_url,
        })
    }

    fn outcome(&self) -> Outcome {
        scan_outcome(&self.candidate.steps)
    }

    fn output(&self) -> [(&'static str, &'static str, String); 10] {
        [
            (
                "pull_request",
                "PULL_REQUEST",
                self.candidate.number.to_string(),
            ),
            (
                "revision",
                "VERIFIED_REVISION",
                self.candidate.revision.clone(),
            ),
            ("base", "VERIFIED_BASE", self.candidate.base.clone()),
            ("head", "VERIFIED_HEAD", self.head.clone()),
            ("source_run", "SOURCE_RUN", self.run.identity.id.to_string()),
            (
                "source_attempt",
                "SOURCE_ATTEMPT",
                self.run.identity.run_attempt.to_string(),
            ),
            (
                "scanner_repository",
                "VERIFIED_SCANNER_REPOSITORY",
                self.scanner_repository.clone(),
            ),
            (
                "scanner_sha",
                "VERIFIED_SCANNER_SHA",
                self.scanner_sha.clone(),
            ),
            ("details_url", "DETAILS_URL", self.details_url.clone()),
            (
                "source_outcome",
                "SOURCE_OUTCOME",
                outcome_name(self.outcome()).to_owned(),
            ),
        ]
    }

    fn matches_manifest(&self, manifest: &evidence::Manifest) -> bool {
        manifest.format_version == 1
            && manifest.revision == self.candidate.revision
            && manifest.base == self.candidate.base
            && manifest.head == self.head
            && manifest.source_run == self.run.identity.id.to_string()
            && manifest.source_attempt == self.run.identity.run_attempt.to_string()
            && optional("FINDINGS_ARTIFACT_NAME").as_deref()
                == Some(manifest.artifact_name.as_str())
    }
}

fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Bootstrap => "bootstrap",
        Outcome::Passed => "passed",
        Outcome::Blocking => "blocking",
        Outcome::Incomplete => "incomplete",
    }
}

fn prepare() -> Result<String> {
    let (config, event, api, _) = context()?;
    let target = match Target::verify(&api, &event, &config) {
        Ok(target) => target,
        Err(Error::Skip(message)) => {
            append("GITHUB_OUTPUT", "ready=false\n").map_err(|_| Error::Metadata)?;
            return Ok(format!("Skipped conversation publication. {message}"));
        }
        Err(error) => return Err(error),
    };
    let complete = matches!(target.outcome(), Outcome::Passed | Outcome::Blocking);
    let mut output = format!("ready=true\nscan_complete={complete}\n");
    for (key, _, value) in target.output() {
        if value.contains(['\r', '\n']) {
            return Err(Error::Metadata);
        }
        writeln!(output, "{key}={value}").unwrap();
    }
    append("GITHUB_OUTPUT", &output).map_err(|_| Error::Metadata)?;
    Ok("Verified the completed scan and current PR.".to_owned())
}

pub fn prepare_action() -> u8 {
    print_result(prepare(), "Pull requests")
}

type Order = (u64, u64, u64);

fn publish() -> Result<String> {
    let (config, event, api, base_url) = context()?;
    let target = Target::verify(&api, &event, &config)?;
    let expected = target.output();
    if expected
        .iter()
        .any(|(_, env, value)| optional(env).as_ref() != Some(value))
    {
        return Err(Error::skip(
            "The verified scan target changed before publication.",
        ));
    }
    let verify_current = || {
        let current = Target::verify(&api, &event, &config)?;
        if current.run.identity != target.run.identity || current.output() != expected {
            return Err(Error::skip(
                "The PR or scanner changed before feedback was written.",
            ));
        }
        Ok(())
    };
    if !matches!(target.outcome(), Outcome::Passed | Outcome::Blocking)
        || optional("SCAN_COMPLETE").as_deref() != Some("true")
    {
        verify_current()?;
        return Ok("No complete verified scan is available for PR conversations.".to_owned());
    }
    let path = env_path("FINDINGS_REPORT").map_err(|_| Error::Metadata)?;
    let (report, _) = evidence::bounded_json(&path).map_err(|_| Error::Metadata)?;
    let findings = evidence::findings(&report).ok_or(Error::Metadata)?;
    if findings.iter().any(|item| item["status"] == "blocking")
        != (target.outcome() == Outcome::Blocking)
    {
        return Err(Error::skip(
            "The independent scan disagreed with the native policy gate result.",
        ));
    }
    let path = env_path("FINDINGS_MANIFEST").map_err(|_| Error::Metadata)?;
    let (manifest, _) = evidence::bounded_json(&path).map_err(|_| Error::Metadata)?;
    let manifest: evidence::Manifest =
        serde_json::from_value(manifest).map_err(|_| Error::Metadata)?;
    if !target.matches_manifest(&manifest) {
        return Err(Error::skip(
            "The source coordinate manifest does not match the verified PR target.",
        ));
    }
    let slug = optional("APP_SLUG");
    let actor = slug
        .as_deref()
        .map(|slug| verified_app_actor(&api, slug))
        .transpose()?;
    let writes = GitHub::new(&base_url, &config.repository, required("APP_TOKEN")?);
    let publication = Publication {
        writes: &writes,
        repository: &config.repository,
        target: &target,
        verify: &verify_current,
        paths: &manifest.compatible_paths,
        app_id: app_id()?,
        actor,
    };
    publication.reconcile(findings)?;
    Ok("Published PR conversations from the verified scan.".to_owned())
}

pub fn publish_action() -> u8 {
    print_result(publish(), "Pull requests")
}

fn verified_app_actor(api: &GitHub, slug: &str) -> Result<(String, u64)> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::Metadata);
    }
    let login = format!("{slug}[bot]");
    let actor: Value = api.api(&format!("/users/{slug}%5Bbot%5D"), "GET", None)?;
    if actor["login"] != login || actor["type"] != "Bot" {
        return Err(Error::Metadata);
    }
    Ok((login, actor["id"].as_u64().ok_or(Error::Metadata)?))
}

fn code(value: &Value) -> String {
    let value = match value.as_str() {
        Some(text) if text.chars().count() > 200 => {
            json!(format!("{}...", text.chars().take(197).collect::<String>()))
        }
        _ => value.clone(),
    };
    format!(
        "`{}`",
        value
            .to_string()
            .replace('`', r"\u0060")
            .replace('@', r"\u0040")
            .replace('<', r"\u003c")
    )
}

// Match the marker encoding of the existing publisher, including ASCII escapes.
// Changing it would orphan threads written by earlier releases.
fn identity_json(value: &Value, spaces: bool) -> String {
    let comma = if spaces { ", " } else { "," };
    let colon = if spaces { ": " } else { ":" };
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| identity_json(item, spaces))
                .collect::<Vec<_>>()
                .join(comma)
        ),
        Value::Object(items) => format!(
            "{{{}}}",
            items
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}{colon}{}",
                        identity_json(&json!(key), spaces),
                        identity_json(value, spaces)
                    )
                })
                .collect::<Vec<_>>()
                .join(comma)
        ),
        _ => {
            let mut text = String::new();
            for unit in value.to_string().encode_utf16() {
                if unit >= 0x7f {
                    write!(text, "\\u{unit:04x}").unwrap();
                } else {
                    text.push(char::from(unit as u8));
                }
            }
            text
        }
    }
}

fn digest(text: &str) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let mut result = String::with_capacity(64);
    for byte in hash.as_ref() {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}

fn identity(finding: &Value) -> String {
    let value = if finding["rule"] == "source-duplication" {
        let mut locations: Vec<_> = evidence::locations(finding)
            .iter()
            .map(|location| json!({"path": location["path"], "range": location["range"]}))
            .collect();
        locations.sort_by_key(|item| identity_json(item, true));
        json!({"rule": finding["rule"], "occurrences": locations})
    } else {
        json!({"rule": finding["rule"], "path": finding["path"], "language": finding["language"],
            "scope": finding["scope"], "name": finding["name"]})
    };
    digest(&identity_json(&value, false))
}

#[derive(PartialEq, Debug)]
struct Anchor<'a> {
    path: &'a str,
    line: Option<u64>,
}

fn anchor<'a>(
    finding: &'a Value,
    files: &[Value],
    compatible: &BTreeSet<String>,
    hunk: &Regex,
) -> Option<Anchor<'a>> {
    let mut fallback = None;
    for location in evidence::locations(finding) {
        let Some(path) = location["path"].as_str() else {
            continue;
        };
        if path.contains('\\') || path.split('/').any(|part| matches!(part, "" | "." | "..")) {
            continue;
        }
        let Some(file) = files
            .iter()
            .find(|file| file["filename"] == path && file["status"] != "removed")
        else {
            continue;
        };
        fallback = fallback.or(Some(Anchor { path, line: None }));
        let Some((line, patch)) = location["range"]["start"]["line"]
            .as_u64()
            .filter(|line| *line > 0)
            .zip(file["patch"].as_str())
            .filter(|_| compatible.contains(path))
        else {
            continue;
        };
        if patch
            .lines()
            .filter_map(|text| hunk.captures(text))
            .any(|item| {
                item[1].parse::<u64>().ok().is_some_and(|start| {
                    let count = item
                        .get(2)
                        .map_or(Some(1), |item| item.as_str().parse::<u64>().ok());
                    count.is_some_and(|count| line >= start && line - start < count)
                })
            })
        {
            return Some(Anchor {
                path,
                line: Some(line),
            });
        }
    }
    fallback
}

fn graphql(api: &GitHub, query: &str, variables: Value) -> Result<Value> {
    let response: Value = api.api(
        "/graphql",
        "POST",
        Some(&json!({"query": query, "variables": variables})),
    )?;
    if let Some(errors) = response["errors"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        if errors.iter().any(|error| {
            error["extensions"]["type"] == "FORBIDDEN"
                || error["message"]
                    .as_str()
                    .is_some_and(|message| message.to_ascii_lowercase().contains("not accessible"))
        }) {
            return Err(Error::Status {
                operation: "review-thread operation".to_owned(),
                status: 403,
            });
        }
        return Err(Error::Metadata);
    }
    response.get("data").cloned().ok_or(Error::Metadata)
}

struct Publication<'a> {
    writes: &'a GitHub,
    repository: &'a str,
    target: &'a Target,
    verify: &'a dyn Fn() -> Result<()>,
    paths: &'a BTreeSet<String>,
    app_id: u64,
    actor: Option<(String, u64)>,
}

impl Publication<'_> {
    fn owns(&self, comment: &Value) -> bool {
        comment["performed_via_github_app"]["id"] == self.app_id
            || self.actor.as_ref().is_some_and(|(login, id)| {
                comment["user"]["type"] == "Bot"
                    && comment["user"]["id"] == *id
                    && comment["user"]["login"] == *login
            })
    }

    fn send(&self, path: &str, method: &str, payload: &Value) -> Result<Value> {
        (self.verify)()?;
        self.writes.send(path, method, Some(payload))
    }

    fn threads(&self) -> Result<Vec<Value>> {
        let (owner, name) = self.repository.split_once('/').ok_or(Error::Metadata)?;
        let query = "query($owner:String!, $name:String!, $number:Int!, $cursor:String) {
          repository(owner:$owner, name:$name) { pullRequest(number:$number) {
            reviewThreads(first:100, after:$cursor) {
              nodes { id isResolved comments(first:100) { nodes { databaseId } } }
              pageInfo { hasNextPage endCursor }
            }
          }}
        }";
        let mut cursor = Value::Null;
        let mut threads = Vec::new();
        for _ in 0..20 {
            let data = graphql(
                self.writes,
                query,
                json!({"owner": owner, "name": name, "number": self.target.candidate.number, "cursor": cursor}),
            )?;
            let connection = &data["repository"]["pullRequest"]["reviewThreads"];
            threads.extend(
                connection["nodes"]
                    .as_array()
                    .ok_or(Error::Metadata)?
                    .iter()
                    .cloned(),
            );
            if connection["pageInfo"]["hasNextPage"] == false {
                return Ok(threads);
            }
            cursor = connection["pageInfo"]["endCursor"].clone();
            if !cursor.is_string() {
                return Err(Error::Metadata);
            }
        }
        Err(Error::Metadata)
    }

    fn resolve(&self, thread: &Value, resolved: bool) -> Result<()> {
        (self.verify)()?;
        let operation = if resolved {
            "resolveReviewThread"
        } else {
            "unresolveReviewThread"
        };
        let id = thread["id"].as_str().ok_or(Error::Metadata)?;
        let query = format!(
            "mutation($id:ID!) {{ {operation}(input:{{threadId:$id}}) {{ thread {{ id isResolved }} }} }}"
        );
        graphql(self.writes, &query, json!({"id": id}))?;
        Ok(())
    }

    fn reconcile(&self, findings: &[Value]) -> Result<()> {
        let number = self.target.candidate.number;
        let files = self
            .writes
            .pages(&format!("/pulls/{number}/files"), "files")?;
        let mut summary = Vec::new();
        let mut blocking = Vec::new();
        let mut counts = BTreeMap::new();
        for finding in findings {
            match finding["status"].as_str() {
                Some("reported") => summary.push(("Warning", finding)),
                Some("blocking") => {
                    let key = identity(finding);
                    *counts.entry(key.clone()).or_insert(0) += 1;
                    blocking.push((key, finding));
                }
                _ => {}
            }
        }
        for (identity, finding) in &mut blocking {
            if counts[identity] > 1 && finding["rule"] != "source-duplication" {
                *identity = digest(&identity_json(&json!([identity, finding["range"]]), true));
            }
        }
        let blockers: BTreeSet<_> = blocking
            .iter()
            .map(|(identity, _)| identity.as_str())
            .collect();

        let review_path = format!("/pulls/{number}/comments");
        let summary_path = format!("/issues/{number}/comments");
        let review_comments = self.writes.pages(&review_path, "comments")?;
        let issue_comments = self.writes.pages(&summary_path, "comments")?;
        let review_comments: Vec<_> = review_comments
            .iter()
            .filter(|item| self.owns(item))
            .collect();
        let issue_comments: Vec<_> = issue_comments
            .iter()
            .filter(|item| self.owns(item))
            .collect();
        let order = (
            self.target.run.run_number,
            self.target.run.identity.run_attempt,
            self.target.run.identity.id,
        );
        let order_pattern =
            Regex::new(r"<!-- lint-gate-feedback:v1:([0-9]+):([0-9]+):([0-9]+) -->").unwrap();
        if review_comments
            .iter()
            .chain(&issue_comments)
            .filter_map(|item| read_order(&order_pattern, body(item)))
            .any(|recorded| recorded > order)
        {
            return Err(Error::skip(
                "A newer verified scan already published PR feedback.",
            ));
        }
        let order_marker = format!(
            "<!-- lint-gate-feedback:v1:{}:{}:{} -->",
            order.0, order.1, order.2
        );
        let summary_id = if let Some(comment) = issue_comments
            .iter()
            .find(|item| body(item).contains(SUMMARY_MARKER))
        {
            let id = comment["id"].as_u64().ok_or(Error::Metadata)?;
            let reserved = format!(
                "{order_marker}\n{}",
                order_pattern.replace_all(body(comment), "").trim()
            );
            if reserved != body(comment) {
                self.send(
                    &format!("/issues/comments/{id}"),
                    "PATCH",
                    &json!({"body": reserved}),
                )?;
            }
            id
        } else {
            self.send(
                &summary_path,
                "POST",
                &json!({"body": format!("{order_marker}\n{SUMMARY_MARKER}")}),
            )?["id"]
                .as_u64()
                .ok_or(Error::Metadata)?
        };

        let marker = Regex::new(r"<!-- lint-gate-finding:([0-9a-f]{64}) -->").unwrap();
        let comments: BTreeMap<_, _> = review_comments
            .iter()
            .filter_map(|comment| {
                let identity = marker.captures(body(comment))?.get(1)?.as_str();
                Some((comment["id"].as_u64()?, (identity, *comment)))
            })
            .collect();
        let threads = self.threads()?;
        (self.verify)()?;
        let active = active_threads(&threads, &comments)?;
        let mut count = active
            .iter()
            .filter(|(identity, (thread, _))| blockers.contains(**identity) && !resolved(thread))
            .count();
        let hunk = Regex::new(r"^@@ -[0-9]+(?:,[0-9]+)? \+([0-9]+)(?:,([0-9]+))? @@").unwrap();

        for (identity, finding) in &blocking {
            let anchor = anchor(finding, &files, self.paths, &hunk);
            if let Some((thread, existing)) = active.get(identity.as_str()) {
                if let Some(anchor) = anchor.as_ref().filter(|anchor| {
                    existing["path"] == anchor.path && existing["line"].as_u64() == anchor.line
                }) {
                    if resolved(thread) {
                        if count >= MAX_THREADS {
                            summary.push(("Blocking error (thread limit reached)", *finding));
                            continue;
                        }
                        self.resolve(thread, false)?;
                        count += 1;
                    }
                    let text = thread_body(
                        &order_marker,
                        identity,
                        finding,
                        anchor,
                        &self.target.details_url,
                    );
                    if body(existing) != text {
                        let id = existing["id"].as_u64().ok_or(Error::Metadata)?;
                        self.send(
                            &format!("/pulls/comments/{id}"),
                            "PATCH",
                            &json!({"body": text}),
                        )?;
                    }
                    continue;
                }
                if !resolved(thread) {
                    self.resolve(thread, true)?;
                    count -= 1;
                }
            }
            if let Some(anchor) = anchor {
                if count >= MAX_THREADS {
                    summary.push(("Blocking error (thread limit reached)", *finding));
                    continue;
                }
                let text = thread_body(
                    &order_marker,
                    identity,
                    finding,
                    &anchor,
                    &self.target.details_url,
                );
                let mut payload =
                    json!({"body": text, "commit_id": self.target.head, "path": anchor.path});
                if let Some(line) = anchor.line {
                    payload["line"] = json!(line);
                    payload["side"] = json!("RIGHT");
                } else {
                    payload["subject_type"] = json!("file");
                }
                match self.send(&review_path, "POST", &payload) {
                    Ok(_) => {
                        count += 1;
                        continue;
                    }
                    Err(Error::Status {
                        status: 404 | 422, ..
                    }) => {}
                    Err(error) => return Err(error),
                }
            }
            summary.push(("Unanchored blocking error", *finding));
        }
        for (identity, (thread, _)) in active {
            if !blockers.contains(identity) && !resolved(thread) {
                self.resolve(thread, true)?;
            }
        }
        let text = summary_body(&order_marker, &self.target.details_url, &summary);
        self.send(
            &format!("/issues/comments/{summary_id}"),
            "PATCH",
            &json!({"body": text}),
        )?;
        Ok(())
    }
}

fn read_order(pattern: &Regex, text: &str) -> Option<Order> {
    let groups = pattern.captures(text)?;
    Some((
        groups[1].parse().ok()?,
        groups[2].parse().ok()?,
        groups[3].parse().ok()?,
    ))
}

fn body(comment: &Value) -> &str {
    comment["body"].as_str().unwrap_or_default()
}
fn resolved(thread: &Value) -> bool {
    thread["isResolved"] == true
}

fn active_threads<'a>(
    threads: &'a [Value],
    comments: &BTreeMap<u64, (&'a str, &'a Value)>,
) -> Result<BTreeMap<&'a str, (&'a Value, &'a Value)>> {
    let mut active = BTreeMap::new();
    for thread in threads {
        if !thread["isResolved"].is_boolean() {
            return Err(Error::Metadata);
        }
        let nodes = thread["comments"]["nodes"]
            .as_array()
            .ok_or(Error::Metadata)?;
        if let Some((identity, comment)) = nodes
            .iter()
            .filter_map(|node| comments.get(&node["databaseId"].as_u64()?))
            .min_by_key(|(identity, _)| *identity)
        {
            let entry = active.entry(*identity).or_insert((thread, *comment));
            if resolved(entry.0) && !resolved(thread) {
                *entry = (thread, comment);
            }
        }
    }
    Ok(active)
}

fn thread_body(
    marker: &str,
    identity: &str,
    finding: &Value,
    anchor: &Anchor<'_>,
    details: &str,
) -> String {
    let line = anchor
        .line
        .map_or_else(String::new, |line| format!(":{line}"));
    format!(
        "{marker}\n<!-- lint-gate-finding:{identity} -->\n**Lint Gate blocking finding**\n\nRule {} {} ({}) at {}{line}; score {}, limit {}.\n\n[Open the verified scan]({details})",
        code(&finding["rule"]),
        code(&json!(finding["name"].as_str().unwrap_or_default())),
        code(&json!(finding["severity"].as_str().unwrap_or("error"))),
        code(&json!(anchor.path)),
        code(&finding["score"]),
        code(&finding["limit"])
    )
}

fn summary_body(marker: &str, details: &str, items: &[(&str, &Value)]) -> String {
    let mut parts = vec![
        marker.to_owned(),
        SUMMARY_MARKER.to_owned(),
        "## Lint Gate findings".to_owned(),
        format!("[Open the verified scan]({details})"),
    ];
    for (heading, finding) in items.iter().take(MAX_SUMMARY) {
        let location = evidence::locations(finding)
            .iter()
            .filter_map(|item| item["path"].as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let location = if location.is_empty() {
            "location unavailable"
        } else {
            &location
        };
        let severity = finding["severity"]
            .as_str()
            .unwrap_or(if finding["status"] == "blocking" {
                "error"
            } else {
                "warn"
            });
        parts.push(format!(
            "- **{heading}** · {} · {} · {} · {} · score {}, limit {}",
            code(&finding["rule"]),
            code(&json!(severity)),
            code(&json!(finding["name"].as_str().unwrap_or_default())),
            code(&json!(location)),
            code(&finding["score"]),
            code(&finding["limit"])
        ));
    }
    if items.len() > MAX_SUMMARY {
        parts.push(format!(
            "\nShowing {MAX_SUMMARY} of {} findings; use the scan link for the full report.",
            items.len()
        ));
    } else if items.is_empty() {
        parts.push("No reported warnings or unanchored blocking findings remain.".to_owned());
    }
    parts.join("\n\n")
}
