use crate::grammar::Language;
use crate::model::{Callable, Diagnostic, ErrorKind, Identity, SCORING_VERSION};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path};

pub const CONFIG_FILE: &str = ".lint-action.json";
pub const BASELINE_FILE: &str = ".lint-action-baseline.json";
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BASELINE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    ReportOnly,
    Strict,
    Baseline,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    mode: Mode,
    limits: BTreeMap<Language, u32>,
    #[serde(default = "all_paths")]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

pub struct Policy {
    pub mode: Mode,
    limits: BTreeMap<Language, u32>,
    include: GlobSet,
    exclude: GlobSet,
}

impl Policy {
    pub fn load(path: &Path) -> Result<Self, Diagnostic> {
        let invalid = |message: String| policy_error(ErrorKind::InvalidConfig, path, message);
        let config: Config = read_json(path, MAX_FILE_BYTES).map_err(invalid)?;
        if config.limits.is_empty() || config.limits.values().any(|&limit| limit == 0) {
            return Err(invalid(
                "limits must enable at least one language and be positive".into(),
            ));
        }
        if config.include.is_empty() {
            return Err(invalid("include must contain at least one glob".into()));
        }
        Ok(Self {
            mode: config.mode,
            limits: config.limits,
            include: compile_globs(&config.include).map_err(invalid)?,
            exclude: compile_globs(&config.exclude).map_err(invalid)?,
        })
    }

    pub fn limit(&self, language: Language) -> Option<u32> {
        self.limits.get(&language).copied()
    }

    pub fn includes(&self, path: &Path) -> bool {
        self.include.is_match(path) && !self.exclude.is_match(path)
    }

    pub fn excludes_directory(&self, path: &Path) -> bool {
        self.exclude.is_match(path)
            || self
                .exclude
                .is_match(format!("{}/", path.to_string_lossy()))
    }
}

fn all_paths() -> Vec<String> {
    vec!["**".to_owned()]
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if pattern.is_empty()
            || pattern
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
            || pattern.contains('\\')
        {
            return Err(format!(
                "expected a root-relative slash-separated glob, got {pattern:?}"
            ));
        }
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| error.to_string())?;
        builder.add(glob);
    }
    builder.build().map_err(|error| error.to_string())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    scoring_version: u32,
    entries: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    identity: Identity,
    score: u32,
}

pub type BaselineScores = BTreeMap<Identity, Option<u32>>;

impl Baseline {
    pub fn load(path: &Path) -> Result<BaselineScores, Diagnostic> {
        let invalid = |message: String| policy_error(ErrorKind::InvalidBaseline, path, message);
        let baseline: Self = read_json(path, MAX_BASELINE_BYTES).map_err(invalid)?;
        if baseline.scoring_version != SCORING_VERSION {
            return Err(invalid(format!(
                "baseline scoring version {} differs from {SCORING_VERSION}",
                baseline.scoring_version
            )));
        }
        let mut scores = BTreeMap::new();
        for Entry { identity, score } in baseline.entries {
            let source_path = Path::new(&identity.path);
            if !normal_relative_path(source_path)
                || slash_path(source_path).as_deref() != Some(&identity.path)
                || Language::from_path(source_path) != Some(identity.language)
                || identity.scope.is_empty()
                || identity
                    .scope
                    .iter()
                    .any(|scope| scope.kind.is_empty() || scope.name.is_empty())
                || score == 0
            {
                return Err(invalid(format!(
                    "invalid baseline entry for {:?}",
                    identity.path
                )));
            }
            scores
                .entry(identity)
                .and_modify(|value| *value = None)
                .or_insert(Some(score));
        }
        Ok(scores)
    }

    pub fn generate<'a>(callables: impl Iterator<Item = &'a Callable>) -> Self {
        let mut entries: Vec<_> = callables
            .map(|callable| Entry {
                identity: callable.identity.clone(),
                score: callable.score,
            })
            .collect();
        entries.sort_by(|left, right| left.identity.cmp(&right.identity));
        Self {
            scoring_version: SCORING_VERSION,
            entries,
        }
    }
}

pub fn normal_relative_path(path: &Path) -> bool {
    path.components().next().is_some()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

pub fn slash_path(path: &Path) -> Option<String> {
    let components: Option<Vec<_>> = path
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect();
    Some(components?.join("/"))
}

pub fn bounded_read(path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    let file = File::open(path)?;
    if file.metadata()?.len() > limit {
        return Ok(None);
    }
    let mut contents = Vec::new();
    file.take(limit + 1).read_to_end(&mut contents)?;
    Ok((contents.len() as u64 <= limit).then_some(contents))
}

fn read_json<T: DeserializeOwned>(path: &Path, limit: u64) -> Result<T, String> {
    let bytes = bounded_read(path, limit)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("file exceeds {limit} bytes"))?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn policy_error(kind: ErrorKind, path: &Path, message: String) -> Diagnostic {
    Diagnostic::new(kind, Some(path.to_string_lossy().into_owned()), message)
}
