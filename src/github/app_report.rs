use super::{Result, env_path, render::truncate};
use crate::model::Range;
use serde::{Deserialize, Serialize};
use std::{env, fs};

pub const REPORT_NAME: &str = "report.json";
pub const REPORT_LIMIT: usize = 256 * 1024;
const SAMPLE_LIMIT: usize = 8;
const SAMPLE_LENGTH: usize = 300;
const STATUSES: [&str; 3] = ["blocking", "reported", "baselined"];

#[derive(Deserialize)]
struct ScanReport {
    files_scanned: u64,
    diagnostics: Vec<Diagnostic>,
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
struct Diagnostic {
    kind: String,
    message: String,
}

#[derive(Deserialize)]
struct Location {
    path: String,
    range: Range,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Locations {
    Duplication { occurrences: [Location; 2] },
    Single(Location),
}

#[derive(Deserialize)]
struct Finding {
    rule: String,
    status: String,
    score: u64,
    limit: u64,
    #[serde(flatten)]
    locations: Locations,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub blocking: u64,
    pub reported: u64,
    pub baselined: u64,
}

#[derive(Deserialize, Serialize)]
pub struct AppReport {
    pub version: u64,
    pub run_id: u64,
    pub attempt: u64,
    pub revision: String,
    pub files: u64,
    pub errors: u64,
    pub counts: Counts,
    pub samples: Vec<String>,
}

impl AppReport {
    pub fn validate(
        &self,
        run_id: u64,
        attempt: u64,
        revision: &str,
        passed: Option<bool>,
    ) -> Result<()> {
        if (
            self.version,
            self.run_id,
            self.attempt,
            self.revision.as_str(),
        ) != (1, run_id, attempt, revision)
        {
            return Err("Report identity mismatch.".into());
        }
        let Counts {
            blocking,
            reported,
            baselined,
        } = self.counts;
        if [
            self.files,
            self.errors,
            run_id,
            attempt,
            blocking,
            reported,
            baselined,
        ]
        .into_iter()
        .any(|count| count > i64::MAX as u64)
        {
            return Err("Invalid report count or identity.".into());
        }
        if self.samples.len() > SAMPLE_LIMIT
            || self
                .samples
                .iter()
                .any(|sample| sample.chars().count() > SAMPLE_LENGTH)
        {
            return Err("Invalid report samples.".into());
        }
        if let Some(passed) = passed
            && (self.errors != 0 || (blocking == 0) != passed)
        {
            return Err("Report disagrees with the recorded scan outcome.".into());
        }
        Ok(())
    }
}

pub fn normalize_action() -> Result<()> {
    let report: ScanReport = serde_json::from_slice(&fs::read(env_path("SCAN_REPORT")?)?)?;
    let mut samples: Vec<_> = report
        .diagnostics
        .iter()
        .take(SAMPLE_LIMIT)
        .map(|item| truncate(&format!("{}: {}", item.kind, item.message), SAMPLE_LENGTH))
        .collect();
    let [blocking, reported, baselined] = STATUSES.map(|status| {
        let mut count = 0;
        for item in report.findings.iter().filter(|item| item.status == status) {
            count += 1;
            if samples.len() < SAMPLE_LIMIT {
                let location = match &item.locations {
                    Locations::Duplication { occurrences } => &occurrences[0],
                    Locations::Single(location) => location,
                };
                let sample = format!(
                    "{}:{} ({}, {status}, score {}, limit {})",
                    location.path, location.range.start.line, item.rule, item.score, item.limit
                );
                samples.push(truncate(&sample, SAMPLE_LENGTH));
            }
        }
        count
    });
    let normalized = AppReport {
        version: 1,
        run_id: env::var("GITHUB_RUN_ID")?.parse()?,
        attempt: env::var("GITHUB_RUN_ATTEMPT")?.parse()?,
        revision: env::var("GITHUB_SHA")?,
        files: report.files_scanned,
        errors: report.diagnostics.len() as u64,
        counts: Counts {
            blocking,
            reported,
            baselined,
        },
        samples,
    };
    write_app_report(&normalized)
}

fn write_app_report(normalized: &AppReport) -> Result<()> {
    let directory = env_path("RUNNER_TEMP")?.join("lint-gate-app");
    fs::create_dir(&directory)?;
    fs::write(directory.join(REPORT_NAME), serde_json::to_vec(normalized)?)?;
    Ok(())
}
