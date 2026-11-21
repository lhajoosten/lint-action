use serde::{Deserialize, Serialize};
use tree_sitter::{Language as Grammar, Node};

pub const ANONYMOUS: &str = "<anonymous>";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Typescript,
    Tsx,
    Python,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: String,
    pub name: String,
}

#[derive(Clone, Copy)]
pub enum Decision {
    Branch,
    Loop,
    ShortCircuit,
    Guard,
    ConditionalExit,
    OptionalAccess,
    DefaultValue,
}

impl Language {
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "ts" | "mts" | "cts" => Some(Self::Typescript),
            "tsx" => Some(Self::Tsx),
            "py" | "pyi" => Some(Self::Python),
            _ => None,
        }
    }

    pub fn duplication_family(self) -> Self {
        match self {
            Self::Tsx => Self::Typescript,
            language => language,
        }
    }

    pub fn statement_block(self) -> &'static str {
        match self {
            Self::Rust | Self::Python => "block",
            Self::Typescript | Self::Tsx => "statement_block",
        }
    }

    pub fn grammar(self) -> Grammar {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Typescript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }

    pub fn callable(self, node: Node<'_>, source: &str) -> Option<Scope> {
        let kind = match (self, node.kind()) {
            (Self::Rust, "function_item") | (Self::Python, "function_definition") => "function",
            (Self::Rust, "closure_expression") => "closure",
            (Self::Python, "lambda") => "lambda",
            (Self::Typescript | Self::Tsx, "function_declaration" | "function_expression") => {
                "function"
            }
            (
                Self::Typescript | Self::Tsx,
                "generator_function" | "generator_function_declaration",
            ) => "generator",
            (Self::Typescript | Self::Tsx, "arrow_function") => "arrow",
            (Self::Typescript | Self::Tsx, "method_definition") if has_child(node, "get") => {
                "getter"
            }
            (Self::Typescript | Self::Tsx, "method_definition") if has_child(node, "set") => {
                "setter"
            }
            (Self::Typescript | Self::Tsx, "method_definition") => "method",
            _ => return None,
        };
        let name = field(node, "name", source)
            .or_else(|| binding(node, source))
            .unwrap_or(ANONYMOUS);
        Some(scope(kind, name))
    }

    pub fn container(self, node: Node<'_>, source: &str) -> Option<Scope> {
        match (self, node.kind()) {
            (Self::Rust, "impl_item") => {
                let target = field(node, "type", source)?;
                let name = match field(node, "trait", source) {
                    Some(trait_name) => format!("{trait_name} for {target}"),
                    None => target.to_owned(),
                };
                Some(scope("impl", &name))
            }
            (Self::Rust, "mod_item" | "trait_item" | "const_item" | "static_item")
            | (Self::Python, "class_definition")
            | (
                Self::Typescript | Self::Tsx,
                "class_declaration" | "abstract_class_declaration" | "internal_module",
            ) => Some(scope(node.kind(), field(node, "name", source)?)),
            (Self::Typescript | Self::Tsx, "class" | "object") if node.is_named() => Some(scope(
                node.kind(),
                field(node, "name", source)
                    .or_else(|| binding(node, source))
                    .unwrap_or(ANONYMOUS),
            )),
            _ => None,
        }
    }

    pub fn decision(self, node: Node<'_>) -> Option<Decision> {
        use Decision::*;
        match (self, node.kind()) {
            (Self::Rust, "if_expression" | "match_arm")
            | (
                Self::Typescript | Self::Tsx,
                "if_statement" | "switch_case" | "catch_clause" | "ternary_expression",
            )
            | (
                Self::Python,
                "if_statement"
                | "elif_clause"
                | "except_clause"
                | "case_clause"
                | "conditional_expression",
            ) => Some(Branch),
            (Self::Python, "if_clause")
                if node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "case_clause") =>
            {
                Some(Guard)
            }
            (Self::Python, "if_clause") => Some(Branch),

            (Self::Rust, "for_expression" | "while_expression" | "loop_expression")
            | (
                Self::Typescript | Self::Tsx,
                "for_statement" | "for_in_statement" | "while_statement" | "do_statement",
            )
            | (Self::Python, "for_statement" | "while_statement" | "for_in_clause") => Some(Loop),

            (Self::Rust, "match_pattern") if node.child_by_field_name("condition").is_some() => {
                Some(Guard)
            }

            (Self::Rust, "try_expression") => Some(ConditionalExit),
            (Self::Rust, "let_declaration")
                if node.child_by_field_name("alternative").is_some() =>
            {
                Some(ConditionalExit)
            }

            (Self::Typescript | Self::Tsx, "optional_chain") => Some(OptionalAccess),
            (Self::Typescript | Self::Tsx, "?.")
                if node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "call_expression") =>
            {
                Some(OptionalAccess)
            }
            (Self::Typescript | Self::Tsx, "assignment_pattern" | "object_assignment_pattern") => {
                Some(DefaultValue)
            }
            (Self::Typescript | Self::Tsx, "required_parameter" | "optional_parameter")
                if node.child_by_field_name("value").is_some() =>
            {
                Some(DefaultValue)
            }

            (Self::Rust, "binary_expression") if operator_is(node, &["&&", "||"]) => {
                Some(ShortCircuit)
            }
            (Self::Rust, "&&")
                if node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "let_chain") =>
            {
                Some(ShortCircuit)
            }
            (
                Self::Typescript | Self::Tsx,
                "binary_expression" | "augmented_assignment_expression",
            ) if operator_is(node, &["&&", "||", "??", "&&=", "||=", "??="]) => Some(ShortCircuit),
            (Self::Python, "boolean_operator") => Some(ShortCircuit),
            _ => None,
        }
    }
}

fn scope(kind: &str, name: &str) -> Scope {
    Scope {
        kind: kind.to_owned(),
        name: name.to_owned(),
    }
}

fn field<'a>(node: Node<'_>, name: &str, source: &'a str) -> Option<&'a str> {
    source.get(node.child_by_field_name(name)?.byte_range())
}

fn binding<'a>(mut node: Node<'_>, source: &'a str) -> Option<&'a str> {
    while let Some(parent) = node.parent() {
        let (value, name) = match parent.kind() {
            "parenthesized_expression" | "as_expression" | "satisfies_expression" => {
                node = parent;
                continue;
            }
            "return_statement" | "return_expression" => return Some("<return>"),
            "variable_declarator" => ("value", "name"),
            "let_declaration" => ("value", "pattern"),
            "assignment" | "assignment_expression" => ("right", "left"),
            "pair" => ("value", "key"),
            "public_field_definition" => ("value", "name"),
            _ => return None,
        };
        if parent.child_by_field_name(value) == Some(node) {
            return field(parent, name, source);
        }
        return None;
    }
    None
}

fn has_child(node: Node<'_>, kind: &str) -> bool {
    node.children(&mut node.walk())
        .any(|child| child.kind() == kind)
}

fn operator_is(node: Node<'_>, operators: &[&str]) -> bool {
    node.child_by_field_name("operator")
        .is_some_and(|operator| operators.contains(&operator.kind()))
}
