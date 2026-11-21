use crate::grammar::{Language, Scope};
use serde::{Deserialize, Serialize};

pub const SCORING_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    #[default]
    CyclomaticComplexity,
    CognitiveComplexity,
    SourceDuplication,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl From<tree_sitter::Range> for Range {
    fn from(range: tree_sitter::Range) -> Self {
        let position = |point: tree_sitter::Point| Position {
            line: point.row + 1,
            column: point.column + 1,
        };
        Self {
            start: position(range.start_point),
            end: position(range.end_point),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub path: String,
    pub language: Language,
    pub scope: Vec<Scope>,
}

#[derive(Serialize)]
pub struct Callable {
    pub rule: Rule,
    #[serde(flatten)]
    pub identity: Identity,
    pub name: String,
    pub range: Range,
    pub score: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<Contribution>,
}

#[derive(Serialize)]
pub struct Contribution {
    pub construct: &'static str,
    pub points: u32,
    pub nesting: u32,
    pub range: Range,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidConfig,
    InvalidBaseline,
    InvalidPath,
    UnreadableSource,
    SourceTooLarge,
    InvalidEncoding,
    ParseError,
    EmptyScan,
    DuplicationLimit,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub kind: ErrorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    pub message: String,
}

impl Diagnostic {
    pub fn new(kind: ErrorKind, path: Option<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            path,
            range: None,
            message: message.into(),
        }
    }
}
