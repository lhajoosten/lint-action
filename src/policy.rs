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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Pass,
    Warn,
    Error,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    mode: Mode,
    #[serde(default)]
    severities: BTreeMap<Rule, Severity>,
    #[serde(default = "default_limits")]
    limits: BTreeMap<Language, u32>,
    #[serde(default)]
    cognitive_limits: BTreeMap<Language, u32>,
    #[serde(default)]
    duplication: Option<DuplicationConfig>,
    #[serde(default)]
    finally_exit: bool,
    #[serde(default)]
    empty_handler: bool,
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
    severities: BTreeMap<Rule, Severity>,
    pub duplication_minimum: Option<u32>,
    pub finally_exit: bool,
    pub empty_handler: bool,
    limits: BTreeMap<Language, u32>,
    cognitive_limits: BTreeMap<Language, u32>,
    include: GlobSet,
    exclude: GlobSet,
}

impl Policy {
    pub fn load(path: &Path) -> Result<Self, Diagnostic> {
        let invalid = |message: String| policy_error(ErrorKind::InvalidConfig, path, message);
        let config: Config = read_json(path, MAX_FILE_BYTES).map_err(invalid)?;
        let duplication_minimum = config.duplication.as_ref().map(|rule| rule.minimum_tokens);
        validate_config(&config, duplication_minimum).map_err(invalid)?;
        Ok(Self {
            mode: config.mode,
            severities: config.severities,
            duplication_minimum,
            finally_exit: config.finally_exit,
            empty_handler: config.empty_handler,
            limits: config.limits,
            cognitive_limits: config.cognitive_limits,
            include: compile_globs(&config.include).map_err(invalid)?,
            exclude: compile_globs(&config.exclude).map_err(invalid)?,
        })
    }

    pub fn severity(&self, rule: Rule) -> Severity {
        self.severities
            .get(&rule)
            .copied()
            .unwrap_or(if rule == Rule::EmptyHandler {
                Severity::Warn
            } else {
                Severity::Error
            })
    }

    pub fn active(&self, rule: Rule) -> bool {
        self.severity(rule) != Severity::Pass
    }

    pub fn any_active(&self) -> bool {
        (self.active(Rule::CyclomaticComplexity) && !self.limits.is_empty())
            || (self.active(Rule::CognitiveComplexity)
                && self
                    .limits
                    .keys()
                    .any(|language| Rule::CognitiveComplexity.supports(*language)))
            || (self.active(Rule::SourceDuplication) && self.duplication_minimum.is_some())
            || (self.active(Rule::FinallyExit) && self.finally_exit)
            || (self.active(Rule::EmptyHandler) && self.empty_handler)
    }

    pub fn active_for(&self, language: Language, rule: Rule) -> bool {
        self.active(rule)
            && match rule {
                Rule::CyclomaticComplexity => self.limits.contains_key(&language),
                Rule::CognitiveComplexity => self.limit(language, rule).is_some(),
                Rule::FinallyExit => self.finally_exit && rule.supports(language),
                Rule::EmptyHandler => self.empty_handler && rule.supports(language),
                Rule::SourceDuplication => {
                    self.duplication_minimum.is_some() && rule.supports(language)
                }
            }
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
            Rule::CognitiveComplexity
            | Rule::SourceDuplication
            | Rule::FinallyExit
            | Rule::EmptyHandler => None,
        }
    }

    pub fn selects(&self, language: Language) -> bool {
        self.limits.contains_key(&language)
            || (self.finally_exit && Rule::FinallyExit.supports(language))
            || (self.empty_handler && Rule::EmptyHandler.supports(language))
            || (self.duplication_minimum.is_some() && Rule::SourceDuplication.supports(language))
    }

    pub fn includes(&self, path: &Path) -> bool {
        self.include.is_match(path)
            && !self.exclude.is_match(path)
            && !path
                .ancestors()
                .skip(1)
                .any(|parent| !parent.as_os_str().is_empty() && self.excludes_directory(parent))
    }

    pub fn excludes_directory(&self, path: &Path) -> bool {
        self.exclude.is_match(path)
            || self
                .exclude
                .is_match(format!("{}/", path.to_string_lossy()))
    }
}

fn validate_config(config: &Config, duplication_minimum: Option<u32>) -> Result<(), String> {
    if duplication_minimum == Some(0) {
        return Err("duplication.minimum_tokens must be positive".into());
    }
    if (config.limits.is_empty()
        && duplication_minimum.is_none()
        && !config.finally_exit
        && !config.empty_handler)
        || config.limits.values().any(|&limit| limit == 0)
    {
        return Err("limits must enable at least one language and be positive".into());
    }
    if config.cognitive_limits.iter().any(|(&language, &limit)| {
        limit == 0
            || !Rule::CognitiveComplexity.supports(language)
            || !config.limits.contains_key(&language)
    }) {
        return Err(
            "cognitive_limits requires positive limits for enabled TypeScript or TSX".into(),
        );
    }
    if config.include.is_empty() {
        return Err("include must contain at least one glob".into());
    }
    Ok(())
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
            .is_some_and(|entry| found.is_part_of(entry.tokens.iter().map(String::as_str)))
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
    pub fn read(path: &Path) -> Result<Self, Diagnostic> {
        let invalid = |message: String| policy_error(ErrorKind::InvalidBaseline, path, message);
        let mut baseline: Self = read_json(path, MAX_BASELINE_BYTES).map_err(invalid)?;
        if baseline.scoring_version != SCORING_VERSION {
            return Err(invalid(format!(
                "baseline scoring version {} differs from {SCORING_VERSION}",
                baseline.scoring_version
            )));
        }
        for Entry {
            rule,
            identity,
            score,
        } in &baseline.entries
        {
            if !valid_identity(identity)
                || !rule.supports(identity.language)
                || matches!(
                    *rule,
                    Rule::SourceDuplication | Rule::FinallyExit | Rule::EmptyHandler
                )
                || *score == 0
            {
                return Err(invalid(format!(
                    "invalid baseline entry for {:?}",
                    identity.path
                )));
            }
        }
        for entry in &mut baseline.duplication_entries {
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
        }
        Ok(baseline)
    }

    pub fn load(path: &Path) -> Result<BaselineScores, Diagnostic> {
        let baseline = Self::read(path)?;
        let mut scores = BaselineScores::default();
        for Entry {
            rule,
            identity,
            score,
        } in baseline.entries
        {
            scores
                .complexity
                .entry(rule)
                .or_default()
                .entry(identity)
                .and_modify(|value| *value = None)
                .or_insert(Some(score));
        }
        for entry in baseline.duplication_entries {
            scores
                .duplication
                .entry(entry.occurrences.clone())
                .and_modify(|value| *value = None)
                .or_insert(Some(entry));
        }
        Ok(scores)
    }

    pub fn preserve_advisory(&mut self, path: &Path, policy: &Policy) -> Result<(), Diagnostic> {
        if policy
            .severities
            .values()
            .all(|level| *level == Severity::Error)
        {
            return Ok(());
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(policy_error(
                    ErrorKind::InvalidBaseline,
                    path,
                    error.to_string(),
                ));
            }
            Ok(_) => {}
        }
        let previous = Self::read(path)?;
        self.entries.extend(
            previous
                .entries
                .into_iter()
                .filter(|entry| policy.severity(entry.rule) != Severity::Error),
        );
        if policy.severity(Rule::SourceDuplication) != Severity::Error {
            self.duplication_entries
                .extend(previous.duplication_entries);
        }
        Ok(())
    }

    pub fn generate<'a>(
        callables: impl Iterator<Item = &'a Callable>,
        matches: impl Iterator<Item = (Pair, &'a Match<'a>)>,
    ) -> Result<Self, Diagnostic> {
        let entries = callables
            .map(|callable| Entry {
                rule: callable.rule,
                identity: callable.identity.clone(),
                score: callable.score,
            })
            .collect();
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
        Ok(Self {
            scoring_version: SCORING_VERSION,
            entries,
            duplication_entries,
        })
    }

    pub fn into_json(mut self) -> Result<Vec<u8>, Diagnostic> {
        self.entries
            .sort_by(|left, right| (&left.identity, left.rule).cmp(&(&right.identity, right.rule)));
        self.duplication_entries
            .sort_by(|left, right| left.occurrences.cmp(&right.occurrences));
        let mut json = serde_json::to_vec_pretty(&self).expect("baseline serializes to JSON");
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
