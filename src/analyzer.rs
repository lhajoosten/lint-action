use crate::grammar::{Decision, Language, Scope};
use crate::model::{Callable, Diagnostic, ErrorKind, Identity, Rule};
use crate::{cognitive, duplication};
use std::collections::BTreeMap;
use tree_sitter::{Node, Parser};

#[derive(Default)]
pub struct Analysis {
    pub callables: Vec<Callable>,
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
                let base = result.callables.len();
                let rules = [Rule::CyclomaticComplexity, Rule::CognitiveComplexity];
                for rule in rules.into_iter().filter(|rule| {
                    rule.supports(language)
                        && match rule {
                            Rule::CyclomaticComplexity => cyclomatic_enabled,
                            Rule::CognitiveComplexity => cognitive,
                            Rule::SourceDuplication => false,
                        }
                }) {
                    result.callables.push(Callable {
                        rule,
                        identity: identity.clone(),
                        name: name.clone(),
                        range: node.range().into(),
                        score: u64::from(rule == Rule::CyclomaticComplexity),
                        contributions: Vec::new(),
                    });
                }
                active.push(Frame {
                    cyclomatic_index: base + usize::from(!cyclomatic_enabled),
                    cognitive_index: cognitive.then_some(base + usize::from(cyclomatic_enabled)),
                    depth: 0,
                });
            }
        }
        let mut nested = false;
        if let Some(frame) = active.last_mut() {
            if cyclomatic_enabled {
                result.callables[frame.cyclomatic_index].score +=
                    u64::from(decision_points(language, node));
            }
            if let Some(index) = frame.cognitive_index {
                nested = !is_callable && cognitive::enters_nested_flow(node);
                frame.depth += u32::from(nested);
                let metric = &mut result.callables[index];
                metric.score += u64::from(cognitive::score(
                    node,
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
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if let Some(collector) = duplicate_source.as_mut() {
                collector.exit(cursor.node());
            }
            let (entered_scope, is_callable, nested) = entries.pop().expect("cursor has an entry");
            if entered_scope {
                scope.pop();
            }
            if is_callable {
                active.pop();
            }
            if nested {
                active
                    .last_mut()
                    .expect("nesting is inside a callable")
                    .depth -= 1;
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
