//! Token classes for colouring Python in the code view, from the same
//! tree-sitter grammar the analyser uses.

use blueprint_model::lang::{Highlight, TokenClass};
use tree_sitter::{Node, Parser};

/// The coloured tokens of `src`, sorted by start and not overlapping.
/// Never panics: unparsable bytes give fewer tokens.
#[must_use]
pub fn highlight(src: &[u8]) -> Vec<Highlight> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
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
    // Running counts over `ancestors`, so no leaf scans them: how many are
    // decorators, argument lists inside a decorator, and annotations.
    let mut within: Vec<Within> = vec![Within::default()];
    loop {
        let node = cursor.node();
        let whole = class_of_whole(node);
        let leaf = node.child_count() == 0;
        let class = whole.or_else(|| {
            if leaf {
                class_of_leaf(
                    node,
                    &ancestors,
                    *within.last().unwrap_or(&Within::default()),
                )
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
            let mut w = *within.last().unwrap_or(&Within::default());
            match node.kind() {
                "decorator" => w.decorator += 1,
                "argument_list" if w.decorator > 0 => w.decorator_args += 1,
                "type" => w.annotation += 1,
                _ => {}
            }
            within.push(w);
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
            within.pop();
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
        // An f-string's holes are coloured with it: one string token.
        "string" => Some(TokenClass::String),
        "integer" | "float" | "true" | "false" => Some(TokenClass::Number),
        _ => None,
    }
}

/// How many of a node's ancestors are of each kind that colours it.
#[derive(Debug, Default, Clone, Copy)]
struct Within {
    decorator: u32,
    decorator_args: u32,
    annotation: u32,
}

/// Leaves coloured by their kind and place.
fn class_of_leaf(node: Node<'_>, ancestors: &[Node<'_>], within: Within) -> Option<TokenClass> {
    let kind = node.kind();
    if !node.is_named() {
        // `def`, `class`, `async`, `not`, `in`, …; `@` of a decorator.
        return (!kind.is_empty() && kind.bytes().all(|b| b.is_ascii_lowercase()))
            .then_some(TokenClass::Keyword);
    }
    if kind == "none" {
        return Some(TokenClass::Keyword);
    }
    if kind != "identifier" {
        return None;
    }
    let (&parent, rest) = ancestors.split_last()?;
    let is = |of: Node<'_>, field: &str| {
        of.child_by_field_name(field)
            .is_some_and(|c| c.id() == node.id())
    };
    // A decorator's name, however it is written (`@a.b(1)`).
    if within.decorator > 0 {
        return (within.decorator_args == 0).then_some(TokenClass::Function);
    }
    match parent.kind() {
        "function_definition" if is(parent, "name") => Some(TokenClass::Function),
        "class_definition" if is(parent, "name") => Some(TokenClass::Type),
        "call" if is(parent, "function") => Some(TokenClass::Function),
        "attribute" if is(parent, "attribute") => {
            let grand = *rest.last()?;
            (grand.kind() == "call"
                && grand
                    .child_by_field_name("function")
                    .is_some_and(|c| c.id() == parent.id()))
            .then_some(TokenClass::Function)
        }
        // A base class.
        "argument_list" if rest.last().is_some_and(|g| g.kind() == "class_definition") => {
            Some(TokenClass::Type)
        }
        // An annotation.
        _ if within.annotation > 0 => Some(TokenClass::Type),
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
            "# note\n@dataclass\nclass Point(Base):\n    async def add(self, a: int) -> None:\n        s = f\"x{a}\"\n        helper(1.5)\n        return util.go(True, None)\n",
        );
        let has = |text: &str, class| got.iter().any(|(t, c)| t == text && *c == class);
        assert!(has("# note", TokenClass::Comment), "{got:?}");
        assert!(has("class", TokenClass::Keyword));
        assert!(has("async", TokenClass::Keyword));
        assert!(has("def", TokenClass::Keyword));
        assert!(has("return", TokenClass::Keyword));
        assert!(has("None", TokenClass::Keyword), "{got:?}");
        assert!(has("dataclass", TokenClass::Function), "{got:?}");
        assert!(has("Point", TokenClass::Type));
        assert!(has("Base", TokenClass::Type));
        assert!(has("int", TokenClass::Type));
        assert!(has("add", TokenClass::Function));
        assert!(has("helper", TokenClass::Function));
        assert!(has("go", TokenClass::Function), "util.go(...) colours go");
        assert!(has("f\"x{a}\"", TokenClass::String), "{got:?}");
        assert!(has("1.5", TokenClass::Number));
        assert!(has("True", TokenClass::Number));
        assert!(
            !got.iter().any(|(t, _)| t == "s" || t == "a" || t == "util"),
            "{got:?}"
        );
    }

    #[test]
    fn tokens_are_sorted_and_disjoint_on_any_input() {
        for src in [
            &b"def f(:\n    \"unterminated"[..],
            b"\xff\xfe garbage ((((",
            b"",
            b"x = f'a{b!r:>{w}}c' '''t\n'''  # c",
        ] {
            let h = highlight(src);
            assert!(h.windows(2).all(|w| w[0].end <= w[1].start), "{h:?}");
            assert!(h.iter().all(|t| t.end as usize <= src.len()));
        }
    }
}
