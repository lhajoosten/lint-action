use crate::grammar::Language;
use tree_sitter::Node;

pub fn detect(
    language: Language,
    node: Node<'_>,
    source: &str,
) -> Option<(&'static str, &'static str)> {
    let (body, cause) = handler_body(language, node, source)?;
    let shape = body_shape(language, body)?;
    if has_waiver(node, body, source) {
        return None;
    }
    Some((cause, shape))
}

fn handler_body<'a>(
    language: Language,
    node: Node<'a>,
    source: &str,
) -> Option<(Node<'a>, &'static str)> {
    match language {
        Language::Typescript | Language::Tsx if node.kind() == "catch_clause" => {
            Some((node.child_by_field_name("body")?, "empty catch"))
        }
        Language::Python if node.kind() == "except_clause" => {
            let mut cursor = node.walk();
            if node.children(&mut cursor).any(|child| child.kind() == "*") {
                return None;
            }
            let value = node.child_by_field_name("value");
            if value.is_some_and(|value| {
                let expression = if value.kind() == "as_pattern" {
                    value.named_child(0).unwrap_or(value)
                } else {
                    value
                };
                expression.kind() != "identifier" || text(expression, source) != "Exception"
            }) {
                return None;
            }
            let mut cursor = node.walk();
            let body = node
                .named_children(&mut cursor)
                .find(|child| child.kind() == "block")?;
            Some((
                body,
                if value.is_some() {
                    "except Exception"
                } else {
                    "bare except"
                },
            ))
        }
        _ => None,
    }
}

fn body_shape(language: Language, body: Node<'_>) -> Option<&'static str> {
    let mut cursor = body.walk();
    let statements: Vec<_> = body
        .named_children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .collect();
    let empty = match language {
        Language::Python => {
            statements.len() == 1
                && (statements[0].kind() == "pass_statement"
                    || (statements[0].kind() == "expression_statement"
                        && statements[0].named_child_count() == 1
                        && statements[0]
                            .named_child(0)
                            .is_some_and(|child| child.kind() == "ellipsis")))
        }
        _ => statements.is_empty(),
    };
    if !empty {
        return None;
    }
    let shape = if language == Language::Python {
        if statements[0].kind() == "pass_statement" {
            "pass"
        } else {
            "ellipsis"
        }
    } else if body.named_child_count() == 0 {
        "empty body"
    } else {
        "comments only"
    };
    Some(shape)
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn has_waiver(handler: Node<'_>, body: Node<'_>, source: &str) -> bool {
    let valid = |comment: Node<'_>| {
        if comment.kind() != "comment" {
            return false;
        }
        let text = text(comment, source).trim();
        let content = text
            .strip_prefix('#')
            .or_else(|| text.strip_prefix("//"))
            .or_else(|| text.strip_prefix("/*").and_then(|s| s.strip_suffix("*/")))
            .unwrap_or("")
            .trim();
        content
            .strip_prefix("lint-gate: allow empty-handler --")
            .is_some_and(|reason| !reason.trim().is_empty())
    };
    let mut cursor = body.walk();
    if body.named_children(&mut cursor).any(valid) {
        return true;
    }
    let mut cursor = handler.walk();
    handler.named_children(&mut cursor).any(|child| {
        child.kind() == "comment"
            && child.start_position().row > handler.start_position().row
            && child.start_position().column >= body.start_position().column
            && child.end_byte() <= body.start_byte()
            && valid(child)
    })
}
