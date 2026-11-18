//! The Rust analyser (REQUIREMENTS §3.4, DR 0008): L3 types and L4
//! functions and methods, read from one file at a time with tree-sitter-rust.
//!
//! tree-sitter recovers from syntax errors, so a damaged file still yields
//! the items that could be read, with [`FileFacts::clean`] false. Items inside
//! macro invocations are not seen; neither are functions nested in function
//! bodies, which are not part of the architecture.
//!
//! What counts as signature and what as body decides how the diff tells a
//! surface change from a contained one:
//! - an item's outer attributes (`#[derive]`, `#[repr]`, `#[cfg]`…) are
//!   signature, and its line span starts at the first of them;
//! - a type's fields and variants are signature; it has no body;
//! - a trait's associated types and consts are signature, its methods are
//!   its body (they are also L4 items of their own);
//! - a method's signature includes its `impl` header, so changing the impl's
//!   generics or `where` clause changes every method's surface.

use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};
use tree_sitter::{Node, Parser};

/// Inline modules, and `use` groups, deeper than this are not read, so
/// hostile input cannot exhaust the stack. Matches the L2 lexer's limit.
const MAX_DEPTH: usize = 64;

/// The Rust analyser.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rust;

impl LanguageAnalyser for Rust {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn version(&self) -> u32 {
        2
    }

    fn handles(&self, path: &RepoPath) -> bool {
        path.segments().last().is_some_and(|f| f.ends_with(".rs"))
    }

    fn analyse(&self, _path: &RepoPath, bytes: &[u8]) -> FileFacts {
        analyse(bytes)
    }
}

/// What a declaration list belongs to.
#[derive(Clone)]
enum Owner {
    /// A file or inline module.
    Module,
    /// `impl [Trait for] Type`, with a hash of its header.
    Impl {
        self_type: String,
        r#trait: Option<String>,
        header: u64,
    },
    /// A trait's own methods.
    Trait {
        name: String,
        visibility: Option<Visibility>,
    },
    /// An `extern` block.
    Extern,
}

struct Out<'s> {
    src: &'s [u8],
    /// Items and references keyed by start byte, sorted at the end.
    items: Vec<(usize, Item)>,
    references: Vec<(usize, Reference)>,
    clean: bool,
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
    let mut out = Out {
        src,
        items: Vec::new(),
        references: Vec::new(),
        clean: !root.has_error(),
    };
    // Explicit work list of (declaration list, inline module, owner): no
    // recursion, whatever the nesting.
    let mut work = vec![(root, Vec::<String>::new(), Owner::Module)];
    while let Some((list, module, owner)) = work.pop() {
        let mut attrs: Vec<Node<'_>> = Vec::new();
        for node in named_children(list) {
            match node.kind() {
                "attribute_item" => {
                    attrs.push(node);
                    continue;
                }
                k if k.ends_with("comment") => continue,
                _ => {}
            }
            out.declaration(node, &attrs, &module, &owner, &mut work);
            attrs.clear();
        }
    }
    out.items.sort_by_key(|(start, _)| *start);
    out.references.sort_by_key(|(start, _)| *start);
    FileFacts {
        items: out.items.into_iter().map(|(_, i)| i).collect(),
        references: out.references.into_iter().map(|(_, r)| r).collect(),
        clean: out.clean,
    }
}

impl<'s> Out<'s> {
    fn declaration<'t>(
        &mut self,
        node: Node<'t>,
        attrs: &[Node<'t>],
        module: &[String],
        owner: &Owner,
        work: &mut Vec<(Node<'t>, Vec<String>, Owner)>,
    ) {
        let src = self.src;
        match (node.kind(), owner) {
            ("struct_item" | "enum_item" | "union_item" | "type_item", Owner::Module) => {
                let keyword = match node.kind() {
                    "struct_item" => "struct",
                    "enum_item" => "enum",
                    "union_item" => "union",
                    _ => "type",
                };
                let kind = ItemKind::Type {
                    keyword: keyword.into(),
                };
                let vis = visibility(src, node);
                self.item(node, attrs, module, kind, vis, &[], None, 0);
            }
            ("trait_item", Owner::Module) => {
                let body = node.child_by_field_name("body");
                let methods: Vec<Node<'_>> = body
                    .map(named_children)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| is_function(c.kind()))
                    .collect();
                let vis = visibility(src, node);
                let body_hash = (!methods.is_empty()).then(|| hash(src, &methods, &[]));
                let kind = ItemKind::Type {
                    keyword: "trait".into(),
                };
                self.item(node, attrs, module, kind, vis, &methods, body_hash, 0);
                if let (Some(body), Some(name)) = (body, field_text(src, node, "name")) {
                    let owner = Owner::Trait {
                        name,
                        visibility: vis,
                    };
                    work.push((body, module.to_vec(), owner));
                }
            }
            ("impl_item", Owner::Module) => {
                let (Some(ty), Some(body)) = (
                    node.child_by_field_name("type"),
                    node.child_by_field_name("body"),
                ) else {
                    return;
                };
                let owner = Owner::Impl {
                    self_type: type_name(src, ty),
                    r#trait: node
                        .child_by_field_name("trait")
                        .map(|t| trait_name(src, t)),
                    header: hash(src, &[node], &[body]),
                };
                work.push((body, module.to_vec(), owner));
            }
            ("foreign_mod_item", Owner::Module) => {
                if let Some(body) = node.child_by_field_name("body") {
                    work.push((body, module.to_vec(), Owner::Extern));
                }
            }
            ("mod_item", Owner::Module) => {
                if let (Some(body), Some(name)) = (
                    node.child_by_field_name("body"),
                    field_text(src, node, "name"),
                ) {
                    if module.len() < MAX_DEPTH {
                        let mut inner = module.to_vec();
                        inner.push(name);
                        work.push((body, inner, Owner::Module));
                    } else {
                        self.clean = false;
                    }
                }
            }
            ("use_declaration", Owner::Module) => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    let (paths, complete) = use_paths(src, arg);
                    self.clean &= complete;
                    for path in paths {
                        let r = Reference::Use {
                            path,
                            module: module.to_vec(),
                        };
                        self.references.push((node.start_byte(), r));
                    }
                }
            }
            (k, _) if is_function(k) => {
                let (kind, vis, header) = match owner {
                    Owner::Module | Owner::Extern => (ItemKind::Function, visibility(src, node), 0),
                    Owner::Impl {
                        self_type,
                        r#trait,
                        header,
                    } => {
                        let kind = ItemKind::Method {
                            self_type: self_type.clone(),
                            r#trait: r#trait.clone(),
                        };
                        // A trait impl's methods take their visibility from
                        // the trait.
                        let vis = if r#trait.is_some() {
                            None
                        } else {
                            visibility(src, node)
                        };
                        (kind, vis, *header)
                    }
                    Owner::Trait { name, visibility } => {
                        let kind = ItemKind::Method {
                            self_type: name.clone(),
                            r#trait: Some(name.clone()),
                        };
                        (kind, *visibility, 0)
                    }
                };
                let body = node.child_by_field_name("body");
                let body_hash = body.map(|b| hash(src, &[b], &[]));
                let skip: Vec<Node<'_>> = body.into_iter().collect();
                self.item(node, attrs, module, kind, vis, &skip, body_hash, header);
            }
            _ => {}
        }
    }

    /// Records `node` as an item. Its signature is its tokens and those of
    /// `attrs`, without its name and without `not_signature`, mixed with
    /// `header`.
    #[allow(clippy::too_many_arguments)]
    fn item(
        &mut self,
        node: Node<'_>,
        attrs: &[Node<'_>],
        module: &[String],
        kind: ItemKind,
        visibility: Option<Visibility>,
        not_signature: &[Node<'_>],
        body_hash: Option<u64>,
        header: u64,
    ) {
        let src = self.src;
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let mut skip = vec![name_node];
        skip.extend_from_slice(not_signature);
        let roots: Vec<Node<'_>> = attrs.iter().copied().chain([node]).collect();
        let mut signature_hash = hash(src, &roots, &skip);
        if header != 0 {
            signature_hash = mix(signature_hash, header);
        }
        let first = attrs.first().copied().unwrap_or(node);
        let body_line = match &kind {
            ItemKind::Type { keyword } if keyword == "trait" => node
                .child_by_field_name("body")
                .map(|b| line(b.start_position().row)),
            ItemKind::Type { .. } => None,
            _ => node
                .child_by_field_name("body")
                .map(|b| line(b.start_position().row)),
        };
        let item = Item {
            kind,
            name: text(src, name_node),
            module: module.to_vec(),
            lines: (
                line(first.start_position().row),
                line(node.end_position().row),
            ),
            body_line,
            visibility,
            signature_hash,
            body_hash,
        };
        self.items.push((first.start_byte(), item));
    }
}

fn is_function(kind: &str) -> bool {
    matches!(kind, "function_item" | "function_signature_item")
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

/// A 0-based row as a 1-based line.
fn line(row: usize) -> u32 {
    u32::try_from(row + 1).unwrap_or(u32::MAX)
}

fn visibility(src: &[u8], item: Node<'_>) -> Option<Visibility> {
    let vis = named_children(item)
        .into_iter()
        .find(|c| c.kind() == "visibility_modifier");
    let words: Option<String> = vis.map(|v| {
        text(src, v)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    });
    Some(match words.as_deref() {
        None | Some("pub(self)") => Visibility::Private,
        Some("pub") => Visibility::Public,
        Some(_) => Visibility::Restricted,
    })
}

/// The type's own name: `Foo` for `Foo<T>`, `a::b::Foo` or `&Foo`. Any
/// other type (a tuple, `dyn Trait`, a slice) is spelt from its tokens, so
/// formatting and lifetime names never change it.
fn type_name(src: &[u8], ty: Node<'_>) -> String {
    let mut node = ty;
    loop {
        let next = match node.kind() {
            "generic_type" | "reference_type" | "pointer_type" => node.child_by_field_name("type"),
            "scoped_type_identifier" => node.child_by_field_name("name"),
            _ => None,
        };
        match next {
            Some(n) => node = n,
            None => return spelt(src, node),
        }
    }
}

/// A trait's name with its generic arguments, `From<u16>` for
/// `std::convert::From<u16>`, so the impls of one trait for different
/// arguments have different ids rather than order-dependent suffixes.
fn trait_name(src: &[u8], tr: Node<'_>) -> String {
    match (
        tr.kind(),
        tr.child_by_field_name("type"),
        tr.child_by_field_name("type_arguments"),
    ) {
        ("generic_type", Some(base), Some(args)) => {
            format!("{}{}", type_name(src, base), spelt(src, args))
        }
        _ => type_name(src, tr),
    }
}

/// `node`'s tokens without comments or lifetimes, joined with a space only
/// where two words would otherwise run together: `(&'a u8, Vec<T>)` gives
/// `(&u8,Vec<T>)`.
fn spelt(src: &[u8], node: Node<'_>) -> String {
    let mut out = String::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind().ends_with("comment") || n.kind() == "lifetime" {
            continue;
        }
        if n.child_count() == 0 {
            let t = text(src, n);
            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if word(out.chars().last()) && word(t.chars().next()) {
                out.push(' ');
            }
            out.push_str(&t);
        } else {
            let mut cursor = n.walk();
            let children: Vec<Node<'_>> = n.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
    }
    out
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// FNV-1a over the leaf tokens of `roots`, in order, skipping comments and
/// the subtrees in `skip`, with a separator after each token so `ab c` and
/// `a bc` differ.
fn hash(src: &[u8], roots: &[Node<'_>], skip: &[Node<'_>]) -> u64 {
    let mut h = FNV_OFFSET;
    let mut stack: Vec<Node<'_>> = roots.iter().rev().copied().collect();
    while let Some(n) = stack.pop() {
        if n.kind().ends_with("comment") || skip.iter().any(|s| s.id() == n.id()) {
            continue;
        }
        if n.child_count() == 0 {
            for b in src[n.byte_range()].iter().chain(std::iter::once(&0x1f)) {
                h = (h ^ u64::from(*b)).wrapping_mul(FNV_PRIME);
            }
        } else {
            let mut cursor = n.walk();
            let children: Vec<Node<'_>> = n.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
    }
    h
}

/// Combines two hashes, order-sensitively.
fn mix(a: u64, b: u64) -> u64 {
    b.to_le_bytes()
        .iter()
        .fold(a, |h, x| (h ^ u64::from(*x)).wrapping_mul(FNV_PRIME))
}

/// Every path a `use` argument names, flattened: `a::{b, c::d}` gives `a::b`
/// and `a::c::d`, and `a::{self}` gives `a`. Aliases keep the original path;
/// globs end in `*`. The flag is false if a group nested deeper than
/// [`MAX_DEPTH`] was cut off.
fn use_paths(src: &[u8], arg: Node<'_>) -> (Vec<Vec<String>>, bool) {
    let mut out = Vec::new();
    let mut complete = true;
    let mut stack = vec![(arg, Vec::<String>::new(), 0usize)];
    while let Some((node, prefix, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            complete = false;
            continue;
        }
        match node.kind() {
            "scoped_use_list" => {
                let mut p = prefix.clone();
                if let Some(path) = node.child_by_field_name("path") {
                    p.extend(segments(src, path));
                }
                if let Some(list) = node.child_by_field_name("list") {
                    for c in named_children(list).into_iter().rev() {
                        stack.push((c, p.clone(), depth + 1));
                    }
                }
            }
            "use_list" => {
                for c in named_children(node).into_iter().rev() {
                    stack.push((c, prefix.clone(), depth + 1));
                }
            }
            "use_as_clause" => {
                if let Some(path) = node.child_by_field_name("path") {
                    stack.push((path, prefix, depth + 1));
                }
            }
            "use_wildcard" => {
                let mut p = prefix;
                if let Some(path) = named_children(node).first() {
                    p.extend(segments(src, *path));
                }
                p.push("*".into());
                out.push(p);
            }
            k if k.ends_with("comment") => {}
            _ => {
                let mut p = prefix;
                let own = segments(src, node);
                // `a::{self}` names `a` itself.
                if own.as_slice() == ["self"] && !p.is_empty() {
                    out.push(p);
                    continue;
                }
                p.extend(own);
                if !p.is_empty() {
                    out.push(p);
                }
            }
        }
    }
    (out, complete)
}

/// The identifiers of a path, `a::b::c` as `["a", "b", "c"]`, read from its
/// tokens so comments inside it are ignored.
fn segments(src: &[u8], node: Node<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind().ends_with("comment") {
            continue;
        }
        if n.child_count() == 0 {
            let t = text(src, n);
            if t != "::" && !t.trim().is_empty() {
                out.push(t);
            }
        } else {
            let mut cursor = n.walk();
            let children: Vec<Node<'_>> = n.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
    }
    out
}
