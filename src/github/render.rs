use super::{CHECK, Result, append, optional};
use crate::comparison::{Category, Change, Comparison, Location, MatchStatus};
use crate::model::{Diagnostic, Position, Range};
use crate::{Detail, Finding, Report, Severity, Status};
use serde::Serialize;
use std::fmt::Write;
use std::path::{Component, Path};

const LIMIT: usize = 10;
pub(super) const STATUSES: [Status; 3] = [Status::Blocking, Status::Reported, Status::Baselined];

pub(super) fn label(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

pub(super) fn truncate(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

pub(super) fn summary_text(value: &str) -> String {
    format!(
        "<code>{}</code>",
        html_escape(&serde_json::to_string(value).unwrap())
    )
}

fn workflow_escape(value: &str, property: bool) -> String {
    let mut result = String::new();
    for ch in value.chars() {
        match ch {
            '%' => result.push_str("%25"),
            '\r' => result.push_str("%0D"),
            '\n' => result.push_str("%0A"),
            ':' if property => result.push_str("%3A"),
            ',' if property => result.push_str("%2C"),
            '\0'..='\x1f' | '\x7f' => write!(result, "\\x{:02x}", ch as u32).unwrap(),
            _ => result.push(ch),
        }
    }
    result
}

pub(super) fn annotation(
    level: &str,
    message: &str,
    path: Option<&str>,
    position: Option<Position>,
) {
    let mut properties = format!("title={CHECK}");
    if let Some(path) = path {
        write!(properties, ",file={}", workflow_escape(path, true)).unwrap();
        if let Some(position) = position {
            write!(
                properties,
                ",line={},col={}",
                position.line, position.column
            )
            .unwrap();
        }
    }
    println!(
        "::{level} {properties}::{}",
        workflow_escape(&truncate(message, 1000), false)
    );
}

struct Message<'a> {
    level: &'static str,
    text: String,
    path: Option<&'a str>,
    position: Option<Position>,
}

impl<'a> From<&'a Diagnostic> for Message<'a> {
    fn from(item: &'a Diagnostic) -> Self {
        let path = item.path.as_deref().filter(|path| {
            let path = Path::new(path);
            !path.is_absolute() && !path.components().any(|part| part == Component::ParentDir)
        });
        Self {
            level: "error",
            text: format!("{}: {}", label(item.kind), item.message),
            path,
            position: item.range.map(|range| range.start),
        }
    }
}

fn occurrence(path: &str, range: Range) -> String {
    let Range { start, end } = range;
    format!(
        "{path}:{}:{}-{}:{}",
        start.line, start.column, end.line, end.column
    )
}

impl<'a> From<&'a Finding> for Message<'a> {
    fn from(item: &'a Finding) -> Self {
        let status = label(item.status);
        let severity = label(item.severity);
        let limit = item.limit;
        let (text, path, position) = match &item.detail {
            Detail::Duplication(duplicate) => {
                let [primary, secondary] = &duplicate.occurrences;
                let text = format!(
                    "{severity} source-duplication: {} shared tokens (minimum {limit}, {status}) at {} and {}",
                    duplicate.score,
                    occurrence(&primary.path, primary.range),
                    occurrence(&secondary.path, secondary.range)
                );
                (text, primary.path.as_str(), primary.range.start)
            }
            Detail::Complexity(callable) => {
                let rule = label(callable.rule).replace('-', " ");
                let mut text = format!(
                    "{} has {severity} {rule} {} (limit {limit}, {status})",
                    callable.name, callable.score
                );
                let mut contributions: Vec<_> = callable.contributions.iter().collect();
                contributions.sort_by_key(|part| std::cmp::Reverse(part.points));
                if !contributions.is_empty() {
                    let details = contributions
                        .iter()
                        .take(3)
                        .map(|part| {
                            format!(
                                "+{} {} at line {}",
                                part.points, part.construct, part.range.start.line
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(text, ". Largest contributions: {details}").unwrap();
                }
                (text, callable.identity.path.as_str(), callable.range.start)
            }
        };
        Self {
            level: if item.status == Status::Blocking {
                "error"
            } else {
                "warning"
            },
            text,
            path: Some(path),
            position: Some(position),
        }
    }
}

fn source_link(
    path: &str,
    range: Range,
    display: &str,
    revision: Option<&str>,
    url: Option<&str>,
) -> String {
    let display = summary_text(display);
    let (Some(revision), Some(url)) = (revision, url) else {
        return display;
    };
    let start = range.start.line;
    let end = range.end.line;
    let fragment = if start == end {
        format!("L{start}")
    } else {
        format!("L{start}-L{end}")
    };
    let mut encoded = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    let href = html_escape(&format!("{url}/blob/{revision}/{encoded}#{fragment}"));
    format!("<a href=\"{href}\">{display}</a>")
}

fn change_summary(change: &Change, comparison: &Comparison, url: Option<&str>) -> String {
    let active = change
        .current
        .as_ref()
        .or(change.previous.as_ref())
        .unwrap();
    let score = match (&change.previous, &change.current) {
        (Some(previous), Some(current)) => format!("{} → {}", previous.score, current.score),
        _ => active.score.to_string(),
    };
    let rule = label(change.rule);
    let limit = if rule == "source-duplication" {
        "minimum"
    } else {
        "limit"
    };
    let status = active.status.map_or_else(|| "passing".to_owned(), label);
    let match_status = match change.match_status {
        MatchStatus::Matched => "",
        MatchStatus::Ambiguous => ", ambiguous identity",
        MatchStatus::Unmatched => ", no matched finding",
    };
    let mut line = format!(
        "- {rule} {score} ({limit} {}, {status}{match_status})",
        change.limit
    );
    for (side, item, revision) in [
        (
            "base",
            &change.previous,
            comparison.base_revision.as_deref(),
        ),
        (
            "candidate",
            &change.current,
            comparison.candidate_revision.as_deref(),
        ),
    ] {
        let Some(item) = item else { continue };
        let location = match &item.location {
            Location::Callable {
                identity,
                name,
                range,
            } => {
                let path = &identity.path;
                source_link(
                    path,
                    *range,
                    &format!("{name} at {}", occurrence(path, *range)),
                    revision,
                    url,
                )
            }
            Location::Duplication { occurrences } => occurrences
                .iter()
                .map(|part| {
                    source_link(
                        &part.path,
                        part.range,
                        &occurrence(&part.path, part.range),
                        revision,
                        url,
                    )
                })
                .collect::<Vec<_>>()
                .join(" and "),
        };
        write!(line, " · {side}: {location}").unwrap();
    }
    line
}

fn comparison_summary(
    comparison: &Comparison,
    relative: &Path,
    base_error: Option<&str>,
) -> String {
    let mut summary = "\n### Pull request finding comparison\n\n".to_owned();
    for (side, revision) in [
        ("Base", &comparison.base_revision),
        ("Candidate", &comparison.candidate_revision),
    ] {
        if let Some(revision) = revision {
            writeln!(summary, "{side} revision: {}", summary_text(revision)).unwrap();
        }
    }
    let Some(unchanged) = comparison.unchanged else {
        describe_unavailable(&mut summary, comparison, base_error);
        return summary;
    };

    writeln!(summary, "Unchanged findings: **{unchanged}**").unwrap();
    let url = optional("GITHUB_SERVER_URL")
        .zip(optional("GITHUB_REPOSITORY"))
        .filter(|(_, repository)| {
            relative.as_os_str().is_empty()
                && regex::Regex::new(r"\A[\w.-]+/[\w.-]+\z")
                    .unwrap()
                    .is_match(repository)
        })
        .map(|(server, repository)| format!("{}/{repository}", server.trim_end_matches('/')));
    for (category, title, name) in [
        (Category::New, "New", "new"),
        (Category::Worsened, "Worsened", "worsened"),
        (Category::Improved, "Improved", "improved"),
        (Category::Fixed, "Fixed", "fixed"),
        (Category::Removed, "Removed", "removed"),
    ] {
        let items: Vec<_> = comparison
            .changes
            .iter()
            .filter(|item| item.category == category)
            .collect();
        if items.is_empty() {
            continue;
        }
        writeln!(summary, "\n#### {title} ({})\n", items.len()).unwrap();
        for item in items.iter().take(LIMIT) {
            writeln!(
                summary,
                "{}",
                change_summary(item, comparison, url.as_deref())
            )
            .unwrap();
        }
        if items.len() > LIMIT {
            writeln!(
                summary,
                "\nShowing {LIMIT} of {} {name} findings.",
                items.len()
            )
            .unwrap();
        }
    }
    summary
}

fn describe_unavailable(summary: &mut String, comparison: &Comparison, base_error: Option<&str>) {
    let reason = match comparison.reason {
        Some("candidate_analysis_failed") => "Candidate analysis failed.",
        Some("base_analysis_failed") => "Base analysis failed.",
        _ => "The exact base source tree is unavailable.",
    };
    writeln!(summary, "**Comparison unavailable.** {reason}").unwrap();
    if let Some(error) = base_error {
        writeln!(summary, "- {}", summary_text(&truncate(error, 500))).unwrap();
    }
    for item in comparison.base_diagnostics.iter().take(LIMIT) {
        let text = format!("{}: {}", label(item.kind), item.message);
        let text = item
            .path
            .as_ref()
            .map_or(text.clone(), |path| format!("{path} — {text}"));
        writeln!(summary, "- {}", summary_text(&truncate(&text, 500))).unwrap();
    }
}

pub fn report(report: &Report, code: u8, relative: &Path, base_error: Option<&str>) -> Result<()> {
    let mut messages: Vec<_> = report.diagnostics.iter().map(Message::from).collect();
    let [blocking, reported, baselined] = STATUSES.map(|status| {
        let start = messages.len();
        messages.extend(
            report
                .findings
                .iter()
                .filter(|item| item.status == status)
                .map(Message::from),
        );
        messages.len() - start
    });
    let warning = report
        .findings
        .iter()
        .filter(|item| item.severity == Severity::Warn)
        .count();
    let error = report.findings.len() - warning;
    for message in messages.iter().take(LIMIT) {
        let path = message
            .path
            .map(|path| relative.join(path).to_string_lossy().into_owned());
        annotation(
            message.level,
            &message.text,
            path.as_deref(),
            message.position,
        );
    }
    let outcome = ["passed", "violations", "analysis failed"][usize::from(code)];
    let mode = report.mode.map_or_else(|| "unavailable".to_owned(), label);
    let mut summary = format!(
        "## {CHECK}\n\nOutcome: **{outcome}** · Mode: **{mode}**\nFiles scanned: {} · Files skipped: {}\nAnalysis errors: {} · Reported: {reported} · Baselined: {baselined} · Blocking: {blocking}\nWarning severity: {warning} · Error severity: {error}\n",
        report.files_scanned,
        report.files_skipped,
        report.diagnostics.len()
    );
    if let Some(comparison) = &report.comparison {
        summary.push_str(&comparison_summary(comparison, relative, base_error));
        if comparison.status == "complete" {
            messages.clear();
        }
    }
    if !messages.is_empty() {
        summary.push('\n');
    }
    for message in messages.iter().take(LIMIT) {
        let display = match (message.path, message.position) {
            (Some(path), Some(position)) => format!("{path}:{} — {}", position.line, message.text),
            (Some(path), None) => format!("{path} — {}", message.text),
            _ => message.text.clone(),
        };
        writeln!(summary, "- {}", summary_text(&truncate(&display, 500))).unwrap();
    }
    if messages.len() > LIMIT {
        writeln!(
            summary,
            "\nShowing {LIMIT} of {} diagnostics and findings.",
            messages.len()
        )
        .unwrap();
    }
    append("GITHUB_STEP_SUMMARY", &summary)
}
