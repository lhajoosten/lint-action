use crate::grammar::Language;
use crate::model::Contribution;
use tree_sitter::Node;

pub fn score_for_language(
    language: Language,
    node: Node<'_>,
    ancestors: &[Node<'_>],
    depth: u32,
    contributions: &mut Vec<Contribution>,
) -> u32 {
    if language == Language::Python {
        python_score(node, ancestors, depth, contributions)
    } else {
        score(node, depth, contributions)
    }
}

fn enters_nested_flow(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "if_statement" | "ternary_expression" => {
            parent.child_by_field_name("consequence") == Some(node)
                || (parent.kind() == "ternary_expression"
                    && parent.child_by_field_name("alternative") == Some(node))
        }
        "else_clause" => node.is_named() && !node.is_extra() && node.kind() != "if_statement",
        "switch_statement" | "for_statement" | "for_in_statement" | "while_statement"
        | "do_statement" | "catch_clause" => parent.child_by_field_name("body") == Some(node),
        _ => false,
    }
}

pub fn score(node: Node<'_>, depth: u32, contributions: &mut Vec<Contribution>) -> u32 {
    let (construct, token, nesting) = match node.kind() {
        "if_statement"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "else_clause") =>
        {
            ("else if", "if", 0)
        }
        "if_statement" => ("if", "if", depth),
        "else_clause"
            if first_expression(node).is_some_and(|child| child.kind() != "if_statement") =>
        {
            ("else", "else", 0)
        }
        "for_statement" | "for_in_statement" => ("for", "for", depth),
        "while_statement" => ("while", "while", depth),
        "do_statement" => ("do", "do", depth),
        "switch_statement" => ("switch", "switch", depth),
        "catch_clause" => ("catch", "catch", depth),
        "ternary_expression" => ("ternary", "?", depth),
        "break_statement" if node.child_by_field_name("label").is_some() => {
            ("labeled break", "break", 0)
        }
        "continue_statement" if node.child_by_field_name("label").is_some() => {
            ("labeled continue", "continue", 0)
        }
        "binary_expression" if operator(node).is_some() && is_logical_root(node) => {
            return score_logical_runs(node, contributions);
        }
        _ => return 0,
    };

    let location = node
        .children(&mut node.walk())
        .find(|child| child.kind() == token)
        .unwrap_or(node);
    record(contributions, location, construct, nesting)
}

fn record(
    contributions: &mut Vec<Contribution>,
    node: Node<'_>,
    construct: &'static str,
    nesting: u32,
) -> u32 {
    let points = 1 + nesting;
    contributions.push(Contribution {
        construct,
        points,
        nesting,
        range: node.range().into(),
    });
    points
}

fn operator(node: Node<'_>) -> Option<Node<'_>> {
    let operator = node.child_by_field_name("operator")?;
    matches!(operator.kind(), "&&" | "||" | "??" | "and" | "or").then_some(operator)
}

fn first_expression(node: Node<'_>) -> Option<Node<'_>> {
    node.named_children(&mut node.walk())
        .find(|child| !child.is_extra())
}

fn is_logical_root(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if parent.kind() != "parenthesized_expression" {
            return parent.kind() != "binary_expression" || operator(parent).is_none();
        }
        node = parent;
    }
    true
}

fn score_logical_runs(root: Node<'_>, contributions: &mut Vec<Contribution>) -> u32 {
    let mut pending = vec![root];
    let mut previous = None;
    let mut score = 0;
    while let Some(node) = pending.pop() {
        match node.kind() {
            "&&" | "||" | "??" | "and" | "or" => {
                if previous != Some(node.kind()) {
                    score += record(contributions, node, node.kind(), 0);
                    previous = Some(node.kind());
                }
            }
            "parenthesized_expression" => pending.extend(first_expression(node)),
            "binary_expression" | "boolean_operator" => {
                if let Some(operator) = operator(node) {
                    pending.extend(node.child_by_field_name("right"));
                    pending.push(operator);
                    pending.extend(node.child_by_field_name("left"));
                }
            }
            _ => {}
        }
    }
    score
}

pub fn nesting(language: Language, node: Node<'_>, ancestors: &[Node<'_>]) -> u32 {
    if language != Language::Python {
        return u32::from(enters_nested_flow(node));
    }
    let Some(&parent) = ancestors.last() else {
        return 0;
    };
    match parent.kind() {
        "if_statement" | "elif_clause" => {
            u32::from(parent.child_by_field_name("consequence") == Some(node))
        }
        "else_clause" | "for_statement" | "while_statement" | "match_statement" => {
            u32::from(parent.child_by_field_name("body") == Some(node))
        }
        "except_clause" => u32::from(node.kind() == "block"),
        "conditional_expression" => conditional_nesting(parent, node),
        "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => comprehension_nesting(parent, node),
        _ => 0,
    }
}

fn conditional_nesting(parent: Node<'_>, node: Node<'_>) -> u32 {
    let children: Vec<_> = parent
        .named_children(&mut parent.walk())
        .filter(|child| !child.is_extra())
        .collect();
    u32::from(children.first() == Some(&node) || children.last() == Some(&node))
}

fn comprehension_nesting(parent: Node<'_>, node: Node<'_>) -> u32 {
    let body = parent.child_by_field_name("body") == Some(node);
    let mut count = 0;
    for child in parent.named_children(&mut parent.walk()) {
        if child == node && !body {
            return count;
        }
        if child.kind() == "for_in_clause" {
            count += 1;
        }
    }
    if body { count } else { 0 }
}

pub fn python_score(
    node: Node<'_>,
    ancestors: &[Node<'_>],
    depth: u32,
    contributions: &mut Vec<Contribution>,
) -> u32 {
    if node.kind() == "boolean_operator" && is_python_logical_root(ancestors) {
        return score_logical_runs(node, contributions);
    }
    let Some((construct, token, nesting)) =
        python_construct(node, ancestors.last().copied(), depth)
    else {
        return 0;
    };
    let location = node
        .children(&mut node.walk())
        .find(|child| child.kind() == token)
        .unwrap_or(node);
    record(contributions, location, construct, nesting)
}

fn python_construct(
    node: Node<'_>,
    parent: Option<Node<'_>>,
    depth: u32,
) -> Option<(&'static str, &'static str, u32)> {
    Some(match node.kind() {
        "if_statement" => ("if", "if", depth),
        "elif_clause" => ("elif", "elif", 0),
        "else_clause" => ("else", "else", 0),
        "for_statement" => ("for", "for", depth),
        "while_statement" => ("while", "while", depth),
        "except_clause" => ("except", "except", depth),
        "match_statement" => ("match", "match", depth),
        _ => return python_expression_construct(node, parent, depth),
    })
}

fn python_expression_construct(
    node: Node<'_>,
    parent: Option<Node<'_>>,
    depth: u32,
) -> Option<(&'static str, &'static str, u32)> {
    Some(match node.kind() {
        "conditional_expression" => ("conditional expression", "if", depth),
        "for_in_clause" => ("comprehension for", "for", depth),
        "if_clause" if parent.is_some_and(|p| p.kind() == "case_clause") => {
            ("pattern guard", "if", 0)
        }
        "if_clause" => ("comprehension if", "if", depth),
        _ => return None,
    })
}

fn is_python_logical_root(ancestors: &[Node<'_>]) -> bool {
    ancestors
        .iter()
        .rev()
        .find(|parent| parent.kind() != "parenthesized_expression")
        .is_none_or(|parent| parent.kind() != "boolean_operator")
}
