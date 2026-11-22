use super::api::{Error, GitHub, Result};
use super::app_report::{AppReport, REPORT_LIMIT, REPORT_NAME};
use super::is_sha;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::{env, fs};

const CHECK: &str = "Lint Gate (advisory)";
const CHECK_KEY: &str = "lint-gate-advisory-v1";

#[derive(Deserialize)]
struct Repository {
    id: u64,
}

#[derive(Deserialize, PartialEq)]
struct RunIdentity {
    id: u64,
    run_attempt: u64,
    workflow_id: u64,
    head_sha: String,
    event: String,
}

#[derive(Deserialize)]
struct Event {
    workflow_run: RunIdentity,
    repository: Repository,
}

#[derive(Deserialize)]
struct Run {
    #[serde(flatten)]
    identity: RunIdentity,
    run_number: u64,
    repository: Repository,
    path: String,
    status: String,
    referenced_workflows: Option<Vec<Reference>>,
    head_repository: Option<Repository>,
    head_branch: String,
}

#[derive(Deserialize)]
struct Reference {
    path: String,
    sha: String,
    #[serde(rename = "ref")]
    branch: Option<String>,
}

struct Config {
    workflow: String,
    scanner: String,
    job: String,
    repository_id: u64,
    app_id: u64,
    repository_url: String,
}

struct Candidate {
    number: u64,
    revision: String,
    base: String,
    steps: BTreeMap<String, Option<String>>,
}

#[derive(Deserialize)]
struct Job {
    run_id: u64,
    run_attempt: Option<u64>,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    name: String,
    conclusion: Option<String>,
}

#[derive(Deserialize)]
struct PullRequest {
    number: u64,
    state: String,
    merge_commit_sha: Option<String>,
    base: PullRef,
    head: PullRef,
}

#[derive(Deserialize)]
struct PullRef {
    repo: Option<Repository>,
    sha: String,
    #[serde(rename = "ref")]
    branch: Option<String>,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
    #[serde(default)]
    parents: Vec<Commit>,
}

#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Bootstrap,
    Passed,
    Blocking,
    Incomplete,
}

fn source_matches(reference: &Reference, configured: &str) -> bool {
    let Some((path, revision)) = configured.rsplit_once('@') else {
        return false;
    };
    if is_sha(revision, 40) {
        return reference.path == configured && reference.sha == revision;
    }
    revision.strip_prefix("refs/heads/").is_some_and(|branch| {
        (reference.path == configured || reference.path == format!("{path}@{branch}"))
            && reference.branch.as_deref() == Some(revision)
            && is_sha(&reference.sha, 40)
    })
}

fn verified_run(api: &GitHub, event: &Event, config: &Config) -> Result<Run> {
    let incoming = &event.workflow_run;
    let id = incoming.id;
    if id == 0 || incoming.run_attempt == 0 {
        return Err(Error::skip(
            "The completed event has no usable run identity.",
        ));
    }
    let workflow: Repository = api.json(&format!("/actions/workflows/{}", config.workflow))?;
    let run: Run = api.json(&format!("/actions/runs/{id}"))?;
    if run.identity.run_attempt != incoming.run_attempt || run.status != "completed" {
        return Err(Error::skip(
            "The source run has advanced to a different attempt.",
        ));
    }
    if *incoming != run.identity
        || event.repository.id != config.repository_id
        || run.repository.id != config.repository_id
        || run.identity.workflow_id != workflow.id
        || run.path.split('@').next() != Some(&format!(".github/workflows/{}", config.workflow))
        || run.identity.event != "pull_request"
    {
        return Err(Error::skip(
            "The completed run is outside the configured PR analysis workflow.",
        ));
    }
    let matches = run
        .referenced_workflows
        .iter()
        .flatten()
        .filter(|item| source_matches(item, &config.scanner))
        .count();
    if matches != 1 {
        return Err(Error::skip(
            "The run did not identify the configured reusable scanner revision.",
        ));
    }
    Ok(run)
}

fn candidate_job(api: &GitHub, run: &RunIdentity, name: &str) -> Result<Candidate> {
    let path = format!("/actions/runs/{}/attempts/{}/jobs", run.id, run.run_attempt);
    let jobs = api.pages(&path, "jobs")?;
    let mut matching = jobs.into_iter().filter(|job| job["name"] == name);
    let Some(job) = matching.next().filter(|_| matching.next().is_none()) else {
        return Err(Error::skip(
            "The source attempt did not identify a single analysis job.",
        ));
    };
    let job: Job = serde_json::from_value(job).map_err(|_| Error::Metadata)?;
    if job.run_id != run.id || job.run_attempt.unwrap_or(run.run_attempt) != run.run_attempt {
        return Err(Error::skip(
            "The analysis job belongs to a different run attempt.",
        ));
    }
    let steps: BTreeMap<_, _> = job
        .steps
        .into_iter()
        .map(|step| (step.name, step.conclusion))
        .collect();
    let mut names = steps.iter().filter_map(|(name, conclusion)| {
        (conclusion.as_deref() == Some("success"))
            .then_some(name)?
            .strip_prefix("Record candidate ")
    });
    let invalid = || Error::skip("The analysis job did not record a verifiable candidate.");
    let name = names
        .next()
        .filter(|_| names.next().is_none())
        .ok_or_else(invalid)?;
    let pattern = Regex::new(r"\A([1-9][0-9]*) ([0-9a-f]{40}) ([0-9a-f]{40})\z").unwrap();
    let fields = pattern.captures(name).ok_or_else(invalid)?;
    Ok(Candidate {
        number: fields[1].parse().map_err(|_| invalid())?,
        revision: fields[2].to_owned(),
        base: fields[3].to_owned(),
        steps,
    })
}

fn target_head(
    api: &GitHub,
    run: &Run,
    candidate: &Candidate,
    repository_id: u64,
) -> Result<String> {
    let Candidate {
        number,
        revision,
        base,
        ..
    } = candidate;
    let pr: PullRequest = api.json(&format!("/pulls/{number}"))?;
    let head = pr.head;
    if pr.number != *number
        || pr.state != "open"
        || pr.base.repo.map(|repo| repo.id) != Some(repository_id)
        || pr.base.sha != *base
        || pr.merge_commit_sha.as_ref() != Some(revision)
        || head.repo.is_none()
        || head.repo.map(|repo| repo.id) != run.head_repository.as_ref().map(|repo| repo.id)
        || head.branch.as_deref() != Some(run.head_branch.as_str())
        || !is_sha(&head.sha, 40)
        || (run.identity.head_sha != head.sha && run.identity.head_sha != *revision)
    {
        return Err(Error::skip(
            "The recorded synthetic merge no longer matches the open PR and source run.",
        ));
    }
    let commit: Commit = api.json(&format!("/git/commits/{revision}"))?;
    if commit.sha != *revision
        || !commit
            .parents
            .iter()
            .map(|parent| &parent.sha)
            .eq([base, &head.sha])
    {
        return Err(Error::skip(
            "GitHub could not bind the synthetic merge to the PR head and base.",
        ));
    }
    Ok(head.sha)
}

fn scan_outcome(steps: &BTreeMap<String, Option<String>>) -> Outcome {
    let step = |name| steps.get(name).and_then(Option::as_deref);
    match step("Scan source") {
        Some("skipped") if step("Record policy bootstrap") == Some("success") => Outcome::Bootstrap,
        Some("success") => Outcome::Passed,
        Some("failure") if step("Record policy violations") == Some("success") => Outcome::Blocking,
        _ => Outcome::Incomplete,
    }
}

// The zip reader indexes by filename and collapses duplicate central entries.
fn single_central_entry(archive: &[u8], start: u64) -> bool {
    let Some(directory) = usize::try_from(start)
        .ok()
        .and_then(|start| archive.get(start..))
    else {
        return false;
    };
    // The fixed ZIP central header is followed by the filename, extra data, and comment.
    let Some(header) = directory.get(..46) else {
        return false;
    };
    let next = [28, 30, 32].into_iter().fold(46, |size, offset| {
        size + usize::from(u16::from_le_bytes([header[offset], header[offset + 1]]))
    });
    directory
        .get(next..)
        .is_some_and(|rest| !rest.starts_with(b"PK\x01\x02"))
}

fn unpack_report(archive: Vec<u8>) -> super::Result<AppReport> {
    if archive.len() > REPORT_LIMIT {
        return Err("Report archive exceeds the size limit.".into());
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(&archive))?;
    if zip.len() != 1 || !single_central_entry(&archive, zip.central_directory_start()) {
        return Err("Report archive has an unexpected file or size.".into());
    }
    let entry = zip.by_index(0)?;
    if entry.name() != REPORT_NAME || entry.size() > REPORT_LIMIT as u64 {
        return Err("Report archive has an unexpected file or size.".into());
    }
    let mut data = Vec::new();
    entry.take(REPORT_LIMIT as u64 + 1).read_to_end(&mut data)?;
    if data.len() > REPORT_LIMIT {
        return Err("Report exceeds the size limit.".into());
    }
    Ok(serde_json::from_slice(&data)?)
}

type ReportResult = std::result::Result<Option<AppReport>, &'static str>;

fn load_report(api: &GitHub, run: &Run, revision: &str, outcome: Outcome) -> ReportResult {
    if outcome == Outcome::Bootstrap {
        return Ok(None);
    }
    let RunIdentity {
        id,
        run_attempt,
        head_sha,
        ..
    } = &run.identity;
    let name = format!("lint-gate-report-{id}-{run_attempt}");
    let path = format!("/actions/runs/{id}/artifacts?name={name}");
    let unusable = "The analysis report is unusable.";
    let artifacts = api.pages(&path, "artifacts").map_err(|_| unusable)?;
    let mut matching = artifacts.iter().filter(|item| item["name"] == name);
    let Some(artifact) = matching.next() else {
        return Err("The analysis report is missing.");
    };
    let metadata = &artifact["workflow_run"];
    let id = artifact["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or(unusable)?;
    if matching.next().is_some()
        || artifact["expired"] != false
        || !artifact["size_in_bytes"]
            .as_u64()
            .is_some_and(|size| (1..=REPORT_LIMIT as u64).contains(&size))
        || metadata["id"] != run.identity.id
        || metadata["repository_id"] != run.repository.id
        || metadata["head_sha"] != *head_sha
    {
        return Err(unusable);
    }
    let report = unpack_report(api.artifact(id).map_err(|_| unusable)?).map_err(|_| unusable)?;
    let passed = match outcome {
        Outcome::Passed => Some(true),
        Outcome::Blocking => Some(false),
        _ => None,
    };
    report
        .validate(run.identity.id, *run_attempt, revision, passed)
        .map_err(|_| unusable)?;
    Ok(Some(report))
}

fn literal_sample(sample: &str) -> String {
    use std::fmt::Write;
    let quoted = serde_json::to_string(sample).unwrap();
    let mut literal = String::new();
    for unit in quoted.encode_utf16() {
        if unit == 0x60 || unit >= 0x7f {
            write!(literal, "\\u{unit:04x}").unwrap();
        } else {
            literal.push(char::from(unit as u8));
        }
    }
    literal
}

fn check_output(
    outcome: Outcome,
    report: ReportResult,
    revision: &str,
    target: &str,
    details_url: &str,
) -> Value {
    let (mut title, mut conclusion) = match outcome {
        Outcome::Bootstrap => ("Policy bootstrap", "skipped"),
        Outcome::Passed => ("Clean", "success"),
        Outcome::Blocking => ("Blocking policy findings", "neutral"),
        Outcome::Incomplete => ("Analysis incomplete", "neutral"),
    };
    let mut summary = vec![
        format!("[Open the analysis run]({details_url})"),
        format!("PR head `{target}` · GitHub synthetic merge tree `{revision}`"),
    ];
    if outcome == Outcome::Bootstrap {
        summary.push(
            "The PR base has no Lint Gate policy yet. The scan was skipped for bootstrap."
                .to_owned(),
        );
    }
    match report {
        Err(error) => {
            summary.push(error.to_owned());
            if outcome == Outcome::Passed {
                (title, conclusion) = ("Report unavailable", "neutral");
            }
        }
        Ok(Some(report)) => {
            let counts = report.counts;
            if outcome == Outcome::Passed && (counts.reported != 0 || counts.baselined != 0) {
                (title, conclusion) = ("Warnings", "neutral");
            }
            summary.push(format!(
                "Files scanned {} · Analysis errors {} · Reported {} · Baselined {} · Blocking {}",
                report.files, report.errors, counts.reported, counts.baselined, counts.blocking
            ));
            if !report.samples.is_empty() {
                let samples = report
                    .samples
                    .iter()
                    .map(|sample| literal_sample(sample))
                    .collect::<Vec<_>>()
                    .join("\n");
                summary.push(format!("```text\n{samples}\n```"));
            }
        }
        Ok(None) => {}
    }
    json!({
        "name": CHECK, "status": "completed", "conclusion": conclusion, "details_url": details_url,
        "output": {"title": title, "summary": summary.join("\n\n")},
    })
}

fn check_matches(check: &Value, app_id: u64, target: &str) -> bool {
    check["name"] == CHECK && check["head_sha"] == target && check["app"]["id"] == app_id
}

fn upsert(
    api: &GitHub,
    app_id: u64,
    run: &Run,
    target: &str,
    mut payload: Value,
) -> Result<String> {
    let prefix = format!("{CHECK_KEY}:{}:{target}:", run.repository.id);
    let order = (run.run_number, run.identity.run_attempt);
    let path = format!(
        "/commits/{target}/check-runs?check_name=Lint+Gate+%28advisory%29&app_id={app_id}&filter=all"
    );
    let checks = api.pages(&path, "check_runs")?;
    let existing = checks
        .iter()
        .filter(|check| check_matches(check, app_id, target))
        .filter_map(|check| {
            let version = check["external_id"].as_str()?.strip_prefix(&prefix)?;
            let parts: Vec<_> = version.split(':').collect();
            let [number, attempt, id] = parts.as_slice() else {
                return None;
            };
            if [number, attempt, id]
                .iter()
                .any(|part| !part.bytes().all(|byte| byte.is_ascii_digit()))
            {
                return None;
            }
            let number = number.parse::<u64>().ok()?;
            let attempt = attempt.parse::<u64>().ok()?;
            let id = id.parse::<u64>().ok()?;
            Some(((number, attempt), id, check["id"].as_u64()?))
        })
        .max();
    payload["external_id"] = json!(format!(
        "{prefix}{}:{}:{}",
        order.0, order.1, run.identity.id
    ));
    let check = if let Some((recorded, run_id, check_id)) = existing {
        if recorded > order || (recorded == order && run_id == run.identity.id) {
            return Ok("The advisory check already holds this result or a newer one.".to_owned());
        }
        api.send(&format!("/check-runs/{check_id}"), "PATCH", Some(&payload))?
    } else {
        payload["head_sha"] = json!(target);
        api.send("/check-runs", "POST", Some(&payload))?
    };
    if !check_matches(&check, app_id, target) {
        return Err(Error::Metadata);
    }
    Ok(format!("Published {CHECK} on PR head {target}."))
}

fn publish(api: &GitHub, checks: &GitHub, event: &Event, config: &Config) -> Result<String> {
    let run = verified_run(api, event, config)?;
    let candidate = candidate_job(api, &run.identity, &config.job)?;
    let target = match target_head(api, &run, &candidate, config.repository_id) {
        Err(Error::Status { status: 404, .. }) => {
            return Err(Error::skip(
                "GitHub can no longer resolve the PR or its synthetic merge.",
            ));
        }
        result => result?,
    };
    let outcome = scan_outcome(&candidate.steps);
    let report = load_report(api, &run, &candidate.revision, outcome);
    let details_url = format!(
        "{}/actions/runs/{}/attempts/{}",
        config.repository_url, run.identity.id, run.identity.run_attempt
    );
    let payload = check_output(outcome, report, &candidate.revision, &target, &details_url);
    match upsert(checks, config.app_id, &run, &target, payload) {
        Err(Error::Status {
            operation,
            status: status @ (404 | 422),
        }) => Err(Error::skip(format!(
            "GitHub could not attach the check to this PR head. {} Verify the installation has Checks write for this repository.",
            Error::status_message(&operation, status)
        ))),
        result => result,
    }
}

fn required(key: &str) -> Result<String> {
    env::var(key).map_err(|_| Error::Metadata)
}

fn from_environment() -> Result<String> {
    let repository = required("GITHUB_REPOSITORY")?;
    let scanner = required("SCANNER_WORKFLOW")?;
    let workflow = required("SOURCE_WORKFLOW")?;
    if !Regex::new(r"\A[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+\z").unwrap().is_match(&repository)
        || !Regex::new(r"\A[a-zA-Z0-9_.-]+\.ya?ml\z").unwrap().is_match(&workflow)
        || !Regex::new(r"\A[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+/\.github/workflows/source-complexity\.yml@(?:[0-9a-f]{40}|refs/heads/[\w./-]+)\z").unwrap().is_match(&scanner)
    {
        return Err(Error::Metadata);
    }
    let config = Config {
        workflow,
        scanner,
        job: required("ANALYSIS_JOB")?,
        repository_id: required("GITHUB_REPOSITORY_ID")?
            .parse()
            .map_err(|_| Error::Metadata)?,
        app_id: required("APP_ID")?.parse().map_err(|_| Error::Metadata)?,
        repository_url: format!("{}/{repository}", required("GITHUB_SERVER_URL")?),
    };
    let event = fs::read(env::var_os("GITHUB_EVENT_PATH").ok_or(Error::Metadata)?)
        .map_err(|_| Error::Metadata)?;
    let event = serde_json::from_slice(&event).map_err(|_| Error::Metadata)?;
    let base_url = required("GITHUB_API_URL")?;
    let api = GitHub::new(&base_url, &repository, required("GITHUB_TOKEN")?);
    let checks = GitHub::new(&base_url, &repository, required("APP_TOKEN")?);
    publish(&api, &checks, &event, &config)
}

pub fn publish_action() -> u8 {
    match from_environment() {
        Ok(message) => println!("{message}"),
        Err(Error::Skip(message)) => println!("Skipped App publication. {message}"),
        Err(error) => {
            match error {
                Error::Status { operation, status } => eprintln!(
                    "{} Verify the workflow read permissions and the App installation's Checks write permission.",
                    Error::status_message(&operation, status)
                ),
                Error::Network => {
                    eprintln!("GitHub could not be reached. Rerun the publisher to retry.")
                }
                _ => eprintln!(
                    "Unexpected GitHub metadata or publishing configuration. Verify the publisher inputs."
                ),
            }
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    const REVISION: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn existing_report() -> Value {
        // Version 1 report shape emitted by the Python scanner wrapper.
        json!({
            "version": 1, "run_id": 100, "attempt": 1, "revision": REVISION,
            "files": 3, "errors": 0, "counts": {"blocking": 0, "reported": 0, "baselined": 0},
            "samples": ["```\n<a href='https://example.invalid'>@everyone é</a>"],
        })
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn legacy_reports_keep_clean_warning_blocking_and_literal_output() {
        for (outcome, count, title, conclusion) in [
            (Outcome::Passed, None, "Clean", "success"),
            (Outcome::Passed, Some("baselined"), "Warnings", "neutral"),
            (
                Outcome::Blocking,
                Some("blocking"),
                "Blocking policy findings",
                "neutral",
            ),
            (Outcome::Incomplete, None, "Analysis incomplete", "neutral"),
        ] {
            let mut legacy = existing_report();
            if let Some(count) = count {
                legacy["counts"][count] = json!(1);
            }
            if outcome == Outcome::Incomplete {
                legacy["errors"] = json!(1);
            }
            let compressed = archive(&[(REPORT_NAME, legacy.to_string().as_bytes())]);
            let report = unpack_report(compressed).unwrap();
            let passed = match outcome {
                Outcome::Passed => Some(true),
                Outcome::Blocking => Some(false),
                _ => None,
            };
            report.validate(100, 1, REVISION, passed).unwrap();
            let output = check_output(
                outcome,
                Ok(Some(report)),
                REVISION,
                REVISION,
                "https://example.invalid",
            );
            assert_eq!(output["conclusion"], conclusion);
            assert_eq!(output["output"]["title"], title);
            let summary = output["output"]["summary"].as_str().unwrap();
            assert_eq!(summary.matches("```").count(), 2);
            assert!(summary.contains(r"\u0060\u0060\u0060\n<a"));
            assert!(summary.contains(r"\u00e9"));
        }
    }

    #[test]
    fn rejects_unusable_reports() {
        let report = existing_report().to_string();
        let bytes = report.as_bytes();
        let mut duplicate = archive(&[(REPORT_NAME, bytes), ("second.json", bytes)]);
        // Give both entries the same name without changing their header lengths.
        while let Some(start) = duplicate
            .windows(REPORT_NAME.len())
            .position(|name| name == b"second.json")
        {
            duplicate[start..start + REPORT_NAME.len()].copy_from_slice(REPORT_NAME.as_bytes());
        }
        for malformed in [
            b"invalid zip".to_vec(),
            duplicate,
            archive(&[("other.json", bytes)]),
            archive(&[(REPORT_NAME, bytes), ("other.json", bytes)]),
            archive(&[(REPORT_NAME, &vec![b' '; REPORT_LIMIT + 1])]),
            vec![0; REPORT_LIMIT + 1],
        ] {
            assert!(unpack_report(malformed).is_err());
        }
        for (key, field) in [
            ("revision", json!("wrong")),
            ("run_id", json!(99)),
            ("attempt", json!(3)),
            ("samples", json!(["x".repeat(301)])),
            ("errors", json!(1)),
        ] {
            let mut invalid = existing_report();
            invalid[key] = field;
            let invalid =
                unpack_report(archive(&[(REPORT_NAME, invalid.to_string().as_bytes())])).unwrap();
            assert!(
                invalid.validate(100, 1, REVISION, Some(true)).is_err(),
                "{key}"
            );
        }
    }
}
