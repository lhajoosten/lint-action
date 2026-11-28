mod analyzer;
mod cognitive;
mod comparison;
mod duplication;
mod empty_handler;
mod finally;
mod github;
mod grammar;
mod model;
mod policy;
mod scan;
mod text;

use crate::analyzer::Analysis;
use crate::duplication::{Duplication, Match, Pair};
use crate::grammar::ANONYMOUS;
use crate::model::{
    Callable, Diagnostic, EmptyHandler, ErrorKind, FinallyExit, Identity, Position, Rule,
    SCORING_VERSION,
};
use crate::policy::{BASELINE_FILE, Baseline, BaselineScores, CONFIG_FILE, Mode, Policy, Severity};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Github(GithubCommand),

    /// Scan source and write a report to stdout.
    Scan {
        /// Output format (JSON is the default).
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[command(flatten)]
        settings: Settings,
        /// Optional files or directories relative to the scan root.
        paths: Vec<PathBuf>,
    },
    /// Compare full source trees under one trusted policy and return the candidate gate status.
    Compare {
        #[command(flatten)]
        settings: Settings,
        /// Base source root and default location of the trusted policy.
        #[arg(long)]
        base_root: Option<PathBuf>,
        /// Optional base revision label for the materialized tree.
        #[arg(long)]
        base_revision: Option<String>,
        /// Optional candidate revision label for the materialized tree.
        #[arg(long)]
        candidate_revision: Option<String>,
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

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Text,
}

#[derive(Subcommand)]
enum GithubCommand {
    #[command(name = "github-scan", hide = true)]
    Scan,
    #[command(name = "github-app-report", hide = true)]
    AppReport,
    #[command(name = "github-publish", hide = true)]
    Publish,
    #[command(name = "github-prepare-conversation", hide = true)]
    PrepareConversation,
    #[command(name = "github-stage-evidence", hide = true)]
    StageEvidence,
    #[command(name = "github-rescan", hide = true)]
    Rescan { analyzer: PathBuf },
    #[command(name = "github-publish-conversation", hide = true)]
    PublishConversation,
}

#[derive(Args)]
struct ConfigInput {
    /// Source root. In compare, this is the candidate.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Policy file. Defaults to .lint-action.json in the scan root, or the base root for compare.
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

#[derive(Clone, Copy, PartialEq, Serialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    severity: Severity,
    status: Status,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Detail {
    Complexity(Callable),
    Duplication(Duplication),
    FinallyExit(FinallyExit),
    EmptyHandler(EmptyHandler),
}

impl Detail {
    fn location(&self) -> (&str, usize, usize, Rule) {
        let (path, Position { line, column }, rule) = match self {
            Self::Complexity(item) => (&item.identity.path, item.range.start, item.rule),
            Self::FinallyExit(item) => (&item.identity.path, item.range.start, item.rule),
            Self::EmptyHandler(item) => (&item.identity.path, item.range.start, item.rule),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    comparison: Option<comparison::Comparison>,
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
            comparison: None,
        }
    }

    fn set_policy(&mut self, policy: &Policy) {
        self.mode = Some(policy.mode);
        if policy.finally_exit || policy.empty_handler {
            self.format_version = 3;
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
    report.set_policy(&policy);
    let baseline = settings.baseline(policy.mode)?;
    if validate_only {
        return Ok(());
    }
    let mut scanned = scan::scan(&settings.input.root, paths, &policy);
    let scored = ScoredTree::new(&mut scanned, &policy, report);
    scored.add_findings(&policy, &baseline, report);
    Ok(())
}

struct ScoredTree<'a> {
    finally_exits: Vec<FinallyExit>,
    empty_handlers: Vec<EmptyHandler>,
    callables: Vec<Callable>,
    scope_counts: &'a BTreeMap<Identity, usize>,
    matches: Vec<Match<'a>>,
    pairs: Vec<Option<Pair>>,
}

impl<'a> ScoredTree<'a> {
    fn new(scanned: &'a mut scan::Scan, policy: &Policy, report: &mut Report) -> Self {
        report.files_scanned = scanned.files_scanned;
        report.files_skipped = scanned.files_skipped;
        let analysis = &mut scanned.analysis;
        let matches = duplication_matches(policy, &analysis.duplication, &mut analysis.diagnostics);
        let pairs = match_pairs(&matches, &analysis.scope_counts);
        report.diagnostics = std::mem::take(&mut analysis.diagnostics);
        Self {
            finally_exits: std::mem::take(&mut analysis.finally_exits),
            empty_handlers: std::mem::take(&mut analysis.empty_handlers),
            callables: std::mem::take(&mut analysis.callables),
            scope_counts: &analysis.scope_counts,
            matches,
            pairs,
        }
    }

    fn add_findings(self, policy: &Policy, baseline: &BaselineScores, report: &mut Report) {
        for mut callable in self.callables {
            let severity = policy.severity(callable.rule);
            let Some(limit) = policy.limit(callable.identity.language, callable.rule) else {
                continue;
            };
            if severity == Severity::Pass || callable.score <= u64::from(limit) {
                continue;
            }
            let status = complexity_status(&callable, self.scope_counts, policy, baseline);
            callable
                .contributions
                .sort_by_key(|part| (part.range.start.line, part.range.start.column));
            report.findings.push(Finding {
                detail: Detail::Complexity(callable),
                limit: Some(limit),
                severity,
                status,
            });
        }
        if let Some(limit) = policy.duplication_minimum {
            let severity = policy.severity(Rule::SourceDuplication);
            for (found, pair) in self.matches.iter().zip(&self.pairs) {
                report.findings.push(Finding {
                    detail: Detail::Duplication(found.report()),
                    limit: Some(limit),
                    severity,
                    status: duplication_status(found, pair.as_ref(), policy, baseline),
                });
            }
        }
        let severity = policy.severity(Rule::FinallyExit);
        report
            .findings
            .extend(self.finally_exits.into_iter().map(|exit| Finding {
                detail: Detail::FinallyExit(exit),
                limit: None,
                severity,
                status: finally_exit_status(policy),
            }));
        let severity = policy.severity(Rule::EmptyHandler);
        report
            .findings
            .extend(self.empty_handlers.into_iter().map(|item| Finding {
                detail: Detail::EmptyHandler(item),
                limit: None,
                severity,
                status: unscored_status(policy, Rule::EmptyHandler),
            }));
        report
            .findings
            .sort_by(|left, right| left.detail.location().cmp(&right.detail.location()));
    }
}

fn finally_exit_status(policy: &Policy) -> Status {
    unscored_status(policy, Rule::FinallyExit)
}

fn unscored_status(policy: &Policy, rule: Rule) -> Status {
    if policy.severity(rule) == Severity::Warn {
        Status::Reported
    } else {
        Status::for_mode(policy.mode, false)
    }
}

fn complexity_status(
    callable: &Callable,
    scope_counts: &BTreeMap<Identity, usize>,
    policy: &Policy,
    baseline: &BaselineScores,
) -> Status {
    if policy.severity(callable.rule) == Severity::Warn {
        return Status::Reported;
    }
    let covered = policy.mode == Mode::Baseline
        && baseline
            .complexity
            .get(&callable.rule)
            .and_then(|scores| scores.get(&callable.identity))
            .and_then(|&score| score)
            .is_some_and(|score| score >= callable.score)
        && unique(&callable.identity, scope_counts);
    Status::for_mode(policy.mode, covered)
}

fn duplication_status(
    found: &Match<'_>,
    pair: Option<&Pair>,
    policy: &Policy,
    baseline: &BaselineScores,
) -> Status {
    if policy.severity(Rule::SourceDuplication) == Severity::Warn {
        return Status::Reported;
    }
    let covered =
        policy.mode == Mode::Baseline && pair.is_some_and(|pair| baseline.covers(pair, found));
    Status::for_mode(policy.mode, covered)
}

fn compare(
    mut settings: Settings,
    base_root: Option<&Path>,
    comparison: &mut comparison::Comparison,
    report: &mut Report,
) -> Result<(), Diagnostic> {
    if settings.input.config.is_none() {
        settings.input.config = Some(
            base_root
                .ok_or_else(|| {
                    Diagnostic::new(
                        ErrorKind::InvalidConfig,
                        None,
                        "compare requires a trusted --config or --base-root",
                    )
                })?
                .join(CONFIG_FILE),
        );
    }
    let policy = settings.input.policy()?;
    report.set_policy(&policy);
    comparison.set_policy(&policy);
    let baseline = settings.baseline(policy.mode)?;
    let mut scanned = scan::scan(&settings.input.root, &[], &policy);
    let candidate = ScoredTree::new(&mut scanned, &policy, report);
    if !report.diagnostics.is_empty() {
        comparison.unavailable("candidate_analysis_failed");
    } else if let Some(root) = base_root {
        let mut base_report = Report::new();
        let mut scanned = scan::scan(root, &[], &policy);
        let base = ScoredTree::new(&mut scanned, &policy, &mut base_report);
        if base_report.diagnostics.is_empty() {
            comparison.compare(&base, &candidate, &policy, &baseline);
        } else {
            comparison.unavailable("base_analysis_failed");
            comparison.base_diagnostics = base_report.diagnostics;
        }
    } else {
        comparison.unavailable("base_root_unavailable");
    }
    candidate.add_findings(&policy, &baseline, report);
    Ok(())
}

fn generate(
    input: &ConfigInput,
    paths: &[PathBuf],
    report: &mut Report,
) -> Result<Option<Vec<u8>>, Diagnostic> {
    let policy = input.policy()?;
    report.set_policy(&policy);
    let scan = scan::scan(&input.root, paths, &policy);
    report.files_scanned = scan.files_scanned;
    report.files_skipped = scan.files_skipped;
    let Analysis {
        callables,
        mut diagnostics,
        scope_counts,
        duplication,
        ..
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
    report.set_policy(&policy);
    let baseline_path = settings.baseline_path();
    let baseline = Baseline::read(&baseline_path)?;
    let scanned = scan::scan(&settings.input.root, &[], &policy);
    report.files_scanned = scanned.files_scanned;
    report.files_skipped = scanned.files_skipped;
    let Analysis {
        callables,
        mut diagnostics,
        scope_counts,
        duplication,
        ..
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
        if policy.severity(rule) != Severity::Error
            || policy.limit(identity.language, rule).is_none()
        {
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
        if policy.severity(Rule::SourceDuplication) != Severity::Error
            || policy.duplication_minimum.is_none()
        {
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
                    if *score < entry.score
                        && found.is_part_of(entry.tokens.iter().map(String::as_str)) =>
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
    let mut format = OutputFormat::Json;
    match command {
        Command::Github(command) => return Ok(run_github(command)),
        Command::Scan {
            settings,
            paths,
            format: selected,
        } => {
            format = selected;
            if let Err(diagnostic) = scan(&settings, &paths, &mut report, false) {
                report.diagnostics.push(diagnostic);
            }
        }
        Command::Compare {
            settings,
            base_root,
            base_revision,
            candidate_revision,
        } => {
            let mut comparison = comparison::Comparison::new(base_revision, candidate_revision);
            if let Err(diagnostic) =
                compare(settings, base_root.as_deref(), &mut comparison, &mut report)
            {
                comparison.unavailable("candidate_analysis_failed");
                report.diagnostics.push(diagnostic);
            }
            report.comparison = Some(comparison);
        }
        Command::Validate { settings } => {
            if let Err(diagnostic) = scan(&settings, &[], &mut report, true) {
                report.diagnostics.push(diagnostic);
            }
        }
        Command::Baseline { input, paths } => return baseline_output(input, paths),
        Command::BaselineReview { settings } => return review_output(settings),
    }
    match format {
        OutputFormat::Json => write_json(io::stdout().lock(), &report)?,
        OutputFormat::Text => text::write(io::stdout().lock(), &report)?,
    }
    Ok(report.status())
}

fn run_github(command: GithubCommand) -> u8 {
    match command {
        GithubCommand::Scan => github::scan_action(),
        GithubCommand::AppReport => match github::app_report::normalize_action() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("lint-gate: {error}");
                1
            }
        },
        GithubCommand::Publish => github::publisher::publish_action(),
        GithubCommand::PrepareConversation => github::publisher::conversation::prepare_action(),
        GithubCommand::PublishConversation => github::publisher::conversation::publish_action(),
        GithubCommand::StageEvidence => github::evidence::action(github::evidence::stage),
        GithubCommand::Rescan { analyzer } => {
            github::evidence::action(|| github::evidence::rescan(&analyzer))
        }
    }
}

fn baseline_output(input: ConfigInput, paths: Vec<PathBuf>) -> io::Result<u8> {
    let mut report = Report::new();
    match generate(&input, &paths, &mut report) {
        Ok(Some(baseline)) => {
            io::stdout().lock().write_all(&baseline)?;
            return Ok(0);
        }
        Ok(None) => {}
        Err(diagnostic) => report.diagnostics.push(diagnostic),
    }
    write_json(io::stderr().lock(), &report)?;
    Ok(2)
}

fn review_output(settings: Settings) -> io::Result<u8> {
    let mut report = Report::new();
    match review_baseline(&settings, &mut report) {
        Ok(Some(patch)) => {
            write_json(io::stdout().lock(), &patch)?;
            return Ok(0);
        }
        Ok(None) => {}
        Err(diagnostic) => report.diagnostics.push(diagnostic),
    }
    write_json(io::stderr().lock(), &report)?;
    Ok(2)
}

fn main() -> ExitCode {
    let command = Cli::parse().command;
    let text_output = matches!(
        &command,
        Command::Scan {
            format: OutputFormat::Text,
            ..
        }
    );
    match run(command) {
        Ok(status) => status.into(),
        Err(error) => {
            let format = if text_output { "text" } else { "JSON" };
            eprintln!("lint-gate: could not write {format}: {error}");
            2.into()
        }
    }
}
