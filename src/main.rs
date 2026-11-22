mod analyzer;
mod cognitive;
mod duplication;
mod grammar;
mod model;
mod policy;
mod scan;

use crate::analyzer::Analysis;
use crate::duplication::{Duplication, Match, Pair};
use crate::grammar::ANONYMOUS;
use crate::model::{Callable, Diagnostic, Identity, Position, Rule, SCORING_VERSION};
use crate::policy::{BASELINE_FILE, Baseline, BaselineScores, CONFIG_FILE, Mode, Policy, Severity};
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
            Ok(BaselineScores::default())
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
    #[serde(flatten)]
    detail: Detail,
    limit: u32,
    severity: Severity,
    status: Status,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Detail {
    Complexity(Callable),
    Duplication(Duplication),
}

impl Detail {
    fn location(&self) -> (&str, usize, usize, Rule) {
        let (path, Position { line, column }, rule) = match self {
            Self::Complexity(item) => (&item.identity.path, item.range.start, item.rule),
            Self::Duplication(item) => (
                &item.occurrences[0].path,
                item.occurrences[0].range.start,
                item.rule,
            ),
        };
        (path, line, column, rule)
    }
}

impl Status {
    fn for_mode(mode: Mode, covered: bool) -> Self {
        match mode {
            Mode::ReportOnly => Self::Reported,
            Mode::Baseline if covered => Self::Baselined,
            Mode::Strict | Mode::Baseline => Self::Blocking,
        }
    }
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
            format_version: 2,
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
        mut diagnostics,
        scope_counts,
        duplication,
    } = scan.analysis;
    let matches = duplication_matches(&policy, &duplication, &mut diagnostics);
    report.diagnostics = diagnostics;
    for mut callable in callables {
        let severity = policy.severity(callable.rule);
        if severity == Severity::Pass {
            continue;
        }
        let Some(limit) = policy.limit(callable.identity.language, callable.rule) else {
            continue;
        };
        if callable.score <= u64::from(limit) {
            continue;
        }
        let covered = policy.mode == Mode::Baseline
            && baseline
                .complexity
                .get(&callable.rule)
                .and_then(|scores| scores.get(&callable.identity))
                .and_then(|&score| score)
                .is_some_and(|score| score >= callable.score)
            && unique(&callable.identity, &scope_counts);
        callable
            .contributions
            .sort_by_key(|part| (part.range.start.line, part.range.start.column));
        report.findings.push(Finding {
            detail: Detail::Complexity(callable),
            limit,
            severity,
            status: if severity == Severity::Warn {
                Status::Reported
            } else {
                Status::for_mode(policy.mode, covered)
            },
        });
    }
    if let Some(limit) = policy.duplication_minimum {
        let pairs = match_pairs(&matches, &scope_counts);
        for (found, pair) in matches.iter().zip(pairs) {
            let severity = policy.severity(Rule::SourceDuplication);
            if severity == Severity::Pass {
                continue;
            }
            let covered = policy.mode == Mode::Baseline
                && pair.is_some_and(|pair| baseline.covers(&pair, found));
            report.findings.push(Finding {
                detail: Detail::Duplication(found.report()),
                limit,
                severity,
                status: if severity == Severity::Warn {
                    Status::Reported
                } else {
                    Status::for_mode(policy.mode, covered)
                },
            });
        }
    }
    report
        .findings
        .sort_by(|left, right| left.detail.location().cmp(&right.detail.location()));
    Ok(())
}

fn generate(
    input: &ConfigInput,
    paths: &[PathBuf],
    report: &mut Report,
) -> Result<Option<Vec<u8>>, Diagnostic> {
    let policy = input.policy()?;
    report.mode = Some(policy.mode);
    let scan = scan::scan(&input.root, paths, &policy);
    report.files_scanned = scan.files_scanned;
    report.files_skipped = scan.files_skipped;
    let Analysis {
        callables,
        mut diagnostics,
        scope_counts,
        duplication,
    } = scan.analysis;
    let matches = duplication_matches(&policy, &duplication, &mut diagnostics);
    report.diagnostics = diagnostics;
    if !report.diagnostics.is_empty() {
        return Ok(None);
    }
    let pairs = match_pairs(&matches, &scope_counts);
    let mut baseline = Baseline::generate(
        callables.iter().filter(|callable| {
            policy.severity(callable.rule) == Severity::Error
                && policy
                    .limit(callable.identity.language, callable.rule)
                    .is_some_and(|limit| callable.score > u64::from(limit))
                && unique(&callable.identity, &scope_counts)
        }),
        pairs.into_iter().zip(&matches).filter_map(|(pair, found)| {
            (policy.severity(Rule::SourceDuplication) == Severity::Error).then_some((pair?, found))
        }),
    )?;
    baseline.preserve_advisory(&input.config_path().with_file_name(BASELINE_FILE), &policy)?;
    baseline.into_json().map(Some)
}

fn duplication_matches<'a>(
    policy: &Policy,
    sources: &'a [duplication::Source],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Match<'a>> {
    if let Some(limit) = policy
        .duplication_minimum
        .filter(|_| policy.active(Rule::SourceDuplication))
    {
        match duplication::find(sources, limit as usize) {
            Ok(matches) => return matches,
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }
    Vec::new()
}

fn match_pairs(matches: &[Match<'_>], counts: &BTreeMap<Identity, usize>) -> Vec<Option<Pair>> {
    duplication::unique_pairs(matches)
        .into_iter()
        .map(|pair| pair.filter(|pair| pair.iter().all(|identity| unique(identity, counts))))
        .collect()
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
                    io::stdout().lock().write_all(&baseline)?;
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
            eprintln!("lint-gate: could not write JSON: {error}");
            2.into()
        }
    }
}
