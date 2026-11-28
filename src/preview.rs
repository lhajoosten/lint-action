use crate::grammar::Language;
use crate::model::{Diagnostic, ErrorKind, Rule};
use crate::policy::{Baseline, Policy};
use crate::{Detail, Finding, Report, ScoredTree, scan, write_json};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct Side {
    report: Report,
    #[serde(skip)]
    policy: Option<Policy>,
    #[serde(skip)]
    paths: Vec<PathBuf>,
}

fn evaluate(root: &Path, config: &Path, baseline: &Path) -> Side {
    let mut side = Side {
        report: Report::new(),
        policy: None,
        paths: Vec::new(),
    };
    let result = (|| {
        let policy = Policy::load(config)?;
        side.report.set_policy(&policy);
        // Both files are explicit even when the mode does not use waivers: malformed
        // proposed data must never produce an apparently complete preview.
        let baseline = Baseline::load(baseline)?;
        let mut scanned = scan::scan_with_paths(root, &policy);
        side.paths = std::mem::take(&mut scanned.selected_paths);
        let scored = ScoredTree::new(&mut scanned, &policy, &mut side.report);
        scored.add_findings(&policy, &baseline, &mut side.report);
        if side.report.files_scanned == 0 && side.report.diagnostics.is_empty() {
            side.report.diagnostics.push(Diagnostic::new(
                ErrorKind::EmptyScan,
                None,
                "no candidate source files were analyzed",
            ));
        }
        side.policy = Some(policy);
        Ok::<(), Diagnostic>(())
    })();
    if let Err(diagnostic) = result {
        side.report.diagnostics.push(diagnostic);
    }
    side
}

type LocationKey = (String, usize, usize, usize, usize);
type FindingKey = (Rule, Vec<LocationKey>);

fn location(path: &str, range: crate::model::Range) -> LocationKey {
    (
        path.to_owned(),
        range.start.line,
        range.start.column,
        range.end.line,
        range.end.column,
    )
}

fn key(finding: &Finding) -> FindingKey {
    match &finding.detail {
        Detail::Complexity(item) => (item.rule, vec![location(&item.identity.path, item.range)]),
        Detail::FinallyExit(item) => (item.rule, vec![location(&item.identity.path, item.range)]),
        Detail::Duplication(item) => (
            item.rule,
            item.occurrences
                .iter()
                .map(|occ| location(&occ.path, occ.range))
                .collect(),
        ),
    }
}

fn limit(policy: &Policy, finding: &Finding) -> Option<u32> {
    match &finding.detail {
        Detail::Complexity(item) => policy.limit(item.identity.language, item.rule),
        Detail::Duplication(_) => policy.duplication_minimum,
        Detail::FinallyExit(_) => None,
    }
}

fn outcome(policy: &Policy, item: Option<&Finding>, reference: &Finding) -> &'static str {
    if !finding_covered(policy, reference) {
        return "not_covered";
    }
    match item.map(|finding| finding.status) {
        Some(crate::Status::Blocking) => "blocking",
        Some(crate::Status::Baselined) => "baselined",
        Some(crate::Status::Reported) => "reported",
        None => "not_reported",
    }
}

fn covered(policy: &Policy, path: &Path, rule: Rule) -> bool {
    Language::from_path(path).is_some_and(|language| {
        policy.includes(path) && policy.selects(language) && policy.active_for(language, rule)
    })
}

fn finding_covered(policy: &Policy, finding: &Finding) -> bool {
    match &finding.detail {
        Detail::Complexity(item) => covered(policy, Path::new(&item.identity.path), item.rule),
        Detail::FinallyExit(item) => covered(policy, Path::new(&item.identity.path), item.rule),
        Detail::Duplication(item) => item
            .occurrences
            .iter()
            .all(|occ| covered(policy, Path::new(&occ.path), item.rule)),
    }
}

#[derive(Serialize)]
struct Change<'a> {
    disposition: &'static str,
    base_limit: Option<u32>,
    proposed_limit: Option<u32>,
    base_outcome: &'static str,
    proposed_outcome: &'static str,
    base: Option<&'a Finding>,
    proposed: Option<&'a Finding>,
}

#[derive(Serialize)]
struct Coverage {
    path: PathBuf,
    rule: Rule,
    base: bool,
    proposed: bool,
}

#[derive(Serialize)]
struct Preview<'a> {
    complete: bool,
    base_gate: &'static str,
    proposed_gate: &'static str,
    base: &'a Side,
    proposed: &'a Side,
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Vec<Change<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage_changes: Option<Vec<Coverage>>,
}

fn disposition(
    old: &Policy,
    new: &Policy,
    before: Option<&Finding>,
    after: Option<&Finding>,
) -> &'static str {
    match (before, after) {
        (Some(a), Some(b)) => paired_disposition(old, new, a, b),
        (Some(a), None) if !finding_covered(new, a) => "coverage_dropped",
        (None, Some(b)) if !finding_covered(old, b) => "coverage_added",
        (Some(_), None) => "no_longer_reported",
        (None, Some(_)) => "newly_reported",
        (None, None) => unreachable!(),
    }
}

fn paired_disposition(old: &Policy, new: &Policy, a: &Finding, b: &Finding) -> &'static str {
    if old.mode == crate::Mode::Baseline && new.mode == crate::Mode::Baseline {
        if a.status == crate::Status::Blocking && b.status == crate::Status::Baselined {
            return "waiver_gained";
        }
        if a.status == crate::Status::Baselined && b.status == crate::Status::Blocking {
            return "waiver_lost";
        }
    }
    if a.severity == crate::Severity::Error && b.severity == crate::Severity::Warn {
        return "error_to_warn";
    }
    "present_in_both"
}

fn gate(report: &Report) -> &'static str {
    match report.status() {
        0 => "passed",
        1 => "blocking",
        _ => "incomplete",
    }
}

pub fn run(
    root: &Path,
    base_config: &Path,
    base_baseline: &Path,
    proposed_config: &Path,
    proposed_baseline: &Path,
) -> io::Result<u8> {
    let base = evaluate(root, base_config, base_baseline);
    let proposed = evaluate(root, proposed_config, proposed_baseline);
    let complete = base.report.diagnostics.is_empty() && proposed.report.diagnostics.is_empty();
    let mut preview = Preview {
        complete,
        base_gate: gate(&base.report),
        proposed_gate: gate(&proposed.report),
        base: &base,
        proposed: &proposed,
        changes: None,
        coverage_changes: None,
    };
    if complete {
        let old = base.policy.as_ref().expect("complete policy");
        let new = proposed.policy.as_ref().expect("complete policy");
        let base_findings: BTreeMap<_, _> =
            base.report.findings.iter().map(|f| (key(f), f)).collect();
        let proposed_findings: BTreeMap<_, _> = proposed
            .report
            .findings
            .iter()
            .map(|f| (key(f), f))
            .collect();
        let keys: BTreeSet<_> = base_findings
            .keys()
            .chain(proposed_findings.keys())
            .collect();
        preview.changes = Some(
            keys.into_iter()
                .map(|key| {
                    let before = base_findings.get(key).copied();
                    let after = proposed_findings.get(key).copied();
                    let disposition = disposition(old, new, before, after);
                    let reference = before.or(after).expect("finding exists");
                    Change {
                        disposition,
                        base_limit: limit(old, reference),
                        proposed_limit: limit(new, reference),
                        base_outcome: outcome(old, before, reference),
                        proposed_outcome: outcome(new, after, reference),
                        base: before,
                        proposed: after,
                    }
                })
                .collect(),
        );
        let paths: BTreeSet<_> = base.paths.iter().chain(&proposed.paths).collect();
        let mut coverage = Vec::new();
        for path in paths {
            for rule in [
                Rule::CyclomaticComplexity,
                Rule::CognitiveComplexity,
                Rule::SourceDuplication,
                Rule::FinallyExit,
            ] {
                let a = covered(old, path, rule);
                let b = covered(new, path, rule);
                if a != b {
                    coverage.push(Coverage {
                        path: path.clone(),
                        rule,
                        base: a,
                        proposed: b,
                    });
                }
            }
        }
        preview.coverage_changes = Some(coverage);
    }
    write_json(io::stdout().lock(), &preview)?;
    Ok(if complete { 0 } else { 2 })
}
