use crate::duplication::{Match, Occurrence, Pair};
use crate::model::{Callable, Diagnostic, Identity, Range, Rule};
use crate::policy::{BaselineScores, Policy, Severity};
use crate::{ScoredTree, Status, complexity_status, duplication_status, unique};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
pub struct Comparison {
    format_version: u32,
    pub(crate) status: &'static str,
    pub(crate) base_revision: Option<String>,
    pub(crate) candidate_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<&'static str>,
    pub base_diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unchanged: Option<usize>,
    pub(crate) changes: Vec<Change>,
}

#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Category {
    New,
    Worsened,
    Improved,
    Fixed,
    Removed,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MatchStatus {
    Matched,
    Unmatched,
    Ambiguous,
}

#[derive(Serialize)]
pub(crate) struct Change {
    pub(crate) category: Category,
    pub(crate) match_status: MatchStatus,
    pub(crate) rule: Rule,
    pub(crate) limit: u32,
    pub(crate) previous: Option<Observation>,
    pub(crate) current: Option<Observation>,
}

#[derive(Serialize)]
pub(crate) struct Observation {
    pub(crate) score: u64,
    pub(crate) status: Option<Status>,
    #[serde(flatten)]
    pub(crate) location: Location,
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum Location {
    Callable {
        #[serde(flatten)]
        identity: Identity,
        name: String,
        range: Range,
    },
    Duplication {
        occurrences: [Occurrence; 2],
    },
}

impl Observation {
    fn position(&self) -> (&str, usize, usize) {
        let (path, range) = match &self.location {
            Location::Callable {
                identity, range, ..
            } => (&identity.path, range),
            Location::Duplication { occurrences } => (&occurrences[0].path, &occurrences[0].range),
        };
        (path, range.start.line, range.start.column)
    }
}

impl Change {
    fn order(&self) -> (Category, bool, (&str, usize, usize), Rule) {
        let occurrence = self.current.as_ref().or(self.previous.as_ref()).unwrap();
        (
            self.category,
            matches!(self.match_status, MatchStatus::Ambiguous),
            occurrence.position(),
            self.rule,
        )
    }
}

impl Comparison {
    pub fn new(base_revision: Option<String>, candidate_revision: Option<String>) -> Self {
        Self {
            format_version: 1,
            status: "unavailable",
            base_revision,
            candidate_revision,
            reason: None,
            base_diagnostics: Vec::new(),
            unchanged: None,
            changes: Vec::new(),
        }
    }

    pub fn unavailable(&mut self, reason: &'static str) {
        self.reason = Some(reason);
    }

    pub fn compare(
        &mut self,
        base: &ScoredTree<'_>,
        candidate: &ScoredTree<'_>,
        policy: &Policy,
        baseline: &BaselineScores,
    ) {
        self.status = "complete";
        let mut unchanged = 0;
        let mut callables = BTreeMap::<_, [Vec<_>; 2]>::new();
        let sides = [base, candidate];
        for (side, tree) in sides.into_iter().enumerate() {
            for item in &tree.callables {
                callables.entry((item.rule, &item.identity)).or_default()[side].push(item);
            }
        }
        for ((rule, identity), [previous, current]) in callables {
            let Some(limit) = policy.limit(identity.language, rule) else {
                continue;
            };
            if policy.severity(rule) == Severity::Pass {
                continue;
            }
            let observe = |item: &Callable, side: usize| Observation {
                score: item.score,
                status: (item.score > u64::from(limit))
                    .then(|| complexity_status(item, sides[side].scope_counts, policy, baseline)),
                location: Location::Callable {
                    identity: identity.clone(),
                    name: item.name.clone(),
                    range: item.range,
                },
            };
            let stable = [&previous, &current]
                .into_iter()
                .zip(sides)
                .all(|(items, tree)| {
                    items.len() <= 1
                        && (!tree.scope_counts.contains_key(identity)
                            || unique(identity, tree.scope_counts))
                });
            let over_limit = |item: &Callable| item.score > u64::from(limit);
            if let ([before], [after]) = (previous.as_slice(), current.as_slice())
                && stable
            {
                let category = match (over_limit(before), over_limit(after)) {
                    (false, false) => continue,
                    (false, true) => Category::New,
                    (true, false) => Category::Fixed,
                    (true, true) if before.score == after.score => {
                        unchanged += 1;
                        continue;
                    }
                    (true, true) if before.score < after.score => Category::Worsened,
                    (true, true) => Category::Improved,
                };
                self.changes.push(Change {
                    category,
                    match_status: MatchStatus::Matched,
                    rule,
                    limit,
                    previous: Some(observe(before, 0)),
                    current: Some(observe(after, 1)),
                });
            } else {
                let match_status = if stable {
                    MatchStatus::Unmatched
                } else {
                    MatchStatus::Ambiguous
                };
                for item in previous.into_iter().filter(|item| over_limit(item)) {
                    self.changes.push(Change {
                        category: Category::Removed,
                        match_status,
                        rule,
                        limit,
                        previous: Some(observe(item, 0)),
                        current: None,
                    });
                }
                for item in current.into_iter().filter(|item| over_limit(item)) {
                    self.changes.push(Change {
                        category: Category::New,
                        match_status,
                        rule,
                        limit,
                        previous: None,
                        current: Some(observe(item, 1)),
                    });
                }
            }
        }

        if let Some(limit) = policy.duplication_minimum {
            unchanged += self.compare_duplicates(sides, limit, policy, baseline);
        }
        self.unchanged = Some(unchanged);
        self.changes
            .sort_by(|left, right| left.order().cmp(&right.order()));
    }

    fn compare_duplicates(
        &mut self,
        sides: [&ScoredTree<'_>; 2],
        limit: u32,
        policy: &Policy,
        baseline: &BaselineScores,
    ) -> usize {
        let rule = Rule::SourceDuplication;
        let observe = |found: &Match<'_>, pair: Option<&Pair>| {
            let detail = found.report();
            Observation {
                score: detail.score as u64,
                status: Some(duplication_status(found, pair, policy, baseline)),
                location: Location::Duplication {
                    occurrences: detail.occurrences,
                },
            }
        };
        let mut pairs = BTreeMap::<_, [Vec<_>; 2]>::new();
        for (side, tree) in sides.into_iter().enumerate() {
            for (found, pair) in tree.matches.iter().zip(&tree.pairs) {
                if let Some(pair) = pair {
                    pairs.entry(pair).or_default()[side].push(found);
                } else {
                    let occurrence = Some(observe(found, None));
                    let (category, previous, current) = if side == 0 {
                        (Category::Removed, occurrence, None)
                    } else {
                        (Category::New, None, occurrence)
                    };
                    self.changes.push(Change {
                        category,
                        match_status: MatchStatus::Ambiguous,
                        rule,
                        limit,
                        previous,
                        current,
                    });
                }
            }
        }

        let mut unchanged = 0;
        for (pair, [previous, current]) in pairs {
            if let ([before], [after]) = (previous.as_slice(), current.as_slice()) {
                let before_score = before.tokens().len();
                let after_score = after.tokens().len();
                let category = if after_score > before_score {
                    before
                        .is_part_of(after.tokens())
                        .then_some(Category::Worsened)
                } else if after.is_part_of(before.tokens()) {
                    if after_score == before_score {
                        unchanged += 1;
                        continue;
                    }
                    Some(Category::Improved)
                } else {
                    None
                };
                if let Some(category) = category {
                    self.changes.push(Change {
                        category,
                        match_status: MatchStatus::Matched,
                        rule,
                        limit,
                        previous: Some(observe(before, Some(pair))),
                        current: Some(observe(after, Some(pair))),
                    });
                    continue;
                }
            }
            let match_status = if previous.len() > 1 || current.len() > 1 {
                MatchStatus::Ambiguous
            } else {
                MatchStatus::Unmatched
            };
            for found in previous {
                self.changes.push(Change {
                    category: Category::Removed,
                    match_status,
                    rule,
                    limit,
                    previous: Some(observe(found, Some(pair))),
                    current: None,
                });
            }
            for found in current {
                self.changes.push(Change {
                    category: Category::New,
                    match_status,
                    rule,
                    limit,
                    previous: None,
                    current: Some(observe(found, Some(pair))),
                });
            }
        }
        unchanged
    }
}
