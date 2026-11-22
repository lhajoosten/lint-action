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
    /// Propose safe reductions to an existing baseline as a JSON Patch.
    BaselineReview {
        #[command(flatten)]
        settings: Settings,
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
    fn baseline_path(&self) -> PathBuf {
        self.baseline
            .clone()
            .unwrap_or_else(|| self.input.config_path().with_file_name(BASELINE_FILE))
    }

    fn baseline(&self, mode: Mode) -> Result<BaselineScores, Diagnostic> {
        if let Some(path) = &self.baseline {
            Baseline::load(path)
        } else if mode == Mode::Baseline {
            Baseline::load(&self.baseline_path())
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
        mut diagnostics,
        scope_counts,
        duplication,
    } = scan.analysis;
    let matches = duplication_matches(&policy, &duplication, &mut diagnostics);
    report.diagnostics = diagnostics;
    for mut callable in callables {
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
            status: Status::for_mode(policy.mode, covered),
        });
    }
    if let Some(limit) = policy.duplication_minimum {
        let pairs = match_pairs(&matches, &scope_counts);
        for (found, pair) in matches.iter().zip(pairs) {
            let covered = policy.mode == Mode::Baseline
                && pair.is_some_and(|pair| baseline.covers(&pair, found));
            report.findings.push(Finding {
                detail: Detail::Duplication(found.report()),
                limit,
                status: Status::for_mode(policy.mode, covered),
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
    let baseline = Baseline::generate(
        callables.iter().filter(|callable| {
            policy
                .limit(callable.identity.language, callable.rule)
                .is_some_and(|limit| callable.score > u64::from(limit))
                && unique(&callable.identity, &scope_counts)
        }),
        pairs
            .into_iter()
            .zip(&matches)
            .filter_map(|(pair, found)| Some((pair?, found))),
    )?;
    baseline.to_json().map(Some)
}

#[derive(Serialize)]
struct PatchOperation {
    op: &'static str,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<serde_json::Value>,
}

fn patch_value(op: &'static str, path: String, value: serde_json::Value) -> PatchOperation {
    PatchOperation {
        op,
        path,
        value: Some(value),
    }
}

fn patch_remove(path: String) -> PatchOperation {
    PatchOperation {
        op: "remove",
        path,
        value: None,
    }
}

fn review_baseline(
    settings: &Settings,
    report: &mut Report,
) -> Result<Option<Vec<PatchOperation>>, Diagnostic> {
    let policy = settings.input.policy()?;
    report.mode = Some(policy.mode);
    let baseline_path = settings.baseline_path();
    let (baseline, _) = Baseline::load_review(&baseline_path)?;
    let scanned = scan::scan(&settings.input.root, &[], &policy);
    report.files_scanned = scanned.files_scanned;
    report.files_skipped = scanned.files_skipped;
    let Analysis {
        callables,
        mut diagnostics,
        scope_counts,
        duplication,
    } = scanned.analysis;
    let matches = duplication_matches(&policy, &duplication, &mut diagnostics);
    report.diagnostics = diagnostics;
    if !report.diagnostics.is_empty() {
        return Ok(None);
    }

    let mut operations = Vec::new();
    let mut entry_indices: BTreeMap<(Rule, Identity), Vec<usize>> = BTreeMap::new();
    for (index, entry) in baseline.entries.iter().enumerate() {
        entry_indices
            .entry((entry.rule, entry.identity.clone()))
            .or_default()
            .push(index);
    }
    let mut current = BTreeMap::new();
    for callable in &callables {
        current.insert((callable.rule, callable.identity.clone()), callable.score);
    }
    for ((rule, identity), indices) in entry_indices {
        if policy.limit(identity.language, rule).is_none() {
            eprintln!(
                "baseline-review: deferred inactive complexity rule {:?} for {}",
                rule, identity.path
            );
            continue;
        }
        if !policy.includes(std::path::Path::new(&identity.path)) {
            eprintln!(
                "baseline-review: deferred complexity entry outside policy selection for {}",
                identity.path
            );
            continue;
        }
        if indices.len() != 1 {
            eprintln!(
                "baseline-review: deferred ambiguous complexity entries for {} ({:?})",
                identity.path, identity.scope
            );
            continue;
        }
        let index = indices[0];
        match current.get(&(rule, identity.clone())) {
            Some(&score)
                if unique(&identity, &scope_counts)
                    && policy
                        .limit(identity.language, rule)
                        .is_some_and(|limit| score <= u64::from(limit)) =>
            {
                operations.push(patch_remove(format!("/entries/{index}")));
            }
            Some(&score)
                if unique(&identity, &scope_counts) && score < baseline.entries[index].score =>
            {
                operations.push(patch_value(
                    "replace",
                    format!("/entries/{index}/score"),
                    serde_json::json!(score),
                ));
            }
            Some(_) if !unique(&identity, &scope_counts) => {
                eprintln!(
                    "baseline-review: deferred ambiguous callable identity for {} ({:?})",
                    identity.path, identity.scope
                );
            }
            Some(&score) => eprintln!(
                "baseline-review: preserved unchanged or worsened callable {} ({:?}) at score {}",
                identity.path, identity.scope, score
            ),
            None if !scope_counts.contains_key(&identity) => {
                operations.push(patch_remove(format!("/entries/{index}")));
            }
            None => eprintln!(
                "baseline-review: deferred ambiguous callable identity for {} ({:?})",
                identity.path, identity.scope
            ),
        }
    }

    let pairs = match_pairs(&matches, &scope_counts);
    let mut found_by_pair: BTreeMap<Pair, Vec<(&Match<'_>, usize)>> = BTreeMap::new();
    let mut baseline_pair_counts = BTreeMap::<Pair, usize>::new();
    for entry in &baseline.duplication_entries {
        let mut pair = entry.occurrences.clone();
        pair.sort();
        *baseline_pair_counts.entry(pair).or_default() += 1;
    }
    for (found, pair) in matches.iter().zip(pairs) {
        if let Some(pair) = pair {
            found_by_pair
                .entry(pair)
                .or_default()
                .push((found, found.report().score));
        }
    }
    for (index, entry) in baseline.duplication_entries.iter().enumerate() {
        if policy.duplication_minimum.is_none() {
            eprintln!(
                "baseline-review: deferred inactive duplication rule for {} and {}",
                entry.occurrences[0].path, entry.occurrences[1].path
            );
            continue;
        }
        if !entry
            .occurrences
            .iter()
            .all(|identity| Rule::SourceDuplication.supports(identity.language))
        {
            eprintln!(
                "baseline-review: deferred duplication entry for unsupported language at {} and {}",
                entry.occurrences[0].path, entry.occurrences[1].path
            );
            continue;
        }
        if !entry
            .occurrences
            .iter()
            .all(|identity| policy.includes(std::path::Path::new(&identity.path)))
        {
            eprintln!(
                "baseline-review: deferred duplication entry outside policy selection for {} and {}",
                entry.occurrences[0].path, entry.occurrences[1].path
            );
            continue;
        }
        {
            let mut pair = entry.occurrences.clone();
            pair.sort();
            if baseline_pair_counts.get(&pair) != Some(&1) {
                eprintln!(
                    "baseline-review: deferred duplicate baseline entries for {} and {}",
                    pair[0].path, pair[1].path
                );
                continue;
            }
            match found_by_pair.get(&pair).map(Vec::as_slice) {
                Some([(found, score)])
                    if *score < entry.score && found.is_part_of(&entry.tokens) =>
                {
                    let tokens: Vec<_> = found.tokens().map(str::to_owned).collect();
                    operations.push(patch_value(
                        "replace",
                        format!("/duplication_entries/{index}/score"),
                        serde_json::json!(tokens.len()),
                    ));
                    operations.push(patch_value(
                        "replace",
                        format!("/duplication_entries/{index}/tokens"),
                        serde_json::json!(tokens),
                    ));
                }
                Some([(_, score)]) => eprintln!(
                    "baseline-review: preserved unchanged, worsened, or rewritten duplication match for {} and {} (score {})",
                    pair[0].path, pair[1].path, score
                ),
                Some(candidates) if candidates.len() > 1 => {
                    eprintln!(
                        "baseline-review: deferred ambiguous duplication match for {} and {}",
                        pair[0].path, pair[1].path
                    );
                }
                Some(_) => {}
                None if matches.iter().any(|found| {
                    let anchors = found.anchors();
                    (anchors.iter().any(|anchor| anchor.is_none())
                        && found.report().occurrences.iter().any(|occurrence| {
                            entry
                                .occurrences
                                .iter()
                                .any(|identity| identity.path == occurrence.path)
                        }))
                        || anchors
                            .iter()
                            .flatten()
                            .any(|identity| pair.iter().any(|known| known == *identity))
                }) =>
                {
                    eprintln!(
                        "baseline-review: deferred ambiguous or overlapping duplication match for {} and {}",
                        pair[0].path, pair[1].path
                    );
                }
                None if entry.occurrences.iter().all(|identity| {
                    !scope_counts.keys().any(|candidate| {
                        candidate.path == identity.path
                            && candidate.scope.starts_with(&identity.scope)
                    }) || unique(identity, &scope_counts)
                }) =>
                {
                    operations.push(patch_remove(format!("/duplication_entries/{index}")))
                }
                None => eprintln!(
                    "baseline-review: deferred duplication entry with ambiguous identities for {} and {}",
                    pair[0].path, pair[1].path
                ),
            }
        }
    }
    operations.sort_by_key(patch_order);
    Ok(Some(operations))
}

fn patch_order(operation: &PatchOperation) -> (usize, std::cmp::Reverse<usize>, u8, String) {
    let parts: Vec<_> = operation.path.trim_start_matches('/').split('/').collect();
    let array = if parts[0] == "entries" { 0 } else { 1 };
    let index = parts[1].parse::<usize>().unwrap_or(0);
    let field = if operation.op == "remove" {
        0
    } else if parts.len() > 2 && parts[2] == "score" {
        1
    } else {
        2
    };
    (
        array,
        std::cmp::Reverse(index),
        field,
        operation.path.clone(),
    )
}

fn duplication_matches<'a>(
    policy: &Policy,
    sources: &'a [duplication::Source],
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Match<'a>> {
    if let Some(limit) = policy.duplication_minimum {
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
        Command::BaselineReview { settings } => {
            match review_baseline(&settings, &mut report) {
                Ok(Some(patch)) => {
                    write_json(io::stdout().lock(), &patch)?;
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
