use crate::duplication::{DEFAULT_MIN_TOKENS, Match, Pair};
use crate::grammar::Language;
use crate::model::{Callable, Diagnostic, ErrorKind, Identity, Rule, SCORING_VERSION};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path};

pub const CONFIG_FILE: &str = ".lint-action.json";
pub const BASELINE_FILE: &str = ".lint-action-baseline.json";
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_LIMIT: u32 = 12;
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
    #[serde(default = "default_limits")]
    limits: BTreeMap<Language, u32>,
    #[serde(default)]
    cognitive_limits: BTreeMap<Language, u32>,
    #[serde(default)]
    duplication: Option<DuplicationConfig>,
    #[serde(default = "all_paths")]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicationConfig {
    #[serde(default = "default_min_tokens")]
    minimum_tokens: u32,
}

fn default_min_tokens() -> u32 {
    DEFAULT_MIN_TOKENS
}

pub struct Policy {
    pub mode: Mode,
    pub duplication_minimum: Option<u32>,
    limits: BTreeMap<Language, u32>,
    cognitive_limits: BTreeMap<Language, u32>,
    include: GlobSet,
    exclude: GlobSet,
}

impl Policy {
    pub fn load(path: &Path) -> Result<Self, Diagnostic> {
        let invalid = |message: String| policy_error(ErrorKind::InvalidConfig, path, message);
        let config: Config = read_json(path, MAX_FILE_BYTES).map_err(invalid)?;
        let duplication_minimum = config.duplication.map(|rule| rule.minimum_tokens);
        if duplication_minimum == Some(0) {
            return Err(invalid(
                "duplication.minimum_tokens must be positive".into(),
            ));
        }
        if (config.limits.is_empty() && duplication_minimum.is_none())
            || config.limits.values().any(|&limit| limit == 0)
        {
            return Err(invalid(
                "limits must enable at least one language and be positive".into(),
            ));
        }
        for (&language, &limit) in &config.cognitive_limits {
            if limit == 0
                || !Rule::CognitiveComplexity.supports(language)
                || !config.limits.contains_key(&language)
            {
                return Err(invalid(
                    "cognitive_limits requires positive limits for enabled TypeScript or TSX"
                        .into(),
                ));
            }
        }
        if config.include.is_empty() {
            return Err(invalid("include must contain at least one glob".into()));
        }
        Ok(Self {
            mode: config.mode,
            duplication_minimum,
            limits: config.limits,
            cognitive_limits: config.cognitive_limits,
            include: compile_globs(&config.include).map_err(invalid)?,
            exclude: compile_globs(&config.exclude).map_err(invalid)?,
        })
    }

    pub fn limit(&self, language: Language, rule: Rule) -> Option<u32> {
        let cyclomatic_limit = self.limits.get(&language).copied()?;
        match rule {
            Rule::CyclomaticComplexity => Some(cyclomatic_limit),
            Rule::CognitiveComplexity if rule.supports(language) => Some(
                self.cognitive_limits
                    .get(&language)
                    .copied()
                    .unwrap_or(DEFAULT_LIMIT),
            ),
            Rule::CognitiveComplexity | Rule::SourceDuplication => None,
        }
    }

    pub fn selects(&self, language: Language) -> bool {
        self.limits.contains_key(&language)
            || (self.duplication_minimum.is_some() && Rule::SourceDuplication.supports(language))
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

fn default_limits() -> BTreeMap<Language, u32> {
    [
        Language::Rust,
        Language::Typescript,
        Language::Tsx,
        Language::Python,
    ]
    .into_iter()
    .map(|language| (language, DEFAULT_LIMIT))
    .collect()
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub(crate) scoring_version: u32,
    pub(crate) entries: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) duplication_entries: Vec<DuplicationEntry>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    #[serde(default)]
    pub(crate) rule: Rule,
    pub(crate) identity: Identity,
    pub(crate) score: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DuplicationEntry {
    pub(crate) occurrences: Pair,
    pub(crate) score: usize,
    pub(crate) tokens: Vec<String>,
}

#[derive(Default)]
pub struct BaselineScores {
    pub complexity: BTreeMap<Rule, BTreeMap<Identity, Option<u64>>>,
    duplication: BTreeMap<Pair, Option<DuplicationEntry>>,
}

impl BaselineScores {
    pub fn covers(&self, pair: &Pair, found: &Match<'_>) -> bool {
        self.duplication
            .get(pair)
            .and_then(Option::as_ref)
            .is_some_and(|entry| found.is_part_of(&entry.tokens))
    }
}

fn valid_identity(identity: &Identity) -> bool {
    let source_path = Path::new(&identity.path);
    normal_relative_path(source_path)
        && slash_path(source_path).as_deref() == Some(&identity.path)
        && Language::from_path(source_path) == Some(identity.language)
        && !identity.scope.is_empty()
        && identity
            .scope
            .iter()
            .all(|scope| !scope.kind.is_empty() && !scope.name.is_empty())
}

impl Baseline {
    pub fn load(path: &Path) -> Result<BaselineScores, Diagnostic> {
        Self::load_review(path).map(|(_, scores)| scores)
    }

    pub fn load_review(path: &Path) -> Result<(Self, BaselineScores), Diagnostic> {
        let invalid = |message: String| policy_error(ErrorKind::InvalidBaseline, path, message);
        let baseline: Self = read_json(path, MAX_BASELINE_BYTES).map_err(invalid)?;
        if baseline.scoring_version != SCORING_VERSION {
            return Err(invalid(format!(
                "baseline scoring version {} differs from {SCORING_VERSION}",
                baseline.scoring_version
            )));
        }
        let mut scores = BaselineScores::default();
        for Entry {
            rule,
            identity,
            score,
        } in baseline.entries.iter().cloned()
        {
            if !valid_identity(&identity)
                || !rule.supports(identity.language)
                || rule == Rule::SourceDuplication
                || score == 0
            {
                return Err(invalid(format!(
                    "invalid baseline entry for {:?}",
                    identity.path
                )));
            }
            scores
                .complexity
                .entry(rule)
                .or_default()
                .entry(identity)
                .and_modify(|value| *value = None)
                .or_insert(Some(score));
        }
        for mut entry in baseline.duplication_entries.iter().cloned() {
            let [left, right] = &entry.occurrences;
            if left == right
                || entry.occurrences.iter().any(|identity| {
                    !valid_identity(identity)
                        || !Rule::SourceDuplication.supports(identity.language)
                })
                || entry.score == 0
                || entry.score != entry.tokens.len()
                || entry.tokens.iter().any(|token| {
                    !token
                        .split_once('\0')
                        .is_some_and(|(kind, text)| !kind.is_empty() && !text.is_empty())
                })
            {
                return Err(invalid("invalid source duplication baseline entry".into()));
            }
            entry.occurrences.sort();
            scores
                .duplication
                .entry(entry.occurrences.clone())
                .and_modify(|value| *value = None)
                .or_insert(Some(entry));
        }
        Ok((baseline, scores))
    }

    pub fn generate<'a>(
        callables: impl Iterator<Item = &'a Callable>,
        matches: impl Iterator<Item = (Pair, &'a Match<'a>)>,
    ) -> Result<Self, Diagnostic> {
        let mut entries: Vec<_> = callables
            .map(|callable| Entry {
                rule: callable.rule,
                identity: callable.identity.clone(),
                score: callable.score,
            })
            .collect();
        entries
            .sort_by(|left, right| (&left.identity, left.rule).cmp(&(&right.identity, right.rule)));
        let mut duplication_entries = Vec::new();
        let mut remaining_bytes = MAX_BASELINE_BYTES;
        for (occurrences, found) in matches {
            let tokens = found.tokens();
            let score = tokens.len();
            let entry = DuplicationEntry {
                occurrences,
                score,
                tokens: tokens.map(str::to_owned).collect(),
            };
            let size = serde_json::to_vec_pretty(&entry)
                .expect("baseline entries serialize to JSON")
                .len() as u64;
            remaining_bytes = remaining_bytes
                .checked_sub(size)
                .ok_or_else(baseline_too_large)?;
            duplication_entries.push(entry);
        }
        duplication_entries.sort_by(|left, right| left.occurrences.cmp(&right.occurrences));
        Ok(Self {
            scoring_version: SCORING_VERSION,
            entries,
            duplication_entries,
        })
    }

    pub fn to_json(&self) -> Result<Vec<u8>, Diagnostic> {
        let mut json = serde_json::to_vec_pretty(self).expect("baseline serializes to JSON");
        json.push(b'\n');
        if json.len() as u64 > MAX_BASELINE_BYTES {
            return Err(baseline_too_large());
        }
        Ok(json)
    }
}

fn baseline_too_large() -> Diagnostic {
    Diagnostic::new(
        ErrorKind::InvalidBaseline,
        None,
        format!("generated baseline exceeds {MAX_BASELINE_BYTES} bytes"),
    )
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
