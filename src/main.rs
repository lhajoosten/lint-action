mod analyzer;
mod grammar;
mod model;
mod policy;
mod scan;

use crate::analyzer::Analysis;
use crate::grammar::ANONYMOUS;
use crate::model::{Callable, Diagnostic, Identity, RULE, SCORING_VERSION};
use crate::policy::{BASELINE_FILE, Baseline, BaselineScores, CONFIG_FILE, Mode, Policy};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan source and write the complete JSON report to stdout.
    Scan {
        #[command(flatten)]
        settings: Settings,
        /// Optional files or directories relative to the scan root.
        paths: Vec<PathBuf>,
    },
    /// Write a baseline to stdout using the configured limits and selection.
    Baseline {
        #[command(flatten)]
        input: ConfigInput,
        /// Optional files or directories relative to the scan root.
        paths: Vec<PathBuf>,
    },
    /// Check configuration and its baseline without scanning source.
    Validate {
        #[command(flatten)]
        settings: Settings,
    },
}

#[derive(Args)]
struct ConfigInput {
    /// Source root. The default policy file is read from here.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Policy file. Defaults to <root>/.lint-action.json.
    #[arg(long)]
    config: Option<PathBuf>,
}

impl ConfigInput {
    fn config_path(&self) -> PathBuf {
        self.config
            .clone()
            .unwrap_or_else(|| self.root.join(CONFIG_FILE))
    }

    fn policy(&self) -> Result<Policy, Diagnostic> {
        Policy::load(&self.config_path())
    }
}

#[derive(Args)]
struct Settings {
    #[command(flatten)]
    input: ConfigInput,
    /// Baseline file. Baseline mode defaults to .lint-action-baseline.json beside the policy file.
    #[arg(long)]
    baseline: Option<PathBuf>,
}

impl Settings {
    fn baseline(&self, mode: Mode) -> Result<BaselineScores, Diagnostic> {
        if let Some(path) = &self.baseline {
            Baseline::load(path)
        } else if mode == Mode::Baseline {
            Baseline::load(&self.input.config_path().with_file_name(BASELINE_FILE))
        } else {
            Ok(BTreeMap::new())
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Reported,
    Baselined,
    Blocking,
}

#[derive(Serialize)]
struct Finding {
    rule: &'static str,
    #[serde(flatten)]
    callable: Callable,
    limit: u32,
    status: Status,
}

#[derive(Serialize)]
struct Report {
    format_version: u32,
    scoring_version: u32,
    mode: Option<Mode>,
    files_scanned: usize,
    files_skipped: usize,
    findings: Vec<Finding>,
    diagnostics: Vec<Diagnostic>,
}

impl Report {
    fn new() -> Self {
        Self {
            format_version: 1,
            scoring_version: SCORING_VERSION,
            mode: None,
            files_scanned: 0,
            files_skipped: 0,
            findings: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn status(&self) -> u8 {
        if !self.diagnostics.is_empty() {
            2
        } else if self
            .findings
            .iter()
            .any(|finding| matches!(finding.status, Status::Blocking))
        {
            1
        } else {
            0
        }
    }
}

fn unique(identity: &Identity, counts: &BTreeMap<Identity, usize>) -> bool {
    let mut prefix = identity.clone();
    while !prefix.scope.is_empty() {
        if prefix
            .scope
            .last()
            .is_some_and(|scope| scope.name == ANONYMOUS)
            || counts.get(&prefix) != Some(&1)
        {
            return false;
        }
        prefix.scope.pop();
    }
    true
}

fn scan(
    settings: &Settings,
    paths: &[PathBuf],
    report: &mut Report,
    validate_only: bool,
) -> Result<(), Diagnostic> {
    let policy = settings.input.policy()?;
    report.mode = Some(policy.mode);
    let baseline = settings.baseline(policy.mode)?;
    if validate_only {
        return Ok(());
    }
    let scan = scan::scan(&settings.input.root, paths, &policy);
    report.files_scanned = scan.files_scanned;
    report.files_skipped = scan.files_skipped;
    let Analysis {
        callables,
        diagnostics,
        scope_counts,
    } = scan.analysis;
    report.diagnostics = diagnostics;
    for callable in callables {
        let limit = policy
            .limit(callable.identity.language)
            .expect("scanned language is enabled");
        if callable.score <= limit {
            continue;
        }
        let status = match policy.mode {
            Mode::ReportOnly => Status::Reported,
            Mode::Baseline
                if baseline
                    .get(&callable.identity)
                    .and_then(|&score| score)
                    .is_some_and(|score| score >= callable.score)
                    && unique(&callable.identity, &scope_counts) =>
            {
                Status::Baselined
            }
            Mode::Strict | Mode::Baseline => Status::Blocking,
        };
        report.findings.push(Finding {
            rule: RULE,
            callable,
            limit,
            status,
        });
    }
    report.findings.sort_by(|left, right| {
        (
            &left.callable.identity.path,
            left.callable.range.start.line,
            left.callable.range.start.column,
        )
            .cmp(&(
                &right.callable.identity.path,
                right.callable.range.start.line,
                right.callable.range.start.column,
            ))
    });
    Ok(())
}

fn generate(
    input: &ConfigInput,
    paths: &[PathBuf],
    report: &mut Report,
) -> Result<Option<Baseline>, Diagnostic> {
    let policy = input.policy()?;
    report.mode = Some(policy.mode);
    let scan = scan::scan(&input.root, paths, &policy);
    report.files_scanned = scan.files_scanned;
    report.files_skipped = scan.files_skipped;
    let Analysis {
        callables,
        diagnostics,
        scope_counts,
    } = scan.analysis;
    report.diagnostics = diagnostics;
    if !report.diagnostics.is_empty() {
        return Ok(None);
    }
    Ok(Some(Baseline::generate(callables.iter().filter(
        |callable| {
            policy
                .limit(callable.identity.language)
                .is_some_and(|limit| callable.score > limit)
                && unique(&callable.identity, &scope_counts)
        },
    ))))
}

fn write_json(mut output: impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut output, value)?;
    writeln!(output)
}

fn run(command: Command) -> io::Result<u8> {
    let mut report = Report::new();
    match command {
        Command::Scan { settings, paths } => {
            if let Err(diagnostic) = scan(&settings, &paths, &mut report, false) {
                report.diagnostics.push(diagnostic);
            }
        }
        Command::Validate { settings } => {
            if let Err(diagnostic) = scan(&settings, &[], &mut report, true) {
                report.diagnostics.push(diagnostic);
            }
        }
        Command::Baseline { input, paths } => {
            match generate(&input, &paths, &mut report) {
                Ok(Some(baseline)) => {
                    write_json(io::stdout().lock(), &baseline)?;
                    return Ok(0);
                }
                Ok(None) => {}
                Err(diagnostic) => report.diagnostics.push(diagnostic),
            }
            write_json(io::stderr().lock(), &report)?;
            return Ok(2);
        }
    }
    write_json(io::stdout().lock(), &report)?;
    Ok(report.status())
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(status) => status.into(),
        Err(error) => {
            eprintln!("lint-action: could not write JSON: {error}");
            2.into()
        }
    }
}
