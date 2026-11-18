//! Token classes for colouring Rust in the code view (#93), from the same
//! tree-sitter grammar the analyser uses.

use blueprint_model::lang::{Highlight, TokenClass};
use tree_sitter::{Node, Parser};

/// The coloured tokens of `src`, sorted by start and not overlapping.
/// Never panics: unparsable bytes give fewer tokens, not an error.
#[must_use]
pub fn highlight(src: &[u8]) -> Vec<Highlight> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    // Depth-first with the cursor: no recursion, whatever the nesting.
    'walk: loop {
        let node = cursor.node();
        let whole = class_of_whole(node);
        let leaf = node.child_count() == 0;
        let class = whole.or_else(|| if leaf { class_of_leaf(node) } else { None });
        if let Some(class) = class
            && let (Ok(start), Ok(end)) = (
                u32::try_from(node.start_byte()),
                u32::try_from(node.end_byte()),
            )
            && end > start
        {
            out.push(Highlight { start, end, class });
        }
        // A node coloured whole is not entered.
        if whole.is_none() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    out.sort_by_key(|h| (h.start, h.end));
    // Drop anything that starts inside the token before it.
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
        "line_comment" | "block_comment" => Some(TokenClass::Comment),
        "string_literal" | "raw_string_literal" | "char_literal" => Some(TokenClass::String),
        "integer_literal" | "float_literal" | "boolean_literal" => Some(TokenClass::Number),
        _ => None,
    }
}

/// Leaves coloured by their kind and place.
fn class_of_leaf(node: Node<'_>) -> Option<TokenClass> {
    let kind = node.kind();
    if !node.is_named() {
        // Anonymous leaves are punctuation and keywords; keywords are words.
        return kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_')
            .then_some(TokenClass::Keyword)
            .filter(|_| !kind.is_empty());
    }
    match kind {
        "type_identifier" | "primitive_type" => Some(TokenClass::Type),
        "self" | "super" | "crate" | "mutable_specifier" => Some(TokenClass::Keyword),
        "identifier" => {
            let parent = node.parent()?;
            let named = |field: &str| {
                parent
                    .child_by_field_name(field)
                    .is_some_and(|c| c.id() == node.id())
            };
            match parent.kind() {
                "function_item" | "function_signature_item" if named("name") => {
                    Some(TokenClass::Function)
                }
                "call_expression" if named("function") => Some(TokenClass::Function),
                "macro_invocation" | "macro_definition" if named("macro") || named("name") => {
                    Some(TokenClass::Function)
                }
                "scoped_identifier" if named("name") => {
                    let grand = parent.parent()?;
                    (grand.kind() == "call_expression"
                        && grand
                            .child_by_field_name("function")
                            .is_some_and(|c| c.id() == parent.id()))
                    .then_some(TokenClass::Function)
                }
                _ => None,
            }
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
            "// note\npub fn add(a: u32, b: Point) -> u32 {\n    let s = \"x\";\n    helper(1);\n    a::b(true)\n}\n",
        );
        let has = |text: &str, class| got.iter().any(|(t, c)| t == text && *c == class);
        assert!(has("// note", TokenClass::Comment), "{got:?}");
        assert!(has("pub", TokenClass::Keyword));
        assert!(has("fn", TokenClass::Keyword));
        assert!(has("let", TokenClass::Keyword));
        assert!(has("add", TokenClass::Function));
        assert!(has("helper", TokenClass::Function));
        assert!(has("b", TokenClass::Function), "a::b(...) colours b");
        assert!(has("u32", TokenClass::Type));
        assert!(has("Point", TokenClass::Type));
        assert!(has("\"x\"", TokenClass::String));
        assert!(has("1", TokenClass::Number));
        assert!(has("true", TokenClass::Number));
        assert!(!got.iter().any(|(t, _)| t == "a" || t == "s"), "{got:?}");
    }

    #[test]
    fn tokens_are_sorted_and_disjoint_on_any_input() {
        for src in [
            &b"fn f() { \"unterminated"[..],
            b"\xff\xfe garbage {{{{",
            b"",
            b"/* a /* nested */ comment */ fn g() {}",
        ] {
            let h = highlight(src);
            assert!(h.windows(2).all(|w| w[0].end <= w[1].start), "{h:?}");
            assert!(h.iter().all(|t| t.end as usize <= src.len()));
        }
    }
}
