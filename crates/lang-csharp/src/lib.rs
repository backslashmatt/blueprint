//! The C# analyser (REQUIREMENTS §3.4): L3 types and L4 members, read from
//! one file at a time with tree-sitter-c-sharp.
//!
//! What the model sees:
//! - **Types (L3):** `class`, `struct`, `interface`, `enum`, `record` and
//!   `delegate`; a `partial` one's keyword says so (`partial class`), for
//!   the engine to join its parts. A nested type is named by its path in its outer types
//!   (`Outer.Inner`). A type's base list, fields and their types are its
//!   signature; its function-like members and field initial values are its
//!   body (as for TypeScript classes and Rust traits).
//! - **Members (L4):** methods, constructors, destructors, operators,
//!   properties and indexers, as [`ItemKind::Method`] of their type. A
//!   property's accessors are its body. Local functions are not items.
//! - **Namespaces:** a block or file-scoped `namespace A.B` gives its items
//!   [`Item::module`] `["A", "B"]`; the engine places items by it.
//! - **Visibility:** `public` is public; `internal`, `protected` and their
//!   combinations are restricted; `private` is private. Without a modifier a
//!   top-level type is `internal` (restricted), a member `private`, and an
//!   interface's member `public`.
//! - **References:** `using A.B;` is a glob `["A", "B", "*"]` (it brings in
//!   the namespace's types), as is `using static A.T;`; `using X = A.B;` is
//!   `["A", "B"]` aliased `X`. Each base in a base list is a
//!   [`Reference::Impl`]: C# writes base classes and interfaces alike, so
//!   which it is is left to the engine, from the target's kind.
//! - **Calls:** `F()`, `T.F()`, `A.B.F()` as written, generic arguments
//!   dropped; a call through a name bound in the member (a parameter, a
//!   local, a lambda parameter) is skipped, as are `this.` and `base.`.

pub mod highlight;

use std::collections::HashSet;

use blueprint_model::lang::{FileFacts, Highlight, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};
use tree_sitter::{Node, Parser};

/// Namespaces and types nested deeper than this are not read, so hostile
/// input cannot make the work list grow without bound.
const MAX_DEPTH: usize = 64;

/// The C# analyser.
#[derive(Debug, Default, Clone, Copy)]
pub struct CSharp;

impl LanguageAnalyser for CSharp {
    fn name(&self) -> &'static str {
        "csharp"
    }

    fn version(&self) -> u32 {
        3
    }

    fn handles(&self, path: &RepoPath) -> bool {
        path.segments().last().is_some_and(|f| f.ends_with(".cs"))
    }

    fn analyse(&self, _path: &RepoPath, bytes: &[u8]) -> FileFacts {
        analyse(bytes)
    }

    fn highlight(&self, bytes: &[u8]) -> Vec<Highlight> {
        highlight::highlight(bytes)
    }
}

/// What a declaration list belongs to.
#[derive(Clone)]
struct Scope {
    /// The namespace, as segments.
    namespace: Vec<String>,
    /// The enclosing types, outermost first; empty in a namespace.
    types: Vec<String>,
    /// Whether the enclosing type is an interface (members default public).
    interface: bool,
    /// The enclosing type's primary-constructor parameters, fields and
    /// properties, which its members can name (`class C(ILogger log)`).
    params: Vec<String>,
}

struct Out<'s> {
    src: &'s [u8],
    items: Vec<(usize, Item)>,
    references: Vec<(usize, Reference)>,
}

/// The facts of one C# file.
#[must_use]
pub fn analyse(src: &[u8]) -> FileFacts {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
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
    };
    let top = Scope {
        namespace: Vec::new(),
        types: Vec::new(),
        interface: false,
        params: Vec::new(),
    };
    let mut work = vec![(root, top)];
    while let Some((list, mut scope)) = work.pop() {
        for node in named_children(list) {
            match node.kind() {
                // Applies to every declaration after it in the file.
                "file_scoped_namespace_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        scope.namespace = [scope.namespace.clone(), segments(src, name)].concat();
                    }
                }
                _ => out.declaration(node, &scope, &mut work),
            }
        }
    }
    out.items.sort_by_key(|(start, _)| *start);
    out.references.sort_by_key(|(start, _)| *start);
    FileFacts {
        items: out.items.into_iter().map(|(_, i)| i).collect(),
        references: out.references.into_iter().map(|(_, r)| r).collect(),
        clean: !root.has_error(),
    }
}

/// The type keyword of a type declaration's kind.
fn type_keyword(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "class_declaration" => "class",
        "struct_declaration" => "struct",
        "interface_declaration" => "interface",
        "enum_declaration" => "enum",
        "record_declaration" | "record_struct_declaration" => "record",
        "delegate_declaration" => "delegate",
        _ => return None,
    })
}

/// Whether a type declaration has the `partial` modifier.
fn is_partial(src: &[u8], node: Node<'_>) -> bool {
    named_children(node)
        .into_iter()
        .any(|c| c.kind() == "modifier" && text(src, c) == "partial")
}

/// Whether a member is function-like: an L4 item with a body of its own.
fn is_member_function(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration"
            | "property_declaration"
            | "indexer_declaration"
    )
}

impl Out<'_> {
    fn declaration<'t>(
        &mut self,
        node: Node<'t>,
        scope: &Scope,
        work: &mut Vec<(Node<'t>, Scope)>,
    ) {
        let src = self.src;
        let depth = scope.namespace.len() + scope.types.len();
        match node.kind() {
            "using_directive" => self.using(node, scope),
            "namespace_declaration" if depth < MAX_DEPTH => {
                let (Some(name), Some(body)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("body"),
                ) else {
                    return;
                };
                let inner = Scope {
                    namespace: [scope.namespace.clone(), segments(src, name)].concat(),
                    types: Vec::new(),
                    interface: false,
                    params: Vec::new(),
                };
                work.push((body, inner));
            }
            kind if depth < MAX_DEPTH && type_keyword(kind).is_some() => {
                self.type_declaration(node, scope, work);
            }
            kind if is_member_function(kind) && !scope.types.is_empty() => {
                self.member(node, scope);
            }
            // Parser recovery: what it could read is inside. A type that
            // never closed leaves its name here; its members stay its own.
            "ERROR" if depth < MAX_DEPTH => {
                let mut inner = scope.clone();
                // The grammar keeps the unclosed type's `name` field.
                let opens_type = named_children(node)
                    .iter()
                    .any(|c| is_member_function(c.kind()));
                let name = field_text(src, node, "name").or_else(|| {
                    named_children(node)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                        .map(|c| text(src, c))
                });
                if let (true, Some(name)) = (opens_type, name) {
                    inner.types.push(name);
                    inner.params.clear();
                }
                work.push((node, inner));
            }
            _ => {}
        }
    }

    fn using(&mut self, node: Node<'_>, scope: &Scope) {
        let src = self.src;
        let alias = node.child_by_field_name("name").map(|n| text(src, n));
        // The namespace or type named: the last named child that is a name.
        let name = node.child_by_field_name("name");
        let target = named_children(node).into_iter().rev().find(|c| {
            matches!(
                c.kind(),
                "identifier" | "qualified_name" | "generic_name" | "alias_qualified_name"
            ) && name.is_none_or(|n| n.id() != c.id())
        });
        let Some(target) = target else { return };
        let mut path = segments(src, target);
        if path.is_empty() {
            return;
        }
        if alias.is_none() {
            path.push("*".into());
        }
        let r = Reference::Use {
            path,
            alias,
            module: scope.namespace.clone(),
        };
        self.references.push((node.start_byte(), r));
    }

    fn type_declaration<'t>(
        &mut self,
        node: Node<'t>,
        scope: &Scope,
        work: &mut Vec<(Node<'t>, Scope)>,
    ) {
        let src = self.src;
        let Some(keyword) = type_keyword(node.kind()) else {
            return;
        };
        let Some(name) = field_text(src, node, "name") else {
            return;
        };
        let full = [scope.types.clone(), vec![name]].concat().join(".");
        // Bases: each a type that this one extends or implements.
        for base in named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "base_list")
        {
            for b in named_children(base) {
                let path = segments(src, b);
                if !path.is_empty() {
                    let r = Reference::Impl {
                        self_type: vec![full.clone()],
                        r#trait: path,
                        module: scope.namespace.clone(),
                        extends: false,
                    };
                    self.references.push((b.start_byte(), r));
                }
            }
        }
        // Body: function-like members, and fields' initial values.
        let members = node
            .child_by_field_name("body")
            .map(named_children)
            .unwrap_or_default();
        let mut body: Vec<Node<'_>> = Vec::new();
        for m in &members {
            if is_member_function(m.kind()) || type_keyword(m.kind()).is_some() {
                body.push(*m);
            } else if m.kind() == "field_declaration" {
                preorder(*m, |n| {
                    if n.kind() == "variable_declarator" {
                        // `x = 1`: the value is the named child after the name.
                        let name = n.child_by_field_name("name");
                        body.extend(
                            named_children(n)
                                .into_iter()
                                .filter(|c| name.is_none_or(|nm| nm.id() != c.id())),
                        );
                        return false;
                    }
                    true
                });
            }
        }
        let name_node = node.child_by_field_name("name");
        let mut skip: Vec<Node<'_>> = name_node.into_iter().collect();
        skip.extend(&body);
        let has_body = !body.is_empty();
        let item = Item {
            kind: ItemKind::Type {
                // `partial class`: its parts in other files are one type
                // (DR 0013).
                keyword: if is_partial(src, node) {
                    format!("partial {keyword}")
                } else {
                    keyword.into()
                },
            },
            name: full.clone(),
            module: scope.namespace.clone(),
            lines: (
                line(node.start_position().row),
                line(node.end_position().row),
            ),
            body_line: has_body
                .then(|| {
                    node.child_by_field_name("body")
                        .map(|b| line(b.start_position().row))
                })
                .flatten(),
            visibility: Some(visibility(src, node, scope)),
            signature_hash: hash(src, &[node], &skip),
            body_hash: has_body.then(|| hash(src, &body, &[])),
            calls: Vec::new(),
        };
        self.items.push((node.start_byte(), item));
        if let Some(b) = node.child_by_field_name("body")
            && b.kind() == "declaration_list"
        {
            let inner = Scope {
                namespace: scope.namespace.clone(),
                types: [
                    scope.types.clone(),
                    vec![field_text(src, node, "name").unwrap_or_default()],
                ]
                .concat(),
                interface: keyword == "interface",
                params: {
                    let mut names = HashSet::new();
                    for p in named_children(node)
                        .into_iter()
                        .filter(|c| c.kind() == "parameter_list")
                    {
                        bind(src, p, &mut names);
                    }
                    // Its fields and properties: `Store.Get()` through a
                    // field calls an instance, not a type named `Store`.
                    for m in &members {
                        match m.kind() {
                            "field_declaration" | "event_field_declaration" => {
                                preorder(*m, |n| {
                                    if n.kind() == "variable_declarator" {
                                        names.extend(field_text(src, n, "name"));
                                        return false;
                                    }
                                    true
                                });
                            }
                            "property_declaration" => names.extend(field_text(src, *m, "name")),
                            _ => {}
                        }
                    }
                    names.into_iter().collect()
                },
            };
            work.push((b, inner));
        }
    }

    fn member(&mut self, node: Node<'_>, scope: &Scope) {
        let src = self.src;
        let self_type = scope.types.join(".");
        let name = match node.kind() {
            "indexer_declaration" => "this[]".to_owned(),
            "destructor_declaration" => {
                format!("~{}", scope.types.last().cloned().unwrap_or_default())
            }
            "operator_declaration" => {
                let op = node
                    .child_by_field_name("operator")
                    .map(|o| text(src, o))
                    .unwrap_or_default();
                format!("operator {op}")
            }
            "conversion_operator_declaration" => {
                let ty = node
                    .child_by_field_name("type")
                    .map(|t| text(src, t))
                    .unwrap_or_default();
                format!("operator {ty}")
            }
            _ => match field_text(src, node, "name") {
                Some(n) => n,
                None => return,
            },
        };
        // The body: a block, an arrow expression, a property's accessors and
        // its initial value.
        let body: Vec<Node<'_>> = ["body", "accessors", "value"]
            .into_iter()
            .filter_map(|f| node.child_by_field_name(f))
            .chain(
                named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == "arrow_expression_clause"),
            )
            .fold(Vec::new(), |mut v, n| {
                if !v.iter().any(|x: &Node<'_>| x.id() == n.id()) {
                    v.push(n);
                }
                v
            });
        let mut skip: Vec<Node<'_>> = body.clone();
        skip.extend(node.child_by_field_name("name"));
        // `void IFoo.Bar()`: an explicit implementation of IFoo's member.
        let explicit = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "explicit_interface_specifier")
            .map(|e| segments(src, e).join("."))
            .filter(|t| !t.is_empty());
        let first = body.iter().map(Node::start_byte).min();
        let item = Item {
            kind: ItemKind::Method {
                self_type,
                r#trait: explicit,
            },
            name,
            module: scope.namespace.clone(),
            lines: (
                line(node.start_position().row),
                line(node.end_position().row),
            ),
            body_line: body
                .iter()
                .find(|b| Some(b.start_byte()) == first)
                .map(|b| line(b.start_position().row)),
            visibility: Some(visibility(src, node, scope)),
            signature_hash: hash(src, &[node], &skip),
            body_hash: (!body.is_empty()).then(|| hash(src, &body, &[])),
            calls: if body.is_empty() {
                Vec::new()
            } else {
                calls(src, node, &body, scope)
            },
        };
        self.items.push((node.start_byte(), item));
    }
}

/// A declaration's visibility from its modifiers, else C#'s default for
/// where it is.
fn visibility(src: &[u8], node: Node<'_>, scope: &Scope) -> Visibility {
    let mods: Vec<String> = named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "modifier")
        .map(|c| text(src, c))
        .collect();
    let has = |m: &str| mods.iter().any(|x| x == m);
    if has("file") {
        // Visible only in its own file.
        Visibility::Private
    } else if has("public") {
        Visibility::Public
    } else if has("internal") || has("protected") {
        Visibility::Restricted
    } else if has("private") {
        Visibility::Private
    } else if scope.interface {
        Visibility::Public
    } else if scope.types.is_empty() {
        Visibility::Restricted
    } else {
        Visibility::Private
    }
}

/// The paths a member's body calls, in source order, without repeats.
fn calls(src: &[u8], member: Node<'_>, body: &[Node<'_>], scope: &Scope) -> Vec<Vec<String>> {
    let mut locals: HashSet<String> = scope.params.iter().cloned().collect();
    // A setter's (or `init`'s, `add`'s, `remove`'s) implicit parameter.
    if matches!(
        member.kind(),
        "property_declaration" | "indexer_declaration"
    ) {
        locals.insert("value".into());
    }
    if let Some(params) = member.child_by_field_name("parameters") {
        bind(src, params, &mut locals);
    }
    for b in body {
        preorder(*b, |n| {
            match n.kind() {
                "variable_declarator" | "parameter" | "catch_declaration" => {
                    match field_text(src, n, "name") {
                        Some(name) => {
                            locals.insert(name);
                        }
                        // `var (a, b) = …`
                        None => bind(src, n, &mut locals),
                    }
                }
                "foreach_statement" => {
                    if let Some(left) = n.child_by_field_name("left") {
                        match left.kind() {
                            "identifier" => {
                                locals.insert(text(src, left));
                            }
                            _ => bind(src, left, &mut locals),
                        }
                    }
                }
                // `is var x`, `{ P: var x }`, `[var x, ..]`, `out var x`.
                "single_variable_designation" => {
                    locals.insert(text(src, n));
                }
                // Query variables: `from x in …`, `let y = …`, `join z in …`.
                "from_clause" | "let_clause" | "join_clause" | "query_continuation" => {
                    // `let z = …` has no name field: its first identifier.
                    locals.extend(field_text(src, n, "name").or_else(|| {
                        named_children(n)
                            .into_iter()
                            .find(|c| c.kind() == "identifier")
                            .map(|c| text(src, c))
                    }));
                }
                "local_function_statement" => {
                    locals.extend(field_text(src, n, "name"));
                }
                "declaration_expression" | "declaration_pattern" | "recursive_pattern" => {
                    bind(src, n, &mut locals);
                }
                "lambda_expression" => {
                    if let Some(p) = n.child_by_field_name("parameters") {
                        bind(src, p, &mut locals);
                    }
                }
                _ => {}
            }
            true
        });
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    for b in body {
        preorder(*b, |n| {
            if n.kind() == "invocation_expression"
                && let Some(f) = n.child_by_field_name("function")
                && let Some(path) = call_path(src, f)
                // `nameof(x)` names, it calls nothing.
                && path != ["nameof"]
                && path.first().is_some_and(|first| !locals.contains(first))
                && seen.insert(path.clone())
            {
                out.push(path);
            }
            true
        });
    }
    out
}

/// `F`, `T.F`, `A.B.F` (generic arguments dropped) as segments; `None` for
/// anything else (`x[0]()`, `this.F`, `base.F`, `F()()`).
fn call_path(src: &[u8], f: Node<'_>) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = f;
    loop {
        match cur.kind() {
            "identifier" => {
                out.push(text(src, cur));
                break;
            }
            "generic_name" => {
                out.push(text(src, cur.named_child(0)?));
                break;
            }
            "member_access_expression" => {
                let name = cur.child_by_field_name("name")?;
                let n = match name.kind() {
                    "generic_name" => name.named_child(0)?,
                    _ => name,
                };
                out.push(text(src, n));
                cur = cur.child_by_field_name("expression")?;
            }
            _ => return None,
        }
        if out.len() > MAX_DEPTH {
            return None;
        }
    }
    out.reverse();
    Some(out)
}

/// Adds every name a parameter list or pattern binds to `names`.
fn bind(src: &[u8], node: Node<'_>, names: &mut HashSet<String>) {
    preorder(node, |n| {
        match n.kind() {
            "parameter"
            | "variable_declarator"
            | "declaration_expression"
            | "declaration_pattern" => {
                names.extend(field_text(src, n, "name"));
            }
            "single_variable_designation" | "implicit_parameter" => {
                names.insert(text(src, n));
            }
            // `var (a, b)`, `(var a, var b)`, `(a, b)` on the left of a
            // deconstruction or a `foreach`.
            "tuple_pattern" | "parenthesized_variable_designation" | "tuple_expression" => {
                for c in named_children(n) {
                    if c.kind() == "identifier" {
                        names.insert(text(src, c));
                    }
                }
            }
            "argument" if n.named_child(0).is_some_and(|c| c.kind() == "identifier") => {
                names.extend(n.named_child(0).map(|c| text(src, c)));
            }
            _ => {}
        }
        !matches!(n.kind(), "block" | "argument_list")
    });
}

/// The names in a dotted name: `A.B.C` gives `["A", "B", "C"]`, `List<T>`
/// gives `["List"]`, `global::A` gives `["A"]`.
fn segments(src: &[u8], node: Node<'_>) -> Vec<String> {
    let mut out = Vec::new();
    preorder(node, |n| match n.kind() {
        // `List<T>`; a record's `Base(X)` base arguments.
        "type_argument_list" | "argument_list" => false,
        "identifier" => {
            let t = text(src, n);
            if t != "global" || !out.is_empty() {
                out.push(t);
            }
            false
        }
        _ => true,
    });
    out
}

/// Visits `root` and its descendants in source order through a tree
/// cursor, without recursion. `visit` returns whether to descend.
fn preorder<'t>(root: Node<'t>, mut visit: impl FnMut(Node<'t>) -> bool) {
    let mut cursor = root.walk();
    loop {
        if visit(cursor.node()) && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.node().id() == root.id() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
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

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0100_0000_01b3;

/// FNV-1a over the leaf tokens of `roots`, in order, skipping comments and
/// the subtrees in `skip`, with a separator after each token.
fn hash(src: &[u8], roots: &[Node<'_>], skip: &[Node<'_>]) -> u64 {
    let skip: HashSet<usize> = skip.iter().map(Node::id).collect();
    let mut h = FNV_OFFSET;
    for root in roots {
        preorder(*root, |n| {
            // Comments and `#region`-style directives change nothing.
            if n.kind() == "comment" || n.kind().starts_with("preproc") || skip.contains(&n.id()) {
                return false;
            }
            if n.child_count() == 0 {
                for b in src[n.byte_range()].iter().chain(std::iter::once(&0x1f)) {
                    h = (h ^ u64::from(*b)).wrapping_mul(FNV_PRIME);
                }
            }
            true
        });
    }
    h
}
