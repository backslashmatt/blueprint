//! Token classes for colouring TypeScript and JavaScript in the code view,
//! from the same tree-sitter grammar the analyser uses.

use blueprint_model::lang::{Highlight, TokenClass};
use tree_sitter::{Language, Node, Parser};

/// The coloured tokens of `src` read with `language`, sorted by start and
/// not overlapping. Never panics: unparsable bytes give fewer tokens.
#[must_use]
pub fn highlight(language: &Language, src: &[u8]) -> Vec<Highlight> {
    let mut parser = Parser::new();
    if parser.set_language(language).is_err() {
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
        "comment" => Some(TokenClass::Comment),
        "string" | "template_string" | "regex" => Some(TokenClass::String),
        "number" | "true" | "false" => Some(TokenClass::Number),
        // `number`, `string`…: a keyword-like leaf inside.
        "predefined_type" => Some(TokenClass::Type),
        _ => None,
    }
}

/// Leaves coloured by their kind and place.
fn class_of_leaf(node: Node<'_>) -> Option<TokenClass> {
    let kind = node.kind();
    if !node.is_named() {
        // Anonymous leaves are punctuation and keywords; keywords are words.
        return (!kind.is_empty() && kind.bytes().all(|b| b.is_ascii_lowercase()))
            .then_some(TokenClass::Keyword);
    }
    match kind {
        "type_identifier" => Some(TokenClass::Type),
        "this" | "super" | "null" | "undefined" => Some(TokenClass::Keyword),
        "identifier" | "property_identifier" => {
            let parent = node.parent()?;
            let named = |of: Node<'_>, field: &str| {
                of.child_by_field_name(field)
                    .is_some_and(|c| c.id() == node.id())
            };
            match parent.kind() {
                "function_declaration"
                | "generator_function_declaration"
                | "function_signature"
                | "method_definition"
                | "method_signature"
                | "abstract_method_signature"
                    if named(parent, "name") =>
                {
                    Some(TokenClass::Function)
                }
                "call_expression" if named(parent, "function") => Some(TokenClass::Function),
                "member_expression" if named(parent, "property") => {
                    let grand = parent.parent()?;
                    (grand.kind() == "call_expression" && named_is(grand, "function", parent))
                        .then_some(TokenClass::Function)
                }
                // `class Foo` in JavaScript names the class with an identifier.
                "class_declaration" | "class" if named(parent, "name") => Some(TokenClass::Type),
                _ => None,
            }
        }
        _ => None,
    }
}

fn named_is(parent: Node<'_>, field: &str, child: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|c| c.id() == child.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(src: &str) -> Vec<(String, TokenClass)> {
        let ts = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        highlight(&ts, src.as_bytes())
            .into_iter()
            .map(|h| (src[h.start as usize..h.end as usize].to_owned(), h.class))
            .collect()
    }

    #[test]
    fn colours_keywords_types_functions_literals_and_comments() {
        let got = classes(
            "// note\nexport function add(a: number, b: Point): number {\n  const s = `x${a}`;\n  helper(1);\n  return ns.call(true, 'y');\n}\n",
        );
        let has = |text: &str, class| got.iter().any(|(t, c)| t == text && *c == class);
        assert!(has("// note", TokenClass::Comment), "{got:?}");
        assert!(has("export", TokenClass::Keyword));
        assert!(has("function", TokenClass::Keyword));
        assert!(has("const", TokenClass::Keyword));
        assert!(has("return", TokenClass::Keyword));
        assert!(has("add", TokenClass::Function));
        assert!(has("helper", TokenClass::Function));
        assert!(
            has("call", TokenClass::Function),
            "ns.call(...) colours call"
        );
        assert!(has("number", TokenClass::Type));
        assert!(has("Point", TokenClass::Type));
        assert!(has("`x${a}`", TokenClass::String));
        assert!(has("'y'", TokenClass::String));
        assert!(has("1", TokenClass::Number));
        assert!(has("true", TokenClass::Number));
        assert!(!got.iter().any(|(t, _)| t == "ns" || t == "s"), "{got:?}");
    }

    #[test]
    fn tokens_are_sorted_and_disjoint_on_any_input() {
        let tsx = tree_sitter_typescript::LANGUAGE_TSX.into();
        for src in [
            &b"function f() { \"unterminated"[..],
            b"\xff\xfe garbage {{{{",
            b"",
            b"const x = <div className=\"a\">{y}</div>; /* c */",
        ] {
            let h = highlight(&tsx, src);
            assert!(h.windows(2).all(|w| w[0].end <= w[1].start), "{h:?}");
            assert!(h.iter().all(|t| t.end as usize <= src.len()));
        }
    }
}
