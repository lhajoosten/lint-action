use crate::model::Contribution;
use tree_sitter::Node;

pub fn enters_nested_flow(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "if_statement" | "if_expression" | "elif_clause" | "ternary_expression" => {
            parent.child_by_field_name("consequence") == Some(node)
                || (parent.kind() == "ternary_expression"
                    && parent.child_by_field_name("alternative") == Some(node))
        }
        "else_clause" => node.is_named() && !node.is_extra() && !is_if(node),
        "switch_statement"
        | "match_statement"
        | "match_expression"
        | "for_statement"
        | "for_in_statement"
        | "for_expression"
        | "while_statement"
        | "while_expression"
        | "loop_expression"
        | "do_statement"
        | "catch_clause"
        | "list_comprehension"
        | "set_comprehension"
        | "dictionary_comprehension"
        | "generator_expression" => parent.child_by_field_name("body") == Some(node),
        "except_clause" => node.kind() == "block",
        "let_declaration" => parent.child_by_field_name("alternative") == Some(node),
        "conditional_expression" => {
            let mut cursor = parent.walk();
            let mut expressions = parent
                .named_children(&mut cursor)
                .filter(|child| !child.is_extra());
            expressions.next() == Some(node) || expressions.nth(1) == Some(node)
        }
        _ => false,
    }
}

pub fn score(node: Node<'_>, depth: u32, contributions: &mut Vec<Contribution>) -> u32 {
    let (construct, token, nesting) = match node.kind() {
        "if_statement" | "if_expression"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "else_clause") =>
        {
            ("else if", "if", 0)
        }
        "if_statement" | "if_expression" => ("if", "if", depth),
        "elif_clause" => ("else if", "elif", 0),
        "else_clause" if first_expression(node).is_some_and(|child| !is_if(child)) => {
            ("else", "else", 0)
        }
        "for_statement" | "for_in_statement" | "for_expression" | "for_in_clause" => {
            ("for", "for", depth)
        }
        "while_statement" | "while_expression" => ("while", "while", depth),
        "loop_expression" => ("loop", "loop", depth),
        "do_statement" => ("do", "do", depth),
        "switch_statement" => ("switch", "switch", depth),
        "catch_clause" => ("catch", "catch", depth),
        "except_clause" => ("except", "except", depth),
        "match_statement" | "match_expression" => ("match", "match", depth),
        "match_pattern" if node.child_by_field_name("condition").is_some() => {
            ("guard", "if", depth)
        }
        "if_clause" => {
            let construct = if node
                .parent()
                .is_some_and(|parent| parent.kind() == "case_clause")
            {
                "guard"
            } else {
                "comprehension filter"
            };
            (construct, "if", depth)
        }
        "let_declaration" if node.child_by_field_name("alternative").is_some() => {
            ("let else", "else", depth)
        }
        "try_expression" => ("error propagation", "?", 0),
        "ternary_expression" => ("ternary", "?", depth),
        "conditional_expression" => ("ternary", "if", depth),
        "break_expression" if named_label(node) => ("labeled break", "break", 0),
        "continue_expression" if named_label(node) => ("labeled continue", "continue", 0),
        "break_statement" if node.child_by_field_name("label").is_some() => {
            ("labeled break", "break", 0)
        }
        "continue_statement" if node.child_by_field_name("label").is_some() => {
            ("labeled continue", "continue", 0)
        }
        "binary_expression" | "boolean_operator" | "let_chain"
            if is_logical(node) && is_logical_root(node) =>
        {
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

fn is_if(node: Node<'_>) -> bool {
    matches!(node.kind(), "if_statement" | "if_expression")
}

fn named_label(node: Node<'_>) -> bool {
    node.named_children(&mut node.walk())
        .any(|child| child.kind() == "label")
}

fn is_logical(node: Node<'_>) -> bool {
    node.kind() == "let_chain"
        || (matches!(node.kind(), "binary_expression" | "boolean_operator")
            && operator(node).is_some())
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
            return !is_logical(parent);
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
            "let_chain" => {
                let mut cursor = node.walk();
                let children: Vec<_> = node.children(&mut cursor).collect();
                pending.extend(children.into_iter().rev());
            }
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
