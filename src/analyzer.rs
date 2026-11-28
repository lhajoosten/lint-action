use crate::grammar::{Decision, Language, Scope};
use crate::model::{Callable, Diagnostic, ErrorKind, FinallyExit, Identity, Rule};
use crate::{cognitive, duplication};
use std::collections::BTreeMap;
use tree_sitter::{Node, Parser};

#[derive(Default)]
pub struct Analysis {
    pub callables: Vec<Callable>,
    pub finally_exits: Vec<FinallyExit>,
    pub diagnostics: Vec<Diagnostic>,
    pub scope_counts: BTreeMap<Identity, usize>,
    pub duplication: Vec<duplication::Source>,
}

struct Frame {
    cyclomatic_index: usize,
    cognitive_index: Option<usize>,
    depth: u32,
}

pub fn analyze(
    path: &str,
    language: Language,
    source: &str,
    duplication_budget: Option<(usize, usize)>,
    cyclomatic_enabled: bool,
    cognitive_enabled: bool,
    finally_enabled: bool,
) -> Result<Analysis, Diagnostic> {
    let parser_error =
        |message: &str| Diagnostic::new(ErrorKind::ParseError, Some(path.to_owned()), message);
    let mut parser = Parser::new();
    parser
        .set_language(&language.grammar())
        .map_err(|error| parser_error(&error.to_string()))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| parser_error("Tree-sitter returned no syntax tree"))?;

    let mut result = Analysis::default();
    let mut duplicate_source = duplication_budget.map(|_| duplication::Source::new(path));
    let mut scope = Vec::<Scope>::new();
    let cognitive = cognitive_enabled && Rule::CognitiveComplexity.supports(language);
    let mut active = Vec::<Frame>::new();
    let mut entries = Vec::new();
    let mut ancestors = Vec::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            let message = if node.is_missing() {
                format!("Tree-sitter expected {}", node.kind())
            } else {
                "Tree-sitter could not parse this syntax".to_owned()
            };
            let mut diagnostic = parser_error(&message);
            diagnostic.range = Some(node.range().into());
            result.diagnostics.push(diagnostic);
        }

        let callable = language.callable(node, source);
        let is_callable = callable.is_some();
        let new_scope = callable.or_else(|| language.container(node, source));
        let entered_scope = new_scope.is_some();
        if let Some(new_scope) = new_scope {
            let name = new_scope.name.clone();
            scope.push(new_scope);
            let identity = Identity {
                path: path.to_owned(),
                language,
                scope: scope.clone(),
            };
            *result.scope_counts.entry(identity.clone()).or_default() += 1;
            if is_callable {
                active.push(register_callable(
                    &mut result.callables,
                    &identity,
                    &name,
                    node,
                    cyclomatic_enabled,
                    cognitive,
                ));
            }
        }
        if finally_enabled {
            collect_finally(
                &mut result.finally_exits,
                path,
                language,
                node,
                source,
                &scope,
                &ancestors,
            );
        }
        let mut nested = 0;
        if let Some(frame) = active.last_mut() {
            if cyclomatic_enabled {
                result.callables[frame.cyclomatic_index].score +=
                    u64::from(decision_points(language, node));
            }
            if let Some(index) = frame.cognitive_index {
                nested = if is_callable {
                    0
                } else {
                    cognitive::nesting(language, node, &ancestors)
                };
                frame.depth += nested;
                let metric = &mut result.callables[index];
                metric.score += u64::from(cognitive::score_for_language(
                    language,
                    node,
                    &ancestors,
                    frame.depth,
                    &mut metric.contributions,
                ));
            }
        }

        if let Some(collector) = duplicate_source.as_mut() {
            collector.enter(node, source, language, &scope);
            let (tokens, bytes) = duplication_budget.expect("collector has a budget");
            if collector.tokens > tokens || collector.bytes > bytes {
                result.diagnostics.push(duplication::resource_limit());
                duplicate_source = None;
            }
        }
        entries.push((entered_scope, is_callable, nested));
        ancestors.push(node);
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if let Some(collector) = duplicate_source.as_mut() {
                collector.exit(cursor.node());
            }
            let (entered_scope, is_callable, nested) = entries.pop().expect("cursor has an entry");
            ancestors.pop();
            if entered_scope {
                scope.pop();
            }
            if is_callable {
                active.pop();
            }
            if nested > 0 {
                active
                    .last_mut()
                    .expect("nesting is inside a callable")
                    .depth -= nested;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                if result
                    .diagnostics
                    .iter()
                    .any(|error| error.kind == ErrorKind::ParseError)
                {
                    result.callables.clear();
                    result.finally_exits.clear();
                    result.scope_counts.clear();
                } else {
                    result.duplication.extend(duplicate_source);
                }
                return Ok(result);
            }
        }
    }
}

fn decision_points(language: Language, node: Node<'_>) -> u32 {
    match language.decision(node) {
        Some(
            Decision::Branch
            | Decision::Loop
            | Decision::ShortCircuit
            | Decision::Guard
            | Decision::ConditionalExit
            | Decision::OptionalAccess
            | Decision::DefaultValue,
        ) => 1,
        None => 0,
    }
}

fn register_callable(
    callables: &mut Vec<Callable>,
    identity: &Identity,
    name: &str,
    node: Node<'_>,
    cyclomatic: bool,
    cognitive: bool,
) -> Frame {
    let base = callables.len();
    for rule in [Rule::CyclomaticComplexity, Rule::CognitiveComplexity]
        .into_iter()
        .filter(|rule| match rule {
            Rule::CyclomaticComplexity => cyclomatic,
            Rule::CognitiveComplexity => cognitive,
            Rule::SourceDuplication | Rule::FinallyExit => false,
        })
    {
        callables.push(Callable {
            rule,
            identity: identity.clone(),
            name: name.to_owned(),
            range: node.range().into(),
            score: u64::from(rule == Rule::CyclomaticComplexity),
            contributions: Vec::new(),
        });
    }
    Frame {
        cyclomatic_index: base + usize::from(!cyclomatic),
        cognitive_index: cognitive.then_some(base + usize::from(cyclomatic)),
        depth: 0,
    }
}

fn collect_finally(
    exits: &mut Vec<FinallyExit>,
    path: &str,
    language: Language,
    node: Node<'_>,
    source: &str,
    scope: &[Scope],
    ancestors: &[Node<'_>],
) {
    if crate::finally::leaves_finally(language, node, source, ancestors) {
        exits.push(FinallyExit {
            rule: Rule::FinallyExit,
            identity: Identity { path: path.to_owned(), language, scope: scope.to_vec() },
            name: scope.last().map_or_else(|| "<module>".to_owned(), |part| part.name.clone()),
            range: crate::finally::transfer_range(language, node, source),
            transfer: node.kind().trim_end_matches("_statement").to_owned(),
            message: "Control flow leaving finally can suppress an exception or override an earlier return.",
        });
    }
}

#[cfg(test)]
mod python_cognitive_tests {
    use super::*;

    fn score(source: &str) -> (u64, Vec<(&'static str, u32)>) {
        let analysis = analyze(
            "test.py",
            Language::Python,
            source,
            None,
            false,
            true,
            false,
        )
        .unwrap();
        assert!(analysis.diagnostics.is_empty());
        let metric = &analysis.callables[0];
        (
            metric.score,
            metric
                .contributions
                .iter()
                .map(|c| (c.construct, c.points))
                .collect(),
        )
    }

    #[test]
    fn branches_headers_and_unscored_blocks() {
        let source = "def f(x):\n    if (1 if x else 2):\n        while x:\n            pass\n    elif x:\n        if x: pass\n    else:\n        for x in y:\n            pass\n    try:\n        with x:\n            pass\n    except* ValueError:\n        if x: pass\n    else:\n        pass\n    finally:\n        pass\n";
        assert_eq!(
            score(source),
            (
                14,
                vec![
                    ("if", 1),
                    ("conditional expression", 1),
                    ("while", 2),
                    ("elif", 1),
                    ("if", 2),
                    ("else", 1),
                    ("for", 2),
                    ("except", 1),
                    ("if", 2),
                    ("else", 1)
                ]
            )
        );
    }

    #[test]
    fn comprehensions_follow_evaluation_depth() {
        let source = "def f(xs):\n    return [[z for z in x] for x in [y for y in xs] if x for q in [a for a in x] if q]\n";
        assert_eq!(
            score(source),
            (
                14,
                vec![
                    ("comprehension for", 3),
                    ("comprehension for", 1),
                    ("comprehension for", 1),
                    ("comprehension if", 2),
                    ("comprehension for", 2),
                    ("comprehension for", 2),
                    ("comprehension if", 3)
                ]
            )
        );
    }

    #[test]
    fn conditional_comments_do_not_change_branch_depth() {
        let source = "def f(a,b,c):\n    return ((1 if a else 2)  # first branch\n            if b else (3 if c else 4)  # second branch\n           )\n";
        assert_eq!(
            score(source),
            (
                5,
                vec![
                    ("conditional expression", 1),
                    ("conditional expression", 2),
                    ("conditional expression", 2)
                ]
            )
        );
    }

    #[test]
    fn logical_runs_break_at_not_and_calls() {
        assert_eq!(
            score(
                "def f(a,b,c,d):\n    return a and (b and c) or not (a or b) and call(c and d)\n"
            ),
            (
                5,
                vec![("and", 1), ("or", 1), ("and", 1), ("or", 1), ("and", 1)]
            )
        );
    }
}
