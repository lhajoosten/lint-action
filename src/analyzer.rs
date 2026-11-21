use crate::grammar::{Decision, Language, Scope};
use crate::model::{Callable, Diagnostic, ErrorKind, Identity};
use std::collections::BTreeMap;
use tree_sitter::{Node, Parser};

#[derive(Default)]
pub struct Analysis {
    pub callables: Vec<Callable>,
    pub diagnostics: Vec<Diagnostic>,
    pub scope_counts: BTreeMap<Identity, usize>,
}

pub fn analyze(path: &str, language: Language, source: &str) -> Result<Analysis, Diagnostic> {
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
    let mut scope = Vec::<Scope>::new();
    let mut active = Vec::<usize>::new();
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
                active.push(result.callables.len());
                result.callables.push(Callable {
                    identity,
                    name,
                    range: node.range().into(),
                    score: 1,
                });
            }
        }
        if let Some(&index) = active.last() {
            result.callables[index].score += decision_points(language, node);
        }

        entries.push((entered_scope, is_callable));
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            let (entered_scope, is_callable) = entries.pop().expect("cursor has an entry");
            if entered_scope {
                scope.pop();
            }
            if is_callable {
                active.pop();
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                if !result.diagnostics.is_empty() {
                    result.callables.clear();
                    result.scope_counts.clear();
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
