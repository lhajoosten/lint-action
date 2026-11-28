use super::{Result, append, env_path, is_sha, repository};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

const LIMIT: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u8,
    pub revision: String,
    pub base: String,
    pub head: String,
    pub source_run: String,
    pub source_attempt: String,
    pub artifact_name: String,
    pub compatible_paths: BTreeSet<String>,
}

pub fn action(run: impl FnOnce() -> Result<u8>) -> u8 {
    match run() {
        Ok(status) => status,
        Err(error) => {
            eprintln!("lint-gate: {error}");
            2
        }
    }
}

fn revision(key: &str) -> Result<String> {
    let value = env::var(key)?;
    if !is_sha(&value, 40) {
        return Err(format!("{key} must be a full Git SHA").into());
    }
    Ok(value)
}

pub fn rescan(analyzer: &Path) -> Result<u8> {
    let workspace = fs::canonicalize(env_path("GITHUB_WORKSPACE")?)?;
    let candidate = repository::path_within(&workspace)?;
    let merge = revision("VERIFIED_REVISION")?;
    let head = repository::output(&candidate, &["rev-parse", "--verify", "HEAD"])?;
    if !head.status.success() || str::from_utf8(&head.stdout)?.trim() != merge {
        return Err("the checked-out candidate is not the verified synthetic merge".into());
    }
    run_verified_scan(analyzer, &candidate)
}

fn run_verified_scan(analyzer: &Path, candidate: &Path) -> Result<u8> {
    let runner = env_path("RUNNER_TEMP")?;
    let policy = tempfile::tempdir_in(&runner)?;
    repository::copy_policy(candidate, &revision("VERIFIED_BASE")?, policy.path())?;
    // This is the CLI built from the verified scanner revision. Its stable scan command
    // also works with provider releases that predate the Rust GitHub integration.
    let report = tempfile::Builder::new()
        .prefix("lint-gate-report-")
        .suffix(".json")
        .tempfile_in(&runner)?;
    let status = Command::new(analyzer)
        .arg("scan")
        .arg("--root")
        .arg(candidate)
        .arg("--config")
        .arg(policy.path().join(crate::policy::CONFIG_FILE))
        .stdout(report.reopen()?)
        .status()?;
    let status = match status.code() {
        Some(code @ (0..=2)) => code,
        _ => return Err("verified scanner exited unexpectedly".into()),
    };
    save_scan_report(report, status)
}

fn save_scan_report(mut report: tempfile::NamedTempFile, status: i32) -> Result<u8> {
    report.flush()?;
    let (_, path) = report.keep()?;
    append(
        "GITHUB_OUTPUT",
        &format!("report={}\noutcome={status}\n", path.display()),
    )?;
    Ok(status as u8)
}

pub fn bounded_json(path: &Path) -> Result<(Value, Vec<u8>)> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err("trusted scan evidence exceeded its size limit".into());
    }
    Ok((serde_json::from_slice(&bytes)?, bytes))
}

pub fn findings(report: &Value) -> Option<&[Value]> {
    let version = report["format_version"].as_u64()?;
    if !matches!(version, 1..=3)
        || report["scoring_version"].as_i64().is_none()
        || report["files_scanned"].as_u64().is_none()
        || report["diagnostics"]
            .as_array()
            .is_none_or(|items| !items.is_empty())
    {
        return None;
    }
    let findings = report["findings"].as_array()?;
    if !findings
        .iter()
        .all(|finding| valid_finding(finding, version))
    {
        return None;
    }
    Some(findings)
}

fn valid_finding(finding: &Value, version: u64) -> bool {
    let Some(status) = finding["status"].as_str() else {
        return false;
    };
    let severity = finding["severity"].as_str();
    if !valid_severity(finding, version, status, severity) || !valid_metrics(finding, version) {
        return false;
    }
    match finding["rule"].as_str() {
        Some("source-duplication") => finding["occurrences"]
            .as_array()
            .is_some_and(|items| items.len() == 2),
        Some(_) => ["path", "range", "name", "scope", "language"]
            .iter()
            .all(|key| finding.get(key).is_some()),
        None => false,
    }
}

fn valid_metrics(finding: &Value, version: u64) -> bool {
    if finding["rule"] != "finally-exit" {
        return finding["score"].as_i64().is_some() && finding["limit"].as_i64().is_some();
    }
    version == 3
        && finding.get("score").is_none()
        && finding.get("limit").is_none()
        && ["path", "language", "name", "transfer", "message"]
            .iter()
            .all(|key| finding[*key].as_str().is_some_and(|text| !text.is_empty()))
        && finding["scope"].is_array()
        && serde_json::from_value::<crate::model::Range>(finding["range"].clone()).is_ok_and(
            |range| {
                range.start.line > 0
                    && range.start.column > 0
                    && range.end.line > 0
                    && range.end.column > 0
            },
        )
}

fn valid_severity(finding: &Value, version: u64, status: &str, severity: Option<&str>) -> bool {
    !(!matches!(status, "reported" | "baselined" | "blocking")
        || (version >= 2 || !finding["severity"].is_null())
            && !matches!(severity, Some("error" | "warn" | "pass"))
        || matches!(status, "blocking" | "baselined") && !matches!(severity, None | Some("error"))
        || status == "reported" && severity == Some("pass"))
}

pub fn locations(finding: &Value) -> &[Value] {
    if finding["rule"] == "source-duplication" {
        finding["occurrences"].as_array().unwrap()
    } else {
        std::slice::from_ref(finding)
    }
}

fn blob(repository: &Path, revision: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let output = repository::output(
        repository,
        &["rev-parse", "--verify", &format!("{revision}:{path}")],
    )?;
    Ok(output.status.success().then_some(output.stdout))
}

pub fn stage() -> Result<u8> {
    let (report, bytes) = bounded_json(&env_path("SCAN_REPORT")?)?;
    let findings = findings(&report).ok_or("trusted scan report is incomplete or invalid")?;
    let workspace = fs::canonicalize(env_path("GITHUB_WORKSPACE")?)?;
    let candidate = repository::path_within(&workspace)?;
    let revision = revision("VERIFIED_REVISION")?;
    let head = self::revision("VERIFIED_HEAD")?;
    let base = self::revision("VERIFIED_BASE")?;
    let compatible_paths = compatible_paths(findings, &candidate, &revision, &head)?;
    write_stage(bytes, revision, head, base, compatible_paths)
}

fn compatible_paths(
    findings: &[Value],
    candidate: &Path,
    revision: &str,
    head: &str,
) -> Result<BTreeSet<String>> {
    let paths: BTreeSet<_> = findings
        .iter()
        .flat_map(locations)
        .filter_map(|item| item["path"].as_str())
        .collect();
    let mut compatible_paths = BTreeSet::new();
    for path in paths {
        if let Some(merge) = blob(candidate, revision, path)?
            && blob(candidate, head, path)?.as_ref() == Some(&merge)
        {
            compatible_paths.insert(path.to_owned());
        }
    }
    Ok(compatible_paths)
}

fn write_stage(
    bytes: Vec<u8>,
    revision: String,
    head: String,
    base: String,
    compatible_paths: BTreeSet<String>,
) -> Result<u8> {
    let run: u64 = env::var("GITHUB_RUN_ID")?.parse()?;
    let attempt: u64 = env::var("GITHUB_RUN_ATTEMPT")?.parse()?;
    let manifest = Manifest {
        format_version: 1,
        revision,
        head,
        base,
        source_run: env::var("SOURCE_RUN")?,
        source_attempt: env::var("SOURCE_ATTEMPT")?,
        artifact_name: format!("lint-gate-verified-findings-{run}-{attempt}"),
        compatible_paths,
    };
    write_manifest(bytes, manifest)
}

fn write_manifest(bytes: Vec<u8>, manifest: Manifest) -> Result<u8> {
    let output = env_path("RUNNER_TEMP")?.join("lint-gate-findings");
    fs::create_dir_all(&output)?;
    fs::write(output.join("report.json"), bytes)?;
    fs::write(output.join("anchors.json"), serde_json::to_vec(&manifest)?)?;
    append(
        "GITHUB_OUTPUT",
        &format!("artifact_name={}\n", manifest.artifact_name),
    )?;
    Ok(0)
}
