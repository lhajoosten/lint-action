use crate::grammar::{Language, Scope};
use crate::model::{Diagnostic, ErrorKind, Identity, Range, Rule};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use tree_sitter::Node;

pub const DEFAULT_MIN_TOKENS: u32 = 50;
pub const MAX_TOKENS: usize = 800_000;
pub const MAX_TOKEN_BYTES: usize = 32 * 1024 * 1024;
const MAX_WORK: usize = 20_000_000;
const MAX_MATCHES: usize = 25_000;
const HASH_BASE: u64 = 0x9e3779b185ebca87;

pub type Pair = [Identity; 2];

#[derive(Serialize)]
pub struct Occurrence {
    pub path: String,
    pub range: Range,
}

#[derive(Serialize)]
pub struct Duplication {
    pub rule: Rule,
    pub score: usize,
    pub occurrences: [Occurrence; 2],
}

struct Token {
    key: String,
    range: Range,
    anchor: Option<usize>,
}

#[derive(Default)]
struct Block {
    tokens: Vec<Token>,
    anchors: Vec<Identity>,
}

#[derive(Default)]
pub struct Source {
    path: String,
    blocks: Vec<Block>,
    stack: Vec<Option<usize>>,
    pub tokens: usize,
    pub bytes: usize,
}

impl Source {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            ..Self::default()
        }
    }

    pub fn enter(&mut self, node: Node<'_>, source: &str, language: Language, scope: &[Scope]) {
        if node.kind() == "statement_block" {
            if self.stack.is_empty() {
                self.blocks.push(Block::default());
            }
            let block = self.blocks.last_mut().expect("block exists");
            let owns_body = node.parent().is_some_and(|parent| {
                parent.child_by_field_name("body") == Some(node)
                    && (language.callable(parent, source).is_some()
                        || language.container(parent, source).is_some())
            });
            let anchor = if owns_body {
                let index = block.anchors.len();
                block.anchors.push(Identity {
                    path: self.path.clone(),
                    language,
                    scope: scope.to_vec(),
                });
                Some(index)
            } else {
                self.stack.last().copied().flatten()
            };
            self.stack.push(anchor);
        }
        let Some(&anchor) = self.stack.last() else {
            return;
        };
        if node.child_count() != 0
            || matches!(node.kind(), "comment" | "html_comment")
            || node.start_byte() == node.end_byte()
            || (self.stack.len() == 1
                && matches!(node.kind(), "{" | "}")
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "statement_block"))
        {
            return;
        }
        let key = format!("{}\0{}", node.kind(), &source[node.byte_range()]);
        self.tokens += 1;
        self.bytes += key.len();
        self.blocks
            .last_mut()
            .expect("block exists")
            .tokens
            .push(Token {
                key,
                range: node.range().into(),
                anchor,
            });
    }

    pub fn exit(&mut self, node: Node<'_>) {
        if node.kind() == "statement_block" {
            self.stack.pop();
            if self.stack.is_empty()
                && self
                    .blocks
                    .last()
                    .is_some_and(|block| block.tokens.is_empty())
            {
                self.blocks.pop();
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Window {
    block: usize,
    start: usize,
}

#[derive(Clone, Copy)]
struct Candidate {
    windows: [Window; 2],
    len: usize,
}

pub struct Match<'a> {
    blocks: [(&'a str, &'a Block); 2],
    candidate: Candidate,
}

impl Match<'_> {
    pub fn report(&self) -> Duplication {
        Duplication {
            rule: Rule::SourceDuplication,
            score: self.candidate.len,
            occurrences: std::array::from_fn(|index| {
                let (path, block) = self.blocks[index];
                let start = self.candidate.windows[index].start;
                Occurrence {
                    path: path.to_owned(),
                    range: Range {
                        start: block.tokens[start].range.start,
                        end: block.tokens[start + self.candidate.len - 1].range.end,
                    },
                }
            }),
        }
    }

    fn anchor(&self, index: usize) -> Option<&Identity> {
        let block = self.blocks[index].1;
        let start = self.candidate.windows[index].start;
        let end = start + self.candidate.len - 1;
        let anchor = block.tokens[start].anchor?;
        (block.tokens[end].anchor == Some(anchor)).then(|| &block.anchors[anchor])
    }

    pub fn is_part_of(&self, accepted: &[String]) -> bool {
        let tokens = self.tokens();
        if tokens.len() > accepted.len() {
            return false;
        }
        if tokens.len() == accepted.len() {
            return tokens.eq(accepted.iter().map(String::as_str));
        }

        // Search linearly for the shorter run inside the accepted token sequence.
        let tokens: Vec<_> = tokens.collect();
        let mut prefixes = vec![0; tokens.len()];
        let mut matched = 0;
        for (index, &token) in tokens.iter().enumerate().skip(1) {
            matched = extend_prefix(&tokens, &prefixes, matched, token);
            prefixes[index] = matched;
        }
        matched = 0;
        for token in accepted {
            matched = extend_prefix(&tokens, &prefixes, matched, token);
            if matched == tokens.len() {
                return true;
            }
        }
        false
    }

    pub fn tokens(&self) -> impl ExactSizeIterator<Item = &str> {
        let start = self.candidate.windows[0].start;
        self.blocks[0].1.tokens[start..start + self.candidate.len]
            .iter()
            .map(|token| token.key.as_str())
    }
}

fn extend_prefix(tokens: &[&str], prefixes: &[usize], mut matched: usize, token: &str) -> usize {
    while matched > 0 && tokens[matched] != token {
        matched = prefixes[matched - 1];
    }
    matched + usize::from(tokens[matched] == token)
}

pub fn unique_pairs(matches: &[Match<'_>]) -> Vec<Option<Pair>> {
    let mut positions = BTreeMap::new();
    for found in matches {
        for (index, window) in found.candidate.windows.into_iter().enumerate() {
            if let Some(anchor) = found.anchor(index) {
                let end = window.start + found.candidate.len;
                positions
                    .entry(anchor)
                    .and_modify(|value: &mut Option<(Window, usize)>| {
                        *value = value.and_then(|(other, other_end)| {
                            let start = window.start.max(other.start);
                            let end = end.min(other_end);
                            (window.block == other.block && start < end)
                                .then_some((Window { start, ..window }, end))
                        });
                    })
                    .or_insert(Some((window, end)));
            }
        }
    }
    matches
        .iter()
        .map(|found| {
            let left = found.anchor(0)?;
            let right = found.anchor(1)?;
            if left == right || positions[left].is_none() || positions[right].is_none() {
                return None;
            }
            let mut pair = [left.clone(), right.clone()];
            pair.sort();
            Some(pair)
        })
        .collect()
}

// Equal rank IDs represent equal token spans. Doubling spans allows exact
// extension without walking long repeated runs or trusting a rolling hash.
struct Ranks(Vec<Vec<Vec<u32>>>);

impl Ranks {
    fn new(sequences: Vec<Vec<u32>>) -> Self {
        let maximum = sequences.iter().map(Vec::len).max().unwrap_or(0);
        let mut levels = vec![sequences];
        let mut half = 1;
        while half * 2 <= maximum {
            let mut ids = HashMap::new();
            let previous = levels.last().expect("base ranks exist");
            levels.push(
                previous
                    .iter()
                    .map(|tokens| {
                        tokens
                            .iter()
                            .zip(tokens.iter().skip(half))
                            .map(|(&left, &right)| {
                                let next = ids.len() as u32;
                                *ids.entry((left, right)).or_insert(next)
                            })
                            .collect()
                    })
                    .collect(),
            );
            half *= 2;
        }
        Self(levels)
    }

    fn match_length(
        &self,
        left: Window,
        right: Window,
        remaining: &mut usize,
    ) -> Result<usize, Diagnostic> {
        let end = |window: Window| self.0[0][window.block].len() - window.start;
        let mut limit = end(left).min(end(right));
        if left.block == right.block {
            limit = limit.min(right.start - left.start);
        }
        let mut len = 0;
        for level in (0..limit.ilog2() as usize + 1).rev() {
            let width = 1 << level;
            spend(remaining)?;
            if len + width <= limit
                && self.0[level][left.block][left.start + len]
                    == self.0[level][right.block][right.start + len]
            {
                len += width;
            }
        }
        Ok(len)
    }
}

// A minimum-sized window is a candidate seed. Equal preceding tokens identify
// an interior seed on the same alignment, which an earlier seed already covers.
pub fn find(sources: &[Source], minimum: usize) -> Result<Vec<Match<'_>>, Diagnostic> {
    let blocks: Vec<_> = sources
        .iter()
        .flat_map(|source| {
            source
                .blocks
                .iter()
                .map(|block| (source.path.as_str(), block))
        })
        .filter(|(_, block)| block.tokens.len() >= minimum)
        .collect();
    if blocks.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = HashMap::new();
    let sequences: Vec<Vec<u32>> = blocks
        .iter()
        .map(|(_, block)| {
            block
                .tokens
                .iter()
                .map(|token| {
                    let next = ids.len() as u32 + 1;
                    *ids.entry(token.key.as_str()).or_insert(next)
                })
                .collect()
        })
        .collect();
    let ranks = Ranks::new(sequences);
    let mut seeds: HashMap<u64, BTreeMap<u32, Vec<Window>>> = HashMap::new();
    let leading_power = (1..minimum).fold(1u64, |value, _| value.wrapping_mul(HASH_BASE));
    let mut candidates = Vec::new();
    let mut remaining = MAX_WORK;
    for (block, tokens) in ranks.0[0].iter().enumerate() {
        let mut hash = tokens[..minimum].iter().fold(0u64, |hash, token| {
            hash.wrapping_mul(HASH_BASE).wrapping_add(u64::from(*token))
        });
        for start in 0..=tokens.len() - minimum {
            let previous = if start == 0 { 0 } else { tokens[start - 1] };
            let groups = seeds.entry(hash).or_default();
            let right = Window { block, start };
            for (&preceding, windows) in groups.iter() {
                if previous != 0 && previous == preceding {
                    continue;
                }
                for &left in windows {
                    spend(&mut remaining)?;
                    if left.block == block && start - left.start < minimum {
                        continue;
                    }
                    let len = ranks.match_length(left, right, &mut remaining)?;
                    if len >= minimum {
                        if candidates.len() == MAX_MATCHES {
                            return Err(resource_limit());
                        }
                        candidates.push(Candidate {
                            windows: [left, right],
                            len,
                        });
                    }
                }
            }
            groups.entry(previous).or_default().push(right);
            if let Some(&next) = tokens.get(start + minimum) {
                hash = hash
                    .wrapping_sub(u64::from(tokens[start]).wrapping_mul(leading_power))
                    .wrapping_mul(HASH_BASE)
                    .wrapping_add(u64::from(next));
            }
        }
    }

    consolidate(candidates, &mut remaining).map(|matches| {
        matches
            .into_iter()
            .map(|candidate| Match {
                blocks: candidate.windows.map(|window| blocks[window.block]),
                candidate,
            })
            .collect()
    })
}

fn consolidate(
    mut candidates: Vec<Candidate>,
    remaining: &mut usize,
) -> Result<Vec<Candidate>, Diagnostic> {
    candidates.sort_unstable_by_key(|item| (Reverse(item.len), item.windows));
    let mut by_blocks = BTreeMap::<_, Vec<Candidate>>::new();
    for candidate in candidates {
        let kept = by_blocks
            .entry(candidate.windows.map(|window| window.block))
            .or_default();
        let mut redundant = false;
        for larger in kept.iter() {
            spend(remaining)?;
            // Overlap in both occurrences means the shorter alignment does not
            // describe a separate copy. Touching endpoints are disjoint.
            if larger
                .windows
                .iter()
                .zip(candidate.windows)
                .all(|(outer, inner)| {
                    outer.start < inner.start + candidate.len
                        && inner.start < outer.start + larger.len
                })
            {
                redundant = true;
                break;
            }
        }
        if !redundant {
            kept.push(candidate);
        }
    }
    let mut matches: Vec<_> = by_blocks.into_values().flatten().collect();
    matches.sort_unstable_by_key(|item| item.windows);
    Ok(matches)
}

fn spend(remaining: &mut usize) -> Result<(), Diagnostic> {
    *remaining = remaining.checked_sub(1).ok_or_else(resource_limit)?;
    Ok(())
}

pub fn resource_limit() -> Diagnostic {
    Diagnostic::new(
        ErrorKind::DuplicationLimit,
        None,
        "source duplication exceeded its analysis budget; narrow the source selection or increase minimum_tokens",
    )
}
