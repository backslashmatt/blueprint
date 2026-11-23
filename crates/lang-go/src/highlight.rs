//! Token classes for colouring Go in the code view, from the same
//! tree-sitter grammar the analyser uses.

use blueprint_model::lang::{Highlight, TokenClass};
use tree_sitter::{Node, Parser};

/// The coloured tokens of `src`, sorted by start and not overlapping.
/// Never panics: unparsable bytes give fewer tokens.
#[must_use]
pub fn highlight(src: &[u8]) -> Vec<Highlight> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
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
        let class = whole.or_else(|| {
            (node.child_count() == 0)
                .then(|| class_of_leaf(node, &ancestors))
                .flatten()
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
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal" => {
            Some(TokenClass::String)
        }
        "int_literal" | "float_literal" | "imaginary_literal" | "true" | "false" => {
            Some(TokenClass::Number)
        }
        _ => None,
    }
}

/// Leaves coloured by their kind and place. Only the parent is looked at,
/// so each leaf costs the same however deep it is.
fn class_of_leaf(node: Node<'_>, ancestors: &[Node<'_>]) -> Option<TokenClass> {
    let kind = node.kind();
    if !node.is_named() {
        return (!kind.is_empty() && kind.bytes().all(|b| b.is_ascii_lowercase()))
            .then_some(TokenClass::Keyword);
    }
    match kind {
        "nil" | "iota" => return Some(TokenClass::Keyword),
        "type_identifier" => return Some(TokenClass::Type),
        "identifier" | "field_identifier" => {}
        _ => return None,
    }
    let (&parent, rest) = ancestors.split_last()?;
    let is = |of: Node<'_>, field: &str| {
        of.child_by_field_name(field)
            .is_some_and(|c| c.id() == node.id())
    };
    match parent.kind() {
        "function_declaration" | "method_declaration" | "method_elem" if is(parent, "name") => {
            Some(TokenClass::Function)
        }
        "call_expression" if is(parent, "function") => Some(TokenClass::Function),
        "selector_expression" if is(parent, "field") => {
            let grand = *rest.last()?;
            (grand.kind() == "call_expression"
                && grand
                    .child_by_field_name("function")
                    .is_some_and(|c| c.id() == parent.id()))
            .then_some(TokenClass::Function)
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
            "// note\npackage app\n\nfunc (t *T) Add(a int) error {\n\ts := \"x\\n\"\n\thelper(1.5, 'r', `raw`)\n\treturn fmt.Errorf(\"%d\", 2, true, nil)\n}\n",
        );
        let has = |text: &str, class| got.iter().any(|(t, c)| t == text && *c == class);
        assert!(has("// note", TokenClass::Comment), "{got:?}");
        assert!(has("package", TokenClass::Keyword));
        assert!(has("func", TokenClass::Keyword));
        assert!(has("return", TokenClass::Keyword));
        assert!(has("nil", TokenClass::Keyword));
        assert!(has("T", TokenClass::Type));
        assert!(has("int", TokenClass::Type));
        assert!(has("error", TokenClass::Type));
        assert!(has("Add", TokenClass::Function));
        assert!(has("helper", TokenClass::Function));
        assert!(
            has("Errorf", TokenClass::Function),
            "fmt.Errorf colours Errorf"
        );
        assert!(has("\"x\\n\"", TokenClass::String), "{got:?}");
        assert!(has("'r'", TokenClass::String));
        assert!(has("`raw`", TokenClass::String));
        assert!(has("1.5", TokenClass::Number));
        assert!(has("true", TokenClass::Number));
        assert!(
            !got.iter().any(|(t, _)| t == "s" || t == "a" || t == "fmt"),
            "{got:?}"
        );
    }

    #[test]
    fn tokens_are_sorted_and_disjoint_on_any_input() {
        for src in [
            &b"func f( {\n\t\"unterminated"[..],
            b"\xff\xfe garbage ((((",
            b"",
            b"var x = `a\nb` + \"c\\\"d\" // c",
        ] {
            let h = highlight(src);
            assert!(h.windows(2).all(|w| w[0].end <= w[1].start), "{h:?}");
            assert!(h.iter().all(|t| t.end as usize <= src.len()));
        }
    }
}
