use crate::grammar::Language;
use crate::model::{Position, Range};
use tree_sitter::Node;

// Resolve the destination while walking outward. A transfer is unsafe as soon
// as its path crosses a finally clause, before reaching its target or callable.
pub fn leaves_finally(
    language: Language,
    node: Node<'_>,
    source: &str,
    ancestors: &[Node<'_>],
) -> bool {
    let kind = node.kind();
    if !matches!(
        kind,
        "return_statement" | "break_statement" | "continue_statement"
    ) {
        return false;
    }
    let label = node
        .child_by_field_name("label")
        .filter(|_| !restricted_line_break(language, node, source))
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .and_then(identifier_value);
    let mut child = node;
    for &parent in ancestors.iter().rev() {
        if language.callable(parent, source).is_some() {
            return false;
        }
        if parent.kind() == "finally_clause" {
            return true;
        }
        if kind != "return_statement" && target(parent, child, kind, label.as_deref(), source) {
            return false;
        }
        child = parent;
    }
    false
}

// Tree-sitter can attach a label or return operand across a line terminator in
// a comment. ECMAScript inserts a semicolon before that token instead.
pub fn transfer_range(language: Language, node: Node<'_>, source: &str) -> Range {
    if restricted_line_break(language, node, source) {
        let start = node.start_position();
        let keyword_len = node.kind().trim_end_matches("_statement").len();
        Range {
            start: Position {
                line: start.row + 1,
                column: start.column + 1,
            },
            end: Position {
                line: start.row + 1,
                column: start.column + 1 + keyword_len,
            },
        }
    } else {
        node.range().into()
    }
}

fn restricted_line_break(language: Language, node: Node<'_>, source: &str) -> bool {
    if !matches!(language, Language::Typescript | Language::Tsx) {
        return false;
    }
    let Some(keyword) = node.child(0) else {
        return false;
    };
    let mut cursor = node.walk();
    let Some(next) = node
        .children(&mut cursor)
        .skip(1)
        .find(|child| !child.is_extra())
    else {
        return false;
    };
    if next.kind() == ";" {
        return false;
    }
    source
        .get(keyword.end_byte()..next.start_byte())
        .is_some_and(|gap| gap.chars().any(is_line_terminator))
}

fn is_line_terminator(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn target(
    parent: Node<'_>,
    child: Node<'_>,
    kind: &str,
    label: Option<&str>,
    source: &str,
) -> bool {
    if let Some(label) = label {
        return parent.kind() == "labeled_statement"
            && parent
                .child_by_field_name("label")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .and_then(identifier_value)
                .as_deref()
                == Some(label);
    }
    let is_loop = matches!(
        parent.kind(),
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement"
    );
    // In Python, the loop is no longer the target in its else clause.
    let in_else = parent.child_by_field_name("alternative") == Some(child);
    (is_loop && !in_else) || (kind == "break_statement" && parent.kind() == "switch_statement")
}

// JavaScript identifiers compare by StringValue, including Unicode escapes.
fn identifier_value(text: &str) -> Option<String> {
    let mut chars = text.chars().peekable();
    let mut units = Vec::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buf = [0u16; 2];
            units.extend_from_slice(ch.encode_utf16(&mut buf));
            continue;
        }
        read_escape(&mut chars, &mut units)?;
    }
    String::from_utf16(&units).ok()
}

fn read_escape(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    units: &mut Vec<u16>,
) -> Option<()> {
    if chars.next()? != 'u' {
        return None;
    }
    let mut digits = String::new();
    if chars.peek() == Some(&'{') {
        chars.next();
        loop {
            let ch = chars.next()?;
            if ch == '}' {
                break;
            }
            digits.push(ch);
        }
        let ch = char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?;
        let mut buf = [0u16; 2];
        units.extend_from_slice(ch.encode_utf16(&mut buf));
    } else {
        for _ in 0..4 {
            digits.push(chars.next()?);
        }
        units.push(u16::from_str_radix(&digits, 16).ok()?);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn leaves(language: Language, node: Node<'_>, source: &str) -> bool {
        let mut ancestors = Vec::new();
        let mut current = node;
        while let Some(parent) = current.parent() {
            ancestors.push(parent);
            current = parent;
        }
        ancestors.reverse();
        leaves_finally(language, node, source, &ancestors)
    }

    fn lines(language: Language, source: &str) -> Vec<usize> {
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let mut result = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if leaves(language, node, source) {
                result.push(node.start_position().row + 1);
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        result.sort();
        result
    }

    #[test]
    fn typescript_targets_and_callable_boundaries() {
        let source = "function f() {\nouter: for (;;) {\ntry {} finally {\nfor (;;) { break; continue; }\nswitch (x) { case 1: break; case 2: continue; }\ninner: { break inner; }\nbreak outer;\nconst nested = () => { return 1; };\nreturn 2;\n}\n}\n}";
        for language in [Language::Typescript, Language::Tsx] {
            assert_eq!(lines(language, source), vec![5, 7, 9]);
        }
    }

    #[test]
    fn escaped_labels_resolve_by_value() {
        let source = r"function f() {
outer: for (;;) { try {} finally { break \u006futer; } }
𐐀: for (;;) { try {} finally { continue \u{10400}; } }
try {} finally { outer: for (;;) { break \u006futer; } }
}";
        assert_eq!(lines(Language::Typescript, source), vec![2, 3]);
    }

    #[test]
    fn line_terminators_inside_comments_end_restricted_statements() {
        for language in [Language::Typescript, Language::Tsx] {
            for separator in ["\n", "\r", "\u{2028}", "\u{2029}"] {
                let source = format!(
                    "function f() {{ outer: while (true) {{ try {{}} finally {{ while (true) {{ break /*{separator}*/ outer; continue /*{separator}*/ outer; }} inner: {{ break /*{separator}*/ inner; }} return /*{separator}*/ 42; }} }} }}"
                );
                let mut parser = Parser::new();
                parser.set_language(&language.grammar()).unwrap();
                let tree = parser.parse(&source, None).unwrap();
                assert!(
                    !tree.root_node().has_error(),
                    "{}",
                    tree.root_node().to_sexp()
                );
                let mut cursor = tree.walk();
                let mut nodes = vec![tree.root_node()];
                let mut transfers = Vec::new();
                while let Some(node) = nodes.pop() {
                    if matches!(
                        node.kind(),
                        "break_statement" | "continue_statement" | "return_statement"
                    ) {
                        transfers.push((
                            node.start_byte(),
                            leaves(language, node, &source),
                            transfer_range(language, node, &source),
                            node.kind(),
                        ));
                    }
                    nodes.extend(node.children(&mut cursor));
                }
                transfers.sort_by_key(|part| part.0);
                assert_eq!(
                    transfers.iter().map(|part| part.1).collect::<Vec<_>>(),
                    vec![false, false, true, true]
                );
                for (_, _, range, kind) in transfers {
                    assert_eq!(range.end.line, range.start.line);
                    assert_eq!(
                        range.end.column - range.start.column,
                        kind.trim_end_matches("_statement").len()
                    );
                }
            }
        }
    }

    #[test]
    fn explicit_semicolon_after_line_break_remains_in_transfer_range() {
        for language in [Language::Typescript, Language::Tsx] {
            for separator in ["\n", "\r", "\u{2028}", "\u{2029}"] {
                let source = format!(
                    "function f() {{ while (true) {{ try {{}} finally {{ return /*{separator}*/; break /*{separator}*/; continue /*{separator}*/; }} }} }}"
                );
                let mut parser = Parser::new();
                parser.set_language(&language.grammar()).unwrap();
                let tree = parser.parse(&source, None).unwrap();
                assert!(!tree.root_node().has_error());
                let mut nodes = vec![tree.root_node()];
                let mut seen = 0;
                while let Some(node) = nodes.pop() {
                    if matches!(
                        node.kind(),
                        "return_statement" | "break_statement" | "continue_statement"
                    ) {
                        let range = transfer_range(language, node, &source);
                        let original: Range = node.range().into();
                        assert_eq!(
                            (range.end.line, range.end.column),
                            (original.end.line, original.end.column)
                        );
                        assert!(source[node.byte_range()].ends_with(';'));
                        seen += 1;
                    }
                    let mut cursor = node.walk();
                    nodes.extend(node.children(&mut cursor));
                }
                assert_eq!(seen, 3);
            }
        }
    }

    #[test]
    fn python_loop_else_and_nested_functions() {
        let source = "def f():\n    for x in []:\n        try:\n            pass\n        finally:\n            for y in []:\n                break\n            else:\n                break\n            def nested():\n                return 1\n            return 2\n";
        assert_eq!(lines(Language::Python, source), vec![9, 12]);
    }
}
