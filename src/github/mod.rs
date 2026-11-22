mod api;
pub mod app_report;
pub mod publisher;
mod render;
mod repository;

use crate::comparison::Comparison;
use crate::{ConfigInput, Report, Settings, compare, scan, write_json};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const CHECK: &str = "lint-gate quality gate";

fn optional(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

fn env_path(key: &str) -> Result<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{key} is required").into())
}

fn append(key: &str, text: &str) -> Result<()> {
    if let Some(path) = env::var_os(key).filter(|path| !path.is_empty()) {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(text.as_bytes())?;
    }
    Ok(())
}

fn is_sha(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn scan_action() -> u8 {
    let event = optional("GITHUB_EVENT_NAME").unwrap_or_default();
    let base = optional("LINT_GATE_PR_BASE");
    let pr = matches!(
        event.as_str(),
        "pull_request" | "pull_request_review" | "pull_request_review_comment"
    );
    let comparing = pr || base.is_some();
    match run_scan(&event, base.as_deref(), pr, comparing) {
        Ok(status) => status,
        Err(error) => {
            let message = error.to_string();
            render::annotation("error", &message, None, None);
            let comparison = if comparing {
                "\n\n**Comparison unavailable.**"
            } else {
                ""
            };
            let detail = render::summary_text(&render::truncate(&message, 500));
            if let Err(error) = append(
                "GITHUB_STEP_SUMMARY",
                &format!("## {CHECK}\n\nOutcome: **analysis failed**{comparison}\n\n{detail}\n"),
            ) {
                eprintln!("lint-gate: {error}");
            }
            2
        }
    }
}

fn run_scan(event: &str, base: Option<&str>, pr: bool, comparing: bool) -> Result<u8> {
    let workspace = fs::canonicalize(env_path("GITHUB_WORKSPACE")?)?;
    let repository = repository::path_within(&workspace)?;
    let runner_temp = env_path("RUNNER_TEMP")?;
    let temporary = tempfile::Builder::new()
        .prefix("lint-gate-policy-")
        .tempdir_in(&runner_temp)?;
    let policy =
        repository::trusted_policy(&workspace, &repository, temporary.path(), event, base, pr)?;
    let settings = Settings {
        input: ConfigInput {
            root: repository.clone(),
            config: Some(policy),
        },
        baseline: None,
    };
    let mut report = Report::new();
    let mut base_error = None;
    if comparing {
        let mut comparison = Comparison::new(
            base.filter(|sha| repository::is_object_id(sha))
                .map(str::to_owned),
            repository::candidate_revision(&repository),
        );
        let base_root = temporary.path().join("base");
        let available = match repository::materialize_base(&repository, base, &base_root) {
            Ok(()) => Some(base_root.as_path()),
            Err(error) => {
                base_error = Some(error.to_string());
                None
            }
        };
        if let Err(diagnostic) = compare(settings, available, &mut comparison, &mut report) {
            comparison.unavailable("candidate_analysis_failed");
            report.diagnostics.push(diagnostic);
        }
        report.comparison = Some(comparison);
    } else if let Err(diagnostic) = scan(&settings, &[], &mut report, false) {
        report.diagnostics.push(diagnostic);
    }
    let status = report.status();
    let mut output = tempfile::Builder::new()
        .prefix("lint-gate-report-")
        .suffix(".json")
        .tempfile_in(&runner_temp)?;
    write_json(&mut output, &report)?;
    let (_, output) = output.keep()?;
    append("GITHUB_OUTPUT", &format!("report={}\n", output.display()))?;
    render::report(
        &report,
        status,
        repository.strip_prefix(&workspace)?,
        base_error.as_deref(),
    )?;
    append("GITHUB_OUTPUT", &format!("outcome={status}\n"))?;
    Ok(status)
}
