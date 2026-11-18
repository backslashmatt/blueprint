//! The Rust analyser (REQUIREMENTS §3.4, DR 0008): L3 types and L4
//! functions and methods, read from one file at a time with tree-sitter-rust.
//!
//! tree-sitter recovers from syntax errors, so a damaged file still yields
//! the items that could be read, with [`FileFacts::clean`] false. Items inside
//! macro invocations are not seen; neither are functions nested in function
//! bodies, which are not part of the architecture.

use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};
use tree_sitter::{Node, Parser};

/// Inline modules deeper than this are not read, so hostile input cannot
/// exhaust the stack. Matches the L2 lexer's limit.
const MAX_MODULE_DEPTH: usize = 64;

/// The Rust analyser.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rust;

impl LanguageAnalyser for Rust {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn version(&self) -> u32 {
        1
    }

    fn handles(&self, path: &RepoPath) -> bool {
        path.segments().last().is_some_and(|f| f.ends_with(".rs"))
    }

    fn analyse(&self, _path: &RepoPath, bytes: &[u8]) -> FileFacts {
        analyse(bytes)
    }
}

/// The facts of one Rust file.
#[must_use]
pub fn analyse(src: &[u8]) -> FileFacts {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return FileFacts::default();
    }
    let Some(tree) = parser.parse(src, None) else {
        return FileFacts::default();
    };
    let root = tree.root_node();
    let mut facts = FileFacts {
        clean: !root.has_error(),
        ..FileFacts::default()
    };
    // Explicit stack of (declaration list, inline module path): no recursion.
    let mut stack = vec![(root, Vec::<String>::new())];
    while let Some((list, module)) = stack.pop() {
        let mut nested = Vec::new();
        for item in named_children(list) {
            match item.kind() {
                "struct_item" | "enum_item" | "union_item" | "type_item" => {
                    push_type(&mut facts, src, item, &module, None);
                }
                "trait_item" => {
                    let body = item.child_by_field_name("body");
                    push_type(&mut facts, src, item, &module, body);
                    if let (Some(body), Some(name)) = (body, field_text(src, item, "name")) {
                        let vis = visibility(src, item);
                        for f in named_children(body) {
                            if matches!(f.kind(), "function_item" | "function_signature_item") {
                                let kind = ItemKind::Method {
                                    self_type: name.clone(),
                                    r#trait: Some(name.clone()),
                                };
                                push_function(&mut facts, src, f, &module, kind, vis);
                            }
                        }
                    }
                }
                "function_item" => {
                    let vis = visibility(src, item);
                    push_function(&mut facts, src, item, &module, ItemKind::Function, vis);
                }
                "impl_item" => push_impl(&mut facts, src, item, &module),
                "mod_item" => {
                    if let (Some(body), Some(name)) = (
                        item.child_by_field_name("body"),
                        field_text(src, item, "name"),
                    ) {
                        if module.len() < MAX_MODULE_DEPTH {
                            let mut inner = module.clone();
                            inner.push(name);
                            nested.push((body, inner));
                        } else {
                            facts.clean = false;
                        }
                    }
                }
                "use_declaration" => {
                    if let Some(arg) = item.child_by_field_name("argument") {
                        for path in use_paths(src, arg) {
                            facts.references.push(Reference::Use {
                                path,
                                module: module.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        // Keep source order: modules are visited after the items before them,
        // so sort once at the end instead.
        stack.extend(nested.into_iter().rev());
    }
    facts.items.sort_by_key(|i| (i.lines.0, i.lines.1));
    facts
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn text(src: &[u8], node: Node<'_>) -> String {
    String::from_utf8_lossy(&src[node.byte_range()]).into_owned()
}

fn field_text(src: &[u8], node: Node<'_>, field: &str) -> Option<String> {
    node.child_by_field_name(field).map(|n| text(src, n))
}

/// Line range, 1-based and inclusive.
fn lines(node: Node<'_>) -> (u32, u32) {
    let row = |r: usize| u32::try_from(r + 1).unwrap_or(u32::MAX);
    (row(node.start_position().row), row(node.end_position().row))
}

fn visibility(src: &[u8], item: Node<'_>) -> Option<Visibility> {
    let vis = named_children(item)
        .into_iter()
        .find(|c| c.kind() == "visibility_modifier");
    Some(match vis.map(|v| text(src, v)) {
        None => Visibility::Private,
        Some(v) if v == "pub" => Visibility::Public,
        Some(_) => Visibility::Restricted,
    })
}

fn push_type(
    facts: &mut FileFacts,
    src: &[u8],
    item: Node<'_>,
    module: &[String],
    body: Option<Node<'_>>,
) {
    let Some(name) = field_text(src, item, "name") else {
        return;
    };
    let keyword = match item.kind() {
        "struct_item" => "struct",
        "enum_item" => "enum",
        "union_item" => "union",
        "trait_item" => "trait",
        _ => "type",
    };
    let name_node = item.child_by_field_name("name");
    // A type's fields and variants are its surface, so everything but a
    // trait's method list counts as signature.
    let signature_hash = token_hash(src, item, &[name_node, body]);
    let body_hash = body.map_or(0, |b| token_hash(src, b, &[]));
    facts.items.push(Item {
        kind: ItemKind::Type {
            keyword: keyword.into(),
        },
        name,
        module: module.to_vec(),
        lines: lines(item),
        body_line: body.map(|b| lines(b).0),
        visibility: visibility(src, item),
        signature_hash,
        body_hash,
    });
}

fn push_function(
    facts: &mut FileFacts,
    src: &[u8],
    item: Node<'_>,
    module: &[String],
    kind: ItemKind,
    visibility: Option<Visibility>,
) {
    let Some(name) = field_text(src, item, "name") else {
        return;
    };
    let body = item.child_by_field_name("body");
    let signature_hash = token_hash(src, item, &[item.child_by_field_name("name"), body]);
    let body_hash = body.map_or(0, |b| token_hash(src, b, &[]));
    facts.items.push(Item {
        kind,
        name,
        module: module.to_vec(),
        lines: lines(item),
        body_line: body.map(|b| lines(b).0),
        visibility,
        signature_hash,
        body_hash,
    });
}

fn push_impl(facts: &mut FileFacts, src: &[u8], item: Node<'_>, module: &[String]) {
    let (Some(ty), Some(body)) = (
        item.child_by_field_name("type"),
        item.child_by_field_name("body"),
    ) else {
        return;
    };
    let self_type = type_name(src, ty);
    let r#trait = item.child_by_field_name("trait").map(|t| type_name(src, t));
    for f in named_children(body) {
        if f.kind() == "function_item" {
            // A trait impl's methods take their visibility from the trait.
            let vis = if r#trait.is_some() {
                None
            } else {
                visibility(src, f)
            };
            let kind = ItemKind::Method {
                self_type: self_type.clone(),
                r#trait: r#trait.clone(),
            };
            push_function(facts, src, f, module, kind, vis);
        }
    }
}

/// The type's own name: `Foo` for `Foo<T>`, `a::b::Foo` or `&Foo`.
fn type_name(src: &[u8], ty: Node<'_>) -> String {
    let mut node = ty;
    loop {
        node = match node.kind() {
            "generic_type" => match node.child_by_field_name("type") {
                Some(n) => n,
                None => break,
            },
            "scoped_type_identifier" => match node.child_by_field_name("name") {
                Some(n) => n,
                None => break,
            },
            "reference_type" | "pointer_type" => match node.child_by_field_name("type") {
                Some(n) => n,
                None => break,
            },
            _ => break,
        };
    }
    text(src, node)
}

/// FNV-1a over the leaf tokens of `node`, skipping comments and the subtrees
/// in `skip`, with a separator between tokens so `ab c` and `a bc` differ.
fn token_hash(src: &[u8], node: Node<'_>, skip: &[Option<Node<'_>>]) -> u64 {
    let skipped = |n: Node<'_>| skip.iter().flatten().any(|s| s.id() == n.id());
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if skipped(n) || n.kind().ends_with("comment") {
            continue;
        }
        if n.child_count() == 0 {
            for b in src[n.byte_range()].iter().chain(std::iter::once(&0x1f)) {
                h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
            }
        } else {
            let mut cursor = n.walk();
            let children: Vec<Node<'_>> = n.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
    }
    h
}

/// Every path a `use` argument names, flattened: `a::{b, c::d}` gives
/// `a::b` and `a::c::d`. Aliases keep the original path; globs end in `*`.
fn use_paths(src: &[u8], arg: Node<'_>) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![(arg, Vec::<String>::new())];
    while let Some((node, prefix)) = stack.pop() {
        if prefix.len() > MAX_MODULE_DEPTH {
            continue;
        }
        match node.kind() {
            "scoped_use_list" => {
                let mut p = prefix.clone();
                if let Some(path) = node.child_by_field_name("path") {
                    p.extend(path_segments(src, path));
                }
                if let Some(list) = node.child_by_field_name("list") {
                    for c in named_children(list).into_iter().rev() {
                        stack.push((c, p.clone()));
                    }
                }
            }
            "use_list" => {
                for c in named_children(node).into_iter().rev() {
                    stack.push((c, prefix.clone()));
                }
            }
            "use_as_clause" => {
                if let Some(path) = node.child_by_field_name("path") {
                    stack.push((path, prefix));
                }
            }
            "use_wildcard" => {
                let mut p = prefix;
                if let Some(path) = named_children(node).first() {
                    p.extend(path_segments(src, *path));
                }
                p.push("*".into());
                out.push(p);
            }
            _ => {
                let mut p = prefix;
                p.extend(path_segments(src, node));
                if !p.is_empty() {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// `a::b::c` as `["a", "b", "c"]`.
fn path_segments(src: &[u8], node: Node<'_>) -> Vec<String> {
    text(src, node)
        .split("::")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}
