use crate::analyzer::{Analysis, analyze};
use crate::grammar::Language;
use crate::model::{Diagnostic, ErrorKind};
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
    if paths.is_empty() {
        discover(&root, &root, policy, &mut files, &mut diagnostics);
    } else {
        for path in paths.iter().collect::<BTreeSet<_>>() {
            match explicit_path(&root, path) {
                Ok(full_path) => discover(&root, &full_path, policy, &mut files, &mut diagnostics),
                Err(diagnostic) => diagnostics.push(diagnostic),
            }
        }
    }
    diagnostics
        .sort_by(|left, right| (&left.path, &left.message).cmp(&(&right.path, &right.message)));
    scan.analysis.diagnostics.extend(diagnostics);

    for path in files {
        match scan_file(&root, &path, policy) {
            Ok(Some(result)) => {
                scan.files_scanned += 1;
                scan.analysis.callables.extend(result.callables);
                scan.analysis.scope_counts.extend(result.scope_counts);
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
) -> Result<Option<Analysis>, Diagnostic> {
    let path = full_path
        .strip_prefix(root)
        .expect("file was discovered within root");
    let Some(language) = Language::from_path(path) else {
        return Ok(None);
    };
    if policy.limit(language).is_none()
        || !policy.includes(path)
        || path
            .ancestors()
            .skip(1)
            .any(|parent| !parent.as_os_str().is_empty() && policy.excludes_directory(parent))
    {
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
    analyze(&path, language, source).map(Some)
}

fn path_error(kind: ErrorKind, path: &Path, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(kind, Some(path.to_string_lossy().into_owned()), message)
}
