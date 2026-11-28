use crate::analyzer::{Analysis, AnalysisOptions, analyze};
use crate::duplication;
use crate::grammar::Language;
use crate::model::{Diagnostic, ErrorKind, Rule};
use crate::policy::{MAX_FILE_BYTES, Policy, bounded_read, normal_relative_path, slash_path};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

#[derive(Default)]
pub struct Scan {
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub analysis: Analysis,
}

pub fn scan(root: &Path, paths: &[PathBuf], policy: &Policy) -> Scan {
    let mut scan = Scan::default();
    let root = match root.canonicalize() {
        Ok(root) if root.is_dir() => root,
        _ => {
            scan.analysis.diagnostics.push(path_error(
                ErrorKind::InvalidPath,
                root,
                "scan root must be an accessible directory",
            ));
            return scan;
        }
    };

    let mut files = BTreeSet::new();
    let mut diagnostics = Vec::new();
    let active = policy.any_active();
    if paths.is_empty() {
        if active {
            discover(&root, &root, policy, &mut files, &mut diagnostics);
        }
    } else {
        for path in paths.iter().collect::<BTreeSet<_>>() {
            match explicit_path(&root, path) {
                Ok(full_path) if active => {
                    discover(&root, &full_path, policy, &mut files, &mut diagnostics);
                }
                Ok(_) => {}
                Err(diagnostic) => diagnostics.push(diagnostic),
            }
        }
    }
    diagnostics
        .sort_by(|left, right| (&left.path, &left.message).cmp(&(&right.path, &right.message)));
    scan.analysis.diagnostics.extend(diagnostics);

    // A deliberately all-pass policy validates the request and selection, then
    // returns a successful skipped-analysis result without touching source files.
    if !active {
        return scan;
    }

    let mut duplication_budget = policy
        .duplication_minimum
        .filter(|_| policy.active(Rule::SourceDuplication))
        .map(|_| (duplication::MAX_TOKENS, duplication::MAX_TOKEN_BYTES));
    for path in files {
        match scan_file(&root, &path, policy, duplication_budget) {
            Ok(Some(result)) => {
                scan.files_scanned += 1;
                scan.analysis.callables.extend(result.callables);
                scan.analysis.finally_exits.extend(result.finally_exits);
                scan.analysis.empty_handlers.extend(result.empty_handlers);
                scan.analysis.scope_counts.extend(result.scope_counts);
                if result
                    .diagnostics
                    .iter()
                    .any(|error| error.kind == ErrorKind::DuplicationLimit)
                {
                    duplication_budget = None;
                    scan.analysis.duplication.clear();
                }
                for source in result.duplication {
                    if let Some((tokens, bytes)) = &mut duplication_budget {
                        *tokens -= source.tokens;
                        *bytes -= source.bytes;
                        scan.analysis.duplication.push(source);
                    }
                }
                scan.analysis.diagnostics.extend(result.diagnostics);
            }
            Ok(None) => scan.files_skipped += 1,
            Err(diagnostic) => scan.analysis.diagnostics.push(diagnostic),
        }
    }
    if scan.files_scanned == 0 && scan.analysis.diagnostics.is_empty() {
        scan.analysis.diagnostics.push(Diagnostic::new(
            ErrorKind::EmptyScan,
            None,
            "no regular files matched the enabled languages and path selection",
        ));
    }
    scan
}

fn explicit_path(root: &Path, path: &Path) -> Result<PathBuf, Diagnostic> {
    if path == Path::new(".") {
        return Ok(root.to_owned());
    }
    let invalid = |message| path_error(ErrorKind::InvalidPath, path, message);
    if !normal_relative_path(path) || path.components().any(|part| part.as_os_str() == ".git") {
        return Err(invalid("selection must stay inside the scan root"));
    }
    let mut full = root.to_owned();
    for part in path.components() {
        full.push(part);
        let metadata = fs::symlink_metadata(&full)
            .map_err(|error| path_error(ErrorKind::UnreadableSource, path, error.to_string()))?;
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(invalid(
                "selection must be a regular file or directory without symlinks",
            ));
        }
    }
    Ok(full)
}

fn discover(
    root: &Path,
    path: &Path,
    policy: &Policy,
    files: &mut BTreeSet<PathBuf>,
    errors: &mut Vec<Diagnostic>,
) {
    for entry in WalkDir::new(path)
        .follow_root_links(false)
        .into_iter()
        .filter_entry(|entry| visit_directory(root, entry, policy))
    {
        match entry {
            Ok(entry) if !entry.file_type().is_dir() => {
                files.insert(entry.into_path());
            }
            Ok(_) => {}
            Err(error) => {
                let error_path = error.path().and_then(|path| path.strip_prefix(root).ok());
                errors.push(Diagnostic::new(
                    ErrorKind::UnreadableSource,
                    error_path.and_then(slash_path),
                    error
                        .io_error()
                        .map_or_else(|| error.to_string(), ToString::to_string),
                ));
            }
        }
    }
}

fn visit_directory(root: &Path, entry: &DirEntry, policy: &Policy) -> bool {
    let relative = entry
        .path()
        .strip_prefix(root)
        .expect("walk starts within root");
    if entry.file_name() == ".git" && !relative.as_os_str().is_empty() {
        return false;
    }
    // An explicit selection must obey the same exclusions as a full scan.
    !entry.file_type().is_dir()
        || !relative
            .ancestors()
            .any(|path| !path.as_os_str().is_empty() && policy.excludes_directory(path))
}

fn scan_file(
    root: &Path,
    full_path: &Path,
    policy: &Policy,
    duplication_budget: Option<(usize, usize)>,
) -> Result<Option<Analysis>, Diagnostic> {
    let path = full_path
        .strip_prefix(root)
        .expect("file was discovered within root");
    let Some(language) = Language::from_path(path) else {
        return Ok(None);
    };
    let active_for_language = [
        Rule::CyclomaticComplexity,
        Rule::CognitiveComplexity,
        Rule::SourceDuplication,
        Rule::FinallyExit,
        Rule::EmptyHandler,
    ]
    .into_iter()
    .any(|rule| policy.active_for(language, rule));
    if !active_for_language || !policy.selects(language) || !policy.includes(path) {
        return Ok(None);
    }
    let error = |kind, message| path_error(kind, path, message);
    let path = slash_path(path)
        .ok_or_else(|| path_error(ErrorKind::InvalidEncoding, path, "path is not UTF-8"))?;
    let metadata = full_path
        .symlink_metadata()
        .map_err(|io| error(ErrorKind::UnreadableSource, io.to_string()))?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let canonical = full_path
        .canonicalize()
        .map_err(|io| error(ErrorKind::UnreadableSource, io.to_string()))?;
    if canonical != full_path {
        return Err(error(
            ErrorKind::InvalidPath,
            "source path changed or traverses a symlink".into(),
        ));
    }
    let bytes = bounded_read(full_path, MAX_FILE_BYTES)
        .map_err(|io| error(ErrorKind::UnreadableSource, io.to_string()))?
        .ok_or_else(|| {
            error(
                ErrorKind::SourceTooLarge,
                format!("source exceeds {MAX_FILE_BYTES} bytes"),
            )
        })?;
    let source = std::str::from_utf8(&bytes)
        .map_err(|io| error(ErrorKind::InvalidEncoding, io.to_string()))?;
    let budget =
        duplication_budget.filter(|_| policy.active_for(language, Rule::SourceDuplication));
    analyze(
        &path,
        language,
        source,
        AnalysisOptions {
            duplication_budget: budget,
            cyclomatic_enabled: policy.active_for(language, Rule::CyclomaticComplexity),
            cognitive_enabled: policy.active_for(language, Rule::CognitiveComplexity),
            finally_enabled: policy.active_for(language, Rule::FinallyExit),
            empty_handler_enabled: policy.active_for(language, Rule::EmptyHandler),
        },
    )
    .map(Some)
}

fn path_error(kind: ErrorKind, path: &Path, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(kind, Some(path.to_string_lossy().into_owned()), message)
}
