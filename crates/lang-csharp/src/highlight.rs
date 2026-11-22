//! Token classes for colouring C# in the code view, from the same
//! tree-sitter grammar the analyser uses.

use blueprint_model::lang::{Highlight, TokenClass};
use tree_sitter::{Node, Parser};

/// The coloured tokens of `src`, sorted by start and not overlapping.
/// Never panics: unparsable bytes give fewer tokens.
#[must_use]
pub fn highlight(src: &[u8]) -> Vec<Highlight> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Ancestors come with each node: asking the tree for a parent walks
    // down from the root, which on deep nesting is quadratic.
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut ancestors: Vec<Node<'_>> = Vec::new();
    loop {
        let node = cursor.node();
        let whole = class_of_whole(node);
        let leaf = node.child_count() == 0;
        let class = whole.or_else(|| {
            if leaf {
                class_of_leaf(node, &ancestors)
            } else {
                None
            }
        });
        if let Some(class) = class
            && let (Ok(start), Ok(end)) = (
                u32::try_from(node.start_byte()),
                u32::try_from(node.end_byte()),
            )
            && end > start
        {
            out.push(Highlight { start, end, class });
        }
        if whole.is_none() && cursor.goto_first_child() {
            ancestors.push(node);
            continue;
        }
        let mut done = false;
        loop {
            if cursor.node().id() == root.id() {
                done = true;
                break;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                done = true;
                break;
            }
            ancestors.pop();
        }
        if done {
            break;
        }
    }
    out.sort_by_key(|h| (h.start, h.end));
    let mut kept: Vec<Highlight> = Vec::with_capacity(out.len());
    for h in out {
        if kept.last().is_none_or(|k| h.start >= k.end) {
            kept.push(h);
        }
    }
    kept
}

/// Nodes coloured as one token, children included.
fn class_of_whole(node: Node<'_>) -> Option<TokenClass> {
    match node.kind() {
        "comment" => Some(TokenClass::Comment),
        "string_literal"
        | "verbatim_string_literal"
        | "raw_string_literal"
        | "interpolated_string_expression"
        | "character_literal" => Some(TokenClass::String),
        "integer_literal" | "real_literal" | "boolean_literal" => Some(TokenClass::Number),
        "predefined_type" => Some(TokenClass::Type),
        _ => None,
    }
}

/// Leaves coloured by their kind and place.
fn class_of_leaf(node: Node<'_>, ancestors: &[Node<'_>]) -> Option<TokenClass> {
    let kind = node.kind();
    if !node.is_named() {
        return (!kind.is_empty() && kind.bytes().all(|b| b.is_ascii_lowercase()))
            .then_some(TokenClass::Keyword);
    }
    if kind != "identifier" {
        return matches!(kind, "modifier" | "this" | "base" | "null_literal")
            .then_some(TokenClass::Keyword);
    }
    let (&parent, rest) = ancestors.split_last()?;
    let is = |of: Node<'_>, field: &str| {
        of.child_by_field_name(field)
            .is_some_and(|c| c.id() == node.id())
    };
    match parent.kind() {
        "method_declaration" | "constructor_declaration" | "local_function_statement"
            if is(parent, "name") =>
        {
            Some(TokenClass::Function)
        }
        "invocation_expression" if is(parent, "function") => Some(TokenClass::Function),
        "member_access_expression" if is(parent, "name") => {
            let grand = *rest.last()?;
            (grand.kind() == "invocation_expression"
                && grand
                    .child_by_field_name("function")
                    .is_some_and(|c| c.id() == parent.id()))
            .then_some(TokenClass::Function)
        }
        "class_declaration"
        | "struct_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
            if is(parent, "name") =>
        {
            Some(TokenClass::Type)
        }
        // A type position: a declaration's type, a base, a generic.
        "variable_declaration"
        | "parameter"
        | "property_declaration"
        | "base_list"
        | "object_creation_expression"
        | "type_argument_list"
        | "generic_name"
            if !is(parent, "name") =>
        {
            Some(TokenClass::Type)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(src: &str) -> Vec<(String, TokenClass)> {
        highlight(src.as_bytes())
            .into_iter()
            .map(|h| (src[h.start as usize..h.end as usize].to_owned(), h.class))
            .collect()
    }

    #[test]
    fn colours_keywords_types_functions_literals_and_comments() {
        let got = classes(
            "// note\npublic class Point : IShape {\n  public int Add(int a, Point b) {\n    var s = \"x\";\n    Helper(1);\n    return Util.Go(true);\n  }\n}\n",
        );
        let has = |text: &str, class| got.iter().any(|(t, c)| t == text && *c == class);
        assert!(has("// note", TokenClass::Comment), "{got:?}");
        assert!(has("public", TokenClass::Keyword));
        assert!(has("class", TokenClass::Keyword));
        assert!(has("return", TokenClass::Keyword));
        assert!(has("Add", TokenClass::Function));
        assert!(has("Helper", TokenClass::Function));
        assert!(has("Go", TokenClass::Function), "Util.Go(...) colours Go");
        assert!(has("int", TokenClass::Type));
        assert!(has("Point", TokenClass::Type));
        assert!(has("IShape", TokenClass::Type));
        assert!(has("\"x\"", TokenClass::String));
        assert!(has("1", TokenClass::Number));
        assert!(has("true", TokenClass::Number));
        assert!(!got.iter().any(|(t, _)| t == "s" || t == "a"), "{got:?}");
    }

    #[test]
    fn tokens_are_sorted_and_disjoint_on_any_input() {
        for src in [
            &b"class C { void F() { \"unterminated"[..],
            b"\xff\xfe garbage {{{{",
            b"",
            b"var s = $\"a{b}c\"; /* c */",
        ] {
            let h = highlight(src);
            assert!(h.windows(2).all(|w| w[0].end <= w[1].start), "{h:?}");
            assert!(h.iter().all(|t| t.end as usize <= src.len()));
        }
    }
}
