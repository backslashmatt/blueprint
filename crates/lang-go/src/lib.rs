//! The Go analyser (REQUIREMENTS §3.4): L3 types and L4 functions and
//! methods, read from one file at a time with tree-sitter-go.
//!
//! What the model sees:
//! - **Types (L3):** each `type` spec, with keyword `struct`, `interface`,
//!   `type` (a defined type) or `alias` (`type A = B`). A type is all
//!   signature, fields, tags, embedded types and interface methods alike:
//!   it has no body.
//! - **Functions (L4):** a `func` is an [`ItemKind::Function`]; a method
//!   (`func (t *T[K]) M()`) is an [`ItemKind::Method`] of its receiver's base
//!   type (`T`). Type parameters, parameters, results and the receiver are
//!   the signature; the block is the body. Function literals are part of
//!   their function's body.
//! - **Packages:** every item's [`Item::module`] is empty: in Go the package
//!   is the directory, which the engine knows from the file's path.
//! - **Visibility:** an exported (upper-case) name is public; any other is
//!   restricted to its package.
//! - **References:** each import spec is a [`Reference::Use`] whose path is
//!   the import path as one segment (`["net/http"]`), with its name as the
//!   alias (`str "strings"` is `str`; dot and blank imports are `.` and
//!   `_`). An embedded struct field or interface element (`Base`,
//!   `*pkg.Other`) is a [`Reference::Impl`] with `extends` set.
//! - **Calls:** `f()`, `pkg.F()`, `x.y.M()` as written, type arguments
//!   dropped (`F[int]()` is `F`, but `handlers[0]()` is no call); a call
//!   through a name local where the call is, by Go's lexical scopes, is
//!   skipped: parameters, results and type parameters (the receiver's
//!   too) everywhere; `:=`, `var`, `const` and local types from their
//!   statement to the end of their block; a function literal's parameters
//!   in it. In a method, a call through its receiver is
//!   written `["self", …]`, whatever the receiver is called, wherever no
//!   inner binding shadows it.
//! - **Interfaces:** an element that is a predeclared type (`interface {
//!   int }`) is a type-set element, not embedding. Another single named
//!   type is taken as embedded: telling a type set from an embedded
//!   interface needs types.
//!
//! Hashes are over tokens, not text, so comments and formatting change
//! nothing, gofmt's included (`;` or a newline, a trailing comma); a string
//! literal is one token. A file without a final newline is read with one.

pub mod highlight;

use std::collections::{HashMap, HashSet};

use blueprint_model::lang::{FileFacts, Highlight, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};
use tree_sitter::{Node, Parser};

/// Go's predeclared types: in an interface they are type-set elements
/// (`interface { int }`), never embedded interfaces, except `error`, which
/// is an interface.
const PREDECLARED: [&str; 21] = [
    "any",
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
];

/// Selector chains longer than this are not read as call paths.
const MAX_DEPTH: usize = 64;

/// The Go analyser.
#[derive(Debug, Default, Clone, Copy)]
pub struct Go;

impl LanguageAnalyser for Go {
    fn name(&self) -> &'static str {
        "go"
    }

    fn version(&self) -> u32 {
        1
    }

    fn handles(&self, path: &RepoPath) -> bool {
        path.segments().last().is_some_and(|f| f.ends_with(".go"))
    }

    fn analyse(&self, _path: &RepoPath, bytes: &[u8]) -> FileFacts {
        analyse(bytes)
    }

    fn highlight(&self, bytes: &[u8]) -> Vec<Highlight> {
        highlight::highlight(bytes)
    }
}

struct Out<'s> {
    src: &'s [u8],
    items: Vec<(usize, Item)>,
    references: Vec<(usize, Reference)>,
}

/// The facts of one Go file.
#[must_use]
pub fn analyse(src: &[u8]) -> FileFacts {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .is_err()
    {
        return FileFacts::default();
    }
    // Without a final newline the grammar can report a valid file as
    // broken (`type I interface { M() }` at the end): parse it with one.
    let owned;
    let src = if src.last().is_some_and(|b| *b != b'\n') {
        owned = [src, b"\n"].concat();
        owned.as_slice()
    } else {
        src
    };
    let Some(tree) = parser.parse(src, None) else {
        return FileFacts::default();
    };
    let root = tree.root_node();
    let mut out = Out {
        src,
        items: Vec::new(),
        references: Vec::new(),
    };
    // Top-level declarations; parser recovery can wrap some in `ERROR`.
    let mut work = vec![root];
    while let Some(list) = work.pop() {
        for node in named_children(list) {
            match node.kind() {
                "import_declaration" => out.imports(node),
                "type_declaration" => {
                    for spec in named_children(node) {
                        if matches!(spec.kind(), "type_spec" | "type_alias") {
                            out.type_spec(spec);
                        }
                    }
                }
                "function_declaration" | "method_declaration" => out.function(node),
                "ERROR" if list.id() == root.id() => work.push(node),
                _ => {}
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

impl Out<'_> {
    fn imports(&mut self, decl: Node<'_>) {
        let src = self.src;
        let mut specs = Vec::new();
        preorder(decl, |n| {
            if n.kind() == "import_spec" {
                specs.push(n);
                return false;
            }
            true
        });
        for spec in specs {
            let Some(path) = spec
                .child_by_field_name("path")
                .map(|p| unquote(&text(src, p)))
                .filter(|p| !p.is_empty())
            else {
                continue;
            };
            let alias = spec.child_by_field_name("name").map(|n| text(src, n));
            let r = Reference::Use {
                path: vec![path],
                alias,
                module: Vec::new(),
            };
            self.references.push((spec.start_byte(), r));
        }
    }

    fn type_spec(&mut self, spec: Node<'_>) {
        let src = self.src;
        let Some(name_node) = spec.child_by_field_name("name") else {
            return;
        };
        let name = text(src, name_node);
        let ty = spec.child_by_field_name("type");
        let keyword = if spec.kind() == "type_alias" {
            "alias"
        } else {
            match ty.map(|t| t.kind()) {
                Some("struct_type") => "struct",
                Some("interface_type") => "interface",
                _ => "type",
            }
        };
        // Embedded fields and interface elements: what the type extends.
        if let Some(t) = ty {
            let mut embedded: Vec<Node<'_>> = Vec::new();
            match t.kind() {
                "struct_type" => preorder(t, |n| {
                    if n.kind() == "field_declaration" {
                        if n.child_by_field_name("name").is_none()
                            && let Some(ft) = n.child_by_field_name("type")
                        {
                            embedded.push(ft);
                        }
                        return false;
                    }
                    true
                }),
                "interface_type" => {
                    for c in named_children(t) {
                        if c.kind() == "type_elem" {
                            // `A | B` unions are constraints, not embedding.
                            let parts = named_children(c);
                            if let [one] = parts.as_slice()
                                && !(one.kind() == "type_identifier"
                                    && PREDECLARED.contains(&text(src, *one).as_str()))
                            {
                                embedded.push(*one);
                            }
                        }
                    }
                }
                _ => {}
            }
            for e in embedded {
                if let Some(path) = type_path(src, e) {
                    let r = Reference::Impl {
                        self_type: vec![name.clone()],
                        r#trait: path,
                        module: Vec::new(),
                        extends: true,
                    };
                    self.references.push((e.start_byte(), r));
                }
            }
        }
        let item = Item {
            kind: ItemKind::Type {
                keyword: keyword.into(),
            },
            visibility: Some(visibility(&name)),
            name,
            module: Vec::new(),
            lines: (
                line(spec.start_position().row),
                line(spec.end_position().row),
            ),
            body_line: None,
            // The name is not signature: a rename is a rename.
            signature_hash: hash(src, &[spec], &[name_node]),
            body_hash: None,
            calls: Vec::new(),
        };
        self.items.push((spec.start_byte(), item));
    }

    fn function(&mut self, node: Node<'_>) {
        let src = self.src;
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = text(src, name_node);
        let body = node.child_by_field_name("body");
        let (kind, receiver) = if node.kind() == "method_declaration" {
            let recv = node
                .child_by_field_name("receiver")
                .and_then(|r| named_children(r).into_iter().next());
            let self_type = recv
                .and_then(|p| p.child_by_field_name("type"))
                .and_then(|t| receiver_type(src, t));
            let Some(self_type) = self_type else {
                return;
            };
            let recv_name = recv
                .and_then(|p| p.child_by_field_name("name"))
                .map(|n| text(src, n))
                .filter(|n| n != "_");
            (
                ItemKind::Method {
                    self_type,
                    r#trait: None,
                },
                recv_name,
            )
        } else {
            (ItemKind::Function, None)
        };
        let mut skip: Vec<Node<'_>> = body.into_iter().collect();
        skip.push(name_node);
        let item = Item {
            kind,
            visibility: Some(visibility(&name)),
            name,
            module: Vec::new(),
            lines: (
                line(node.start_position().row),
                line(node.end_position().row),
            ),
            body_line: body.map(|b| line(b.start_position().row)),
            signature_hash: hash(src, &[node], &skip),
            body_hash: body.map(|b| hash(src, &[b], &[])),
            calls: body
                .map(|b| calls(src, node, b, receiver.as_deref()))
                .unwrap_or_default(),
        };
        self.items.push((node.start_byte(), item));
    }
}

/// A receiver's base type name: `T`, `*T`, `T[K]` and `*T[K]` are all `T`.
fn receiver_type(src: &[u8], t: Node<'_>) -> Option<String> {
    let mut cur = t;
    for _ in 0..MAX_DEPTH {
        match cur.kind() {
            "type_identifier" => return Some(text(src, cur)),
            "pointer_type" | "parenthesized_type" => cur = cur.named_child(0)?,
            "generic_type" => cur = cur.child_by_field_name("type")?,
            _ => return None,
        }
    }
    None
}

/// An embedded type as a path: `Base` is `["Base"]`, `*pkg.Other[T]` is
/// `["pkg", "Other"]`.
fn type_path(src: &[u8], t: Node<'_>) -> Option<Vec<String>> {
    let mut cur = t;
    for _ in 0..MAX_DEPTH {
        match cur.kind() {
            "type_identifier" => return Some(vec![text(src, cur)]),
            "qualified_type" => {
                let pkg = cur.child_by_field_name("package")?;
                let name = cur.child_by_field_name("name")?;
                return Some(vec![text(src, pkg), text(src, name)]);
            }
            "pointer_type" | "parenthesized_type" => cur = cur.named_child(0)?,
            "generic_type" => cur = cur.child_by_field_name("type")?,
            _ => return None,
        }
    }
    None
}

/// Go's export rule: a name starting with an upper-case letter is exported.
fn visibility(name: &str) -> Visibility {
    if name.chars().next().is_some_and(char::is_uppercase) {
        Visibility::Public
    } else {
        Visibility::Restricted
    }
}

/// Whether a node opens a lexical scope (Go spec: blocks).
fn opens_scope(kind: &str) -> bool {
    matches!(
        kind,
        "block"
            | "func_literal"
            | "if_statement"
            | "for_statement"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "select_statement"
            | "expression_case"
            | "type_case"
            | "default_case"
            | "communication_case"
    )
}

/// Whether a declaration-like node binds with `:=` rather than assigns.
fn defines(n: Node<'_>) -> bool {
    let mut cursor = n.walk();
    n.children(&mut cursor)
        .any(|c| !c.is_named() && c.kind() == ":=")
}

/// The identifiers directly in a node (an expression list, a name field).
fn identifiers(src: &[u8], n: Node<'_>) -> Vec<String> {
    if n.kind() == "identifier" {
        return vec![text(src, n)];
    }
    named_children(n)
        .into_iter()
        .filter(|c| c.kind() == "identifier")
        .map(|c| text(src, c))
        .collect()
}

/// Names visible at a point of a function, by Go's lexical scopes: how many
/// open scopes bind each name, so a lookup is O(1) however deep they nest.
struct Scopes {
    live: HashMap<String, u32>,
    /// The names each open scope bound, innermost last.
    frames: Vec<Vec<String>>,
}

impl Scopes {
    fn bind(&mut self, name: String) {
        if name == "_" {
            return;
        }
        *self.live.entry(name.clone()).or_default() += 1;
        if let Some(f) = self.frames.last_mut() {
            f.push(name);
        }
    }

    fn open(&mut self) {
        self.frames.push(Vec::new());
    }

    fn close(&mut self) {
        for name in self.frames.pop().unwrap_or_default() {
            if let Some(c) = self.live.get_mut(&name) {
                *c = c.saturating_sub(1);
            }
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.live.get(name).is_some_and(|c| *c > 0)
    }
}

/// The paths a function's body calls, in source order, without repeats.
///
/// A name is local where Go's scoping makes it so: parameters, results and
/// type parameters (the receiver's included) in the whole function; a `:=`,
/// `var`, `const` or local `type` from its statement to the end of its
/// block; a function literal's parameters in it; `range`, receive and
/// type-switch variables in their statement. The receiver is the receiver
/// wherever no inner binding shadows its name.
fn calls(src: &[u8], func: Node<'_>, body: Node<'_>, receiver: Option<&str>) -> Vec<Vec<String>> {
    let mut scopes = Scopes {
        live: HashMap::new(),
        frames: vec![Vec::new()],
    };
    let mut names = HashSet::new();
    for field in ["parameters", "result", "type_parameters"] {
        if let Some(p) = func.child_by_field_name(field) {
            bind_params(src, p, &mut names);
        }
    }
    // `func (l *List[T]) …`: the receiver's type parameters.
    if let Some(recv) = func.child_by_field_name("receiver") {
        preorder(recv, |n| {
            if n.kind() == "type_arguments" {
                preorder(n, |t| {
                    if t.kind() == "type_identifier" {
                        names.insert(text(src, t));
                    }
                    true
                });
                return false;
            }
            true
        });
    }
    for n in names {
        scopes.bind(n);
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    // Type-switch aliases waiting for their value to be walked.
    let mut pending: Vec<(usize, Vec<String>)> = Vec::new();
    walk(body, |n, entering| {
        let kind = n.kind();
        if !entering {
            if let Some(i) = pending.iter().position(|(id, _)| *id == n.id()) {
                let (_, names) = pending.swap_remove(i);
                for name in names {
                    scopes.bind(name);
                }
            }
            // Bindings that take effect after their statement.
            match kind {
                "short_var_declaration" | "range_clause" | "receive_statement" if defines(n) => {
                    if let Some(left) = n.child_by_field_name("left") {
                        for name in identifiers(src, left) {
                            scopes.bind(name);
                        }
                    }
                }
                "var_spec" | "const_spec" => {
                    let mut cursor = n.walk();
                    let named: Vec<Node<'_>> =
                        n.children_by_field_name("name", &mut cursor).collect();
                    for name in named {
                        for id in identifiers(src, name) {
                            scopes.bind(id);
                        }
                    }
                }
                _ => {}
            }
            if opens_scope(kind) {
                scopes.close();
            }
            return;
        }
        if opens_scope(kind) {
            scopes.open();
        }
        match kind {
            "func_literal" => {
                let mut bound = HashSet::new();
                for field in ["parameters", "result"] {
                    if let Some(p) = n.child_by_field_name(field) {
                        bind_params(src, p, &mut bound);
                    }
                }
                for name in bound {
                    scopes.bind(name);
                }
            }
            // A local type is in scope in its own declaration.
            "type_spec" | "type_alias" => {
                if let Some(name) = field_text(src, n, "name") {
                    scopes.bind(name);
                }
            }
            // `switch v := x.(type)`: `v` is in scope in the cases, not in
            // `x`; it is bound when the walk leaves the value.
            "type_switch_statement" => {
                if let Some(alias) = n.child_by_field_name("alias") {
                    let names = identifiers(src, alias);
                    match n.child_by_field_name("value") {
                        Some(v) => pending.push((v.id(), names)),
                        None => names.into_iter().for_each(|m| scopes.bind(m)),
                    }
                }
            }
            _ => {}
        }
        let target = match kind {
            "call_expression" => n.child_by_field_name("function"),
            // `pkg.F[int](x)` reads as a conversion to a generic type.
            "type_conversion_expression" => n
                .child_by_field_name("type")
                .filter(|t| t.kind() == "generic_type"),
            _ => None,
        };
        if let Some(mut path) = target.and_then(|f| call_path(src, f)) {
            let first = path.first().cloned().unwrap_or_default();
            let keep = if scopes.is_local(&first) {
                false
            } else if Some(first.as_str()) == receiver {
                path[0] = "self".into();
                path.len() > 1
            } else {
                true
            };
            if keep && seen.insert(path.clone()) {
                out.push(path);
            }
        }
    });
    out
}

/// `f`, `pkg.F`, `x.y.M`, `F[int]` as segments; `None` for anything else
/// (`f()()`, `xs[0]()`, a function literal).
fn call_path(src: &[u8], f: Node<'_>) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = f;
    loop {
        match cur.kind() {
            "identifier" | "type_identifier" | "package_identifier" => {
                out.push(text(src, cur));
                break;
            }
            "selector_expression" => {
                out.push(field_text(src, cur, "field")?);
                cur = cur.child_by_field_name("operand")?;
            }
            "qualified_type" => {
                out.push(field_text(src, cur, "name")?);
                cur = cur.child_by_field_name("package")?;
            }
            // Type arguments: `F[int]`, `pkg.F[int]`.
            "generic_type" => cur = cur.child_by_field_name("type")?,
            // `F[T]`, not `handlers[0]` or `table["k"]`.
            "index_expression" => {
                let index = cur.child_by_field_name("index")?;
                if !matches!(
                    index.kind(),
                    "identifier"
                        | "type_identifier"
                        | "qualified_type"
                        | "selector_expression"
                        | "generic_type"
                        | "pointer_type"
                        | "slice_type"
                        | "map_type"
                        | "array_type"
                ) {
                    return None;
                }
                cur = cur.child_by_field_name("operand")?;
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

/// Adds the names a parameter list binds.
fn bind_params(src: &[u8], params: Node<'_>, names: &mut HashSet<String>) {
    for p in named_children(params) {
        let mut cursor = p.walk();
        for name in p.children_by_field_name("name", &mut cursor) {
            if matches!(name.kind(), "identifier" | "type_identifier") {
                names.insert(text(src, name));
            }
        }
    }
}

/// A Go string literal's text without its quotes.
fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '`').to_owned()
}

/// Visits `root` and its descendants in source order, calling `f` with
/// `true` on the way in and `false` on the way out of each.
fn walk<'t>(root: Node<'t>, mut f: impl FnMut(Node<'t>, bool)) {
    let mut cursor = root.walk();
    loop {
        f(cursor.node(), true);
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            f(cursor.node(), false);
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

/// Whether a node is a literal hashed as one token: its text between
/// escapes is in no leaf.
fn atomic(kind: &str) -> bool {
    matches!(
        kind,
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal"
    )
}

/// Whether a node is a statement or declaration: what `;` and newlines
/// separate.
fn is_unit(kind: &str) -> bool {
    kind.ends_with("_statement")
        || kind.ends_with("_declaration")
        || matches!(
            kind,
            "field_declaration"
                | "method_elem"
                | "type_elem"
                | "import_spec"
                | "var_spec"
                | "const_spec"
                | "type_spec"
                | "type_alias"
        )
}

/// FNV-1a over the leaf tokens of `roots`, in order, skipping comments and
/// the subtrees in `skip`, with a separator after each token.
fn hash(src: &[u8], roots: &[Node<'_>], skip: &[Node<'_>]) -> u64 {
    let skip: HashSet<usize> = skip.iter().map(Node::id).collect();
    let mut h = FNV_OFFSET;
    let mut mix = |b: u8| h = (h ^ u64::from(b)).wrapping_mul(FNV_PRIME);
    for root in roots {
        // One cursor, and the kinds above it: asking a node for its parent
        // or next sibling walks down from the root, quadratic when deep.
        let mut cursor = root.walk();
        let mut above: Vec<&str> = Vec::new();
        // A comma waits, at its depth, for the next token beside it: it is
        // hashed only if that is not a closing bracket.
        let mut comma: Option<usize> = None;
        loop {
            let n = cursor.node();
            let depth = above.len();
            if comma == Some(depth) && n.kind() != "comment" {
                comma = None;
                if !matches!(n.kind(), ")" | "}" | "]") {
                    mix(b',');
                    mix(0x1f);
                }
            }
            let mut descend = !(n.kind() == "comment" || skip.contains(&n.id()));
            // gofmt's choices mean the same: `;` or a newline between
            // statements, and a trailing comma before a closing bracket. In
            // a type parameter list the comma matters (`[P *C,]` is
            // generic, `[P *C]` an array length), so it stays. What the
            // separators delimit is hashed instead: a marker opens every
            // statement and declaration, so `{ x; y }` (two fields) and
            // `{ x y }` (one) still differ.
            if descend && !n.is_named() {
                match n.kind() {
                    ";" | "\n" => descend = false,
                    "," if above.last() != Some(&"type_parameter_list") => {
                        comma = Some(depth);
                        descend = false;
                    }
                    _ => {}
                }
            } else if descend && is_unit(n.kind()) {
                mix(0x1e);
            }
            if descend {
                let whole = atomic(n.kind());
                if (whole || n.child_count() == 0) && n.end_byte() > n.start_byte() {
                    for b in &src[n.byte_range()] {
                        mix(*b);
                    }
                    mix(0x1f);
                }
                if !whole && cursor.goto_first_child() {
                    above.push(n.kind());
                    continue;
                }
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
                // Nothing follows at this depth: a waiting comma trailed.
                if comma == Some(above.len()) {
                    comma = None;
                }
                if !cursor.goto_parent() {
                    done = true;
                    break;
                }
                above.pop();
            }
            if done {
                break;
            }
        }
    }
    h
}
