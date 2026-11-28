use crate::model::{Callable, Diagnostic, Rule};
use crate::policy::Severity;
use crate::{Detail, Finding, Report, Status};
use std::cmp::Reverse;
use std::io::{self, Write};

fn safe(value: &str) -> String {
    let mut escaped = String::new();
    for c in value.chars() {
        if c.is_control() {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    escaped
}

fn rule(rule: Rule) -> &'static str {
    match rule {
        Rule::CyclomaticComplexity => "cyclomatic-complexity",
        Rule::CognitiveComplexity => "cognitive-complexity",
        Rule::CallableLines => "callable-lines",
        Rule::SourceDuplication => "source-duplication",
        Rule::FinallyExit => "finally-exit",
    }
}

pub fn write(mut out: impl Write, report: &Report) -> io::Result<()> {
    let mut findings: Vec<_> = report.findings.iter().collect();
    findings.sort_by_key(|finding| match finding.status {
        Status::Blocking => 0,
        Status::Reported => 1,
        Status::Baselined => 2,
    });
    for finding in findings {
        write_finding(&mut out, finding)?;
    }
    for diagnostic in &report.diagnostics {
        write_diagnostic(&mut out, diagnostic)?;
    }
    let outcome = match report.status() {
        0 => "passed",
        1 => "blocked",
        _ => "incomplete",
    };
    writeln!(
        out,
        "{outcome}: {} file(s) scanned, {} skipped",
        report.files_scanned, report.files_skipped
    )
}

fn write_finding(mut out: impl Write, finding: &Finding) -> io::Result<()> {
    let status = status(finding);
    let (path, line, column, finding_rule) = finding.detail.location();
    write!(
        out,
        "{}:{line}:{column}: {status} {}",
        safe(path),
        rule(finding_rule)
    )?;
    match &finding.detail {
        Detail::Complexity(item) => write_complexity(&mut out, item, finding.limit.unwrap())?,
        Detail::Duplication(item) => {
            writeln!(
                out,
                ": score {}, minimum {}",
                item.score,
                finding.limit.unwrap()
            )?;
            let other = &item.occurrences[1];
            writeln!(
                out,
                "  duplicate: {}:{}:{}",
                safe(&other.path),
                other.range.start.line,
                other.range.start.column
            )?;
        }
        Detail::FinallyExit(item) => writeln!(out, ": {}", safe(item.message))?,
    }
    Ok(())
}

fn write_diagnostic(mut out: impl Write, diagnostic: &Diagnostic) -> io::Result<()> {
    write!(out, "diagnostic")?;
    if let Some(path) = &diagnostic.path {
        write!(out, " {}", safe(path))?;
        if let Some(range) = diagnostic.range {
            write!(out, ":{}:{}", range.start.line, range.start.column)?;
        }
    }
    let kind = serde_json::to_value(diagnostic.kind).expect("diagnostic kind serializes");
    writeln!(
        out,
        ": {}: {}",
        kind.as_str().unwrap(),
        safe(&diagnostic.message)
    )?;
    Ok(())
}

fn write_complexity(mut out: impl Write, item: &Callable, limit: u32) -> io::Result<()> {
    writeln!(
        out,
        ": score {}, limit {} ({})",
        item.score,
        limit,
        safe(&item.name)
    )?;
    if item.rule == Rule::CognitiveComplexity {
        let mut parts: Vec<_> = item.contributions.iter().collect();
        parts.sort_by_key(|part| {
            (
                Reverse(part.points),
                part.range.start.line,
                part.range.start.column,
            )
        });
        for part in parts.into_iter().take(3) {
            writeln!(
                out,
                "  line {}: {} +{}",
                part.range.start.line,
                safe(part.construct),
                part.points
            )?;
        }
    }
    Ok(())
}

fn status(finding: &Finding) -> &'static str {
    match (finding.status, finding.severity) {
        (Status::Blocking, _) => "blocking",
        (Status::Baselined, _) => "baselined",
        (Status::Reported, Severity::Warn) => "advisory",
        (Status::Reported, _) => "reported",
    }
}
