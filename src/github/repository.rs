use super::{Result, is_sha, optional};
use crate::policy::{BASELINE_FILE, CONFIG_FILE};
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub fn is_object_id(sha: &str) -> bool {
    is_sha(sha, 40) || is_sha(sha, 64)
}

fn git(repository: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args(["--no-optional-locks", "--no-replace-objects", "-C"])
        .arg(repository);
    command
}

pub(super) fn output(repository: &Path, args: &[&str]) -> io::Result<Output> {
    git(repository).args(args).output()
}

fn has_commit(repository: &Path, revision: &str) -> io::Result<bool> {
    Ok(output(
        repository,
        &["cat-file", "-e", &format!("{revision}^{{commit}}")],
    )?
    .status
    .success())
}

pub fn path_within(workspace: &Path) -> Result<PathBuf> {
    let selection =
        env::var_os("LINT_GATE_REPOSITORY").map_or_else(|| PathBuf::from("."), PathBuf::from);
    let repository = fs::canonicalize(workspace.join(&selection));
    if let Ok(repository) = repository
        && !selection.is_absolute()
        && repository.starts_with(workspace)
        && repository.is_dir()
    {
        return Ok(repository);
    }
    Err("repository must be a directory within GITHUB_WORKSPACE".into())
}

pub fn trusted_policy(
    workspace: &Path,
    repository: &Path,
    temporary: &Path,
    event: &str,
    base: Option<&str>,
    pr: bool,
) -> Result<PathBuf> {
    if event == "pull_request_target" {
        return Err("use the pull_request event for candidate source analysis".into());
    }
    let revision = optional("LINT_GATE_POLICY_REF");
    if let Some(directory) = optional("LINT_GATE_POLICY_DIRECTORY") {
        if revision.is_some() {
            return Err("policy-directory and policy-ref are mutually exclusive".into());
        }
        let directory = Path::new(&directory);
        if !directory.is_absolute() || !directory.is_dir() {
            return Err("policy-directory must be an existing absolute directory".into());
        }
        let directory = fs::canonicalize(directory)?;
        if directory.starts_with(workspace) {
            return Err(
                "policy-directory and its policy files must be outside GITHUB_WORKSPACE".into(),
            );
        }
        for name in [CONFIG_FILE, BASELINE_FILE] {
            let resolved = resolve_policy_path(&directory.join(name), 64)?;
            if resolved.starts_with(workspace) {
                return Err(
                    "policy-directory and its policy files must be outside GITHUB_WORKSPACE".into(),
                );
            }
        }
        return Ok(directory.join(CONFIG_FILE));
    }
    match revision.as_deref().or(base) {
        Some(revision) => {
            copy_policy(repository, revision, temporary)?;
            Ok(temporary.join(CONFIG_FILE))
        }
        None if pr => Err("pull request base SHA is unavailable".into()),
        None => Ok(repository.join(CONFIG_FILE)),
    }
}

// Optional policy files may be absent. Resolve any existing symlink so a
// missing target inside the candidate still cannot be used as trusted policy.
fn resolve_policy_path(path: &Path, remaining: usize) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(error) if error.kind() == io::ErrorKind::NotFound && remaining > 0 => {
            let (Some(parent), Some(component)) = (path.parent(), path.components().next_back())
            else {
                return Err(error);
            };
            let mut parent = resolve_policy_path(parent, remaining - 1)?;
            if component == Component::ParentDir {
                parent.pop();
                return Ok(parent);
            }
            let candidate = parent.join(component.as_os_str());
            match fs::read_link(&candidate) {
                Ok(target) => resolve_policy_path(&parent.join(target), remaining - 1),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                    ) =>
                {
                    Ok(candidate)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn copy_policy(repository: &Path, revision: &str, directory: &Path) -> Result<()> {
    if !is_object_id(revision) {
        return Err("the trusted policy revision must be a full Git object ID".into());
    }
    if !has_commit(repository, revision)? {
        return Err(format!(
            "cannot read trusted policy revision {revision}; check out with fetch-depth: 0"
        )
        .into());
    }
    for (name, limit) in [
        (CONFIG_FILE, 4 * 1024 * 1024),
        (BASELINE_FILE, 16 * 1024 * 1024),
    ] {
        let entry = output(repository, &["ls-tree", revision, "--", name])?;
        let read_error = || format!("cannot read {name} at trusted revision {revision}");
        if !entry.status.success() {
            return Err(read_error().into());
        }
        if entry.stdout.is_empty() {
            if name == BASELINE_FILE {
                continue;
            }
            return Err(format!("{name} is missing from trusted revision {revision}").into());
        }
        if !(entry.stdout.starts_with(b"100644 blob ") || entry.stdout.starts_with(b"100755 blob "))
        {
            return Err(
                format!("{name} at trusted revision {revision} must be a regular file").into(),
            );
        }
        let spec = format!("{revision}:{name}");
        let size = output(repository, &["cat-file", "-s", &spec])?;
        if !size.status.success() || size_number(&size.stdout).is_none_or(|size| size > limit) {
            return Err(format!("{} within {limit} bytes", read_error()).into());
        }
        let blob = output(repository, &["cat-file", "blob", &spec])?;
        if !blob.status.success() {
            return Err(read_error().into());
        }
        fs::write(directory.join(name), blob.stdout)?;
    }
    Ok(())
}

fn size_number(bytes: &[u8]) -> Option<u64> {
    str::from_utf8(bytes).ok()?.trim().parse().ok()
}

pub fn candidate_revision(repository: &Path) -> Option<String> {
    let head = output(repository, &["rev-parse", "--verify", "HEAD"]).ok()?;
    if !head.status.success() {
        return None;
    }
    let status = output(
        repository,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )
    .ok()?;
    if !status.status.success() || !status.stdout.is_empty() {
        return None;
    }
    Some(str::from_utf8(&head.stdout).ok()?.trim().to_owned())
}

pub fn materialize_base(repository: &Path, revision: Option<&str>, directory: &Path) -> Result<()> {
    let revision = revision
        .filter(|sha| is_object_id(sha))
        .ok_or("an exact pull request base SHA is unavailable")?;
    if !has_commit(repository, revision)? {
        return Err(
            format!("cannot read PR base {revision}; check out with fetch-depth: 0").into(),
        );
    }
    let entries = output(repository, &["ls-tree", "-rz", revision])?;
    if !entries.status.success() {
        return Err(format!("cannot list PR base {revision}").into());
    }

    // Copy committed bytes directly. Checkout filters and archive attributes can change the tree.
    fs::create_dir(directory)?;
    let mut child = git(repository)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut requests = child.stdin.take().unwrap();
    let mut blobs = BufReader::new(child.stdout.take().unwrap());
    let result = copy_blobs(
        &entries.stdout,
        directory,
        revision,
        &mut requests,
        &mut blobs,
    );
    drop(requests);
    drop(blobs);
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    result?;
    if !status.success() {
        return Err(format!("cannot finish reading PR base {revision}").into());
    }
    Ok(())
}

fn copy_blobs(
    entries: &[u8],
    directory: &Path,
    revision: &str,
    mut requests: impl Write,
    mut blobs: impl BufRead,
) -> Result<()> {
    let mut header = Vec::new();
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("invalid Git tree entry")?;
        let (metadata, name) = (&entry[..tab], &entry[tab + 1..]);
        let fields: Vec<_> = metadata.split(|byte| *byte == b' ').collect();
        let [mode, _, object] = fields.as_slice() else {
            return Err("invalid Git tree metadata".into());
        };
        if !matches!(*mode, b"100644" | b"100755") {
            continue;
        }
        if name
            .split(|byte| *byte == b'/')
            .any(|part| matches!(part, b"" | b"." | b".." | b".git"))
        {
            return Err("the PR base contains an invalid source path".into());
        }
        #[cfg(unix)]
        let relative = {
            use std::os::unix::ffi::OsStrExt;
            Path::new(std::ffi::OsStr::from_bytes(name))
        };
        #[cfg(not(unix))]
        let relative = Path::new(str::from_utf8(name)?);
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("the PR base contains an invalid source path".into());
        }
        let target = directory.join(relative);
        fs::create_dir_all(target.parent().unwrap())?;
        requests.write_all(object)?;
        requests.write_all(b"\n")?;
        requests.flush()?;
        header.clear();
        blobs.read_until(b'\n', &mut header)?;
        let fields: Vec<_> = header.split(|byte| *byte == b' ').collect();
        let [id, b"blob", size] = fields.as_slice() else {
            return Err(format!("cannot read a source blob from PR base {revision}").into());
        };
        let size = size_number(size)
            .filter(|_| id == object)
            .ok_or_else(|| format!("cannot read a source blob from PR base {revision}"))?;
        let mut output = File::create(target)?;
        if io::copy(&mut (&mut blobs).take(size), &mut output)? != size {
            return Err(format!("incomplete source blob from PR base {revision}").into());
        }
        let mut newline = [0];
        blobs.read_exact(&mut newline)?;
        if newline != [b'\n'] {
            return Err(format!("invalid source blob from PR base {revision}").into());
        }
    }
    Ok(())
}
