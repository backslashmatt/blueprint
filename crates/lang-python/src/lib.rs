//! The Python analyser (REQUIREMENTS §3.4): L3 classes and L4 functions,
//! read from one file at a time with tree-sitter-python.
//!
//! What the model sees:
//! - **Types (L3):** `class` declarations, and `type X = …` aliases
//!   (keyword `type`). A nested class is named by its path in its outer
//!   classes (`Outer.Inner`). A class's decorators, name, bases and the
//!   names and annotations of its class-level attributes are its
//!   signature; its docstring, methods, nested classes and attribute values
//!   are its body.
//! - **Functions (L4):** a module-level `def` is an [`ItemKind::Function`];
//!   a `def` in a class is an [`ItemKind::Method`] of that class. Decorators,
//!   `async`, parameters and annotations are the signature; the block,
//!   docstring included, is the body. Functions and classes defined inside a
//!   function are part of its body, not items.
//! - **Where items are found:** at module and class level, including inside
//!   `if`, `try`, `with`, `for` and `while` blocks there (`if TYPE_CHECKING:`,
//!   version checks). Every item's [`Item::module`] is empty: the engine
//!   places a file's items by its path.
//! - **Visibility:** by convention, a name with a leading `_` is private,
//!   except a dunder (`__init__`); every other name is public.
//! - **References:** every `import`, anywhere in the file, is a
//!   [`Reference::Use`]: `import a.b` is `["a", "b"]`, `import a.b as c`
//!   is that aliased `c`, `from a import b` is `["a", "b"]`, and
//!   `from a import *` is `["a", "*"]`. A relative import's first segment
//!   is its dots: `from ..x import y` is `["..", "x", "y"]` and
//!   `from . import y` is `[".", "y"]`. `from __future__` imports are not
//!   references. Each base class is a [`Reference::Impl`] with `extends` set;
//!   `metaclass=` and other keywords are not bases.
//! - **Calls:** `f()`, `m.f()`, `a.b.f()` as written, unless the first name
//!   is local where the call is. Names are scoped as Python scopes them: a
//!   function's parameters, type parameters and assignments, loop, `with`,
//!   `except` and `case` targets, and nested `def`s are its own; a nested
//!   `def`'s, lambda's or comprehension's names are local only inside it,
//!   while its defaults, annotations and first iterable are read outside
//!   it; a walrus binds in the nearest function; a class body's names are
//!   not seen by the functions inside it; a generic class's or def's type
//!   parameters are seen from its body, methods, annotations and bases
//!   (PEP 695); `global x` makes `x` not local.
//!   In a method, a call through its first parameter (`self`, `cls`) is
//!   written `["self", …]`, whatever the parameter is called; a
//!   `@staticmethod` has no such parameter.
//!
//! Hashes are over tokens, not text, so comments, blank lines and
//! re-indenting a whole definition change nothing; the depth of each token
//! is hashed too, so moving a statement in or out of a block does.

pub mod highlight;

use std::collections::HashSet;

use blueprint_model::lang::{FileFacts, Highlight, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};
use tree_sitter::{Node, Parser};

/// Classes and blocks nested deeper than this are not read, so hostile
/// input cannot make the work list grow without bound.
const MAX_DEPTH: usize = 64;

/// The Python analyser.
#[derive(Debug, Default, Clone, Copy)]
pub struct Python;

impl LanguageAnalyser for Python {
    fn name(&self) -> &'static str {
        "python"
    }

    fn version(&self) -> u32 {
        1
    }

    fn handles(&self, path: &RepoPath) -> bool {
        path.segments()
            .last()
            .is_some_and(|f| f.ends_with(".py") || f.ends_with(".pyi"))
    }

    fn analyse(&self, _path: &RepoPath, bytes: &[u8]) -> FileFacts {
        analyse(bytes)
    }

    fn highlight(&self, bytes: &[u8]) -> Vec<Highlight> {
        highlight::highlight(bytes)
    }
}

/// A statement list to read: its classes, depth, and the type parameters
/// of the classes around it.
type Work<'t> = (Node<'t>, Vec<String>, usize, Vec<String>);

struct Out<'s> {
    src: &'s [u8],
    items: Vec<(usize, Item)>,
    references: Vec<(usize, Reference)>,
}

/// The facts of one Python file.
#[must_use]
pub fn analyse(src: &[u8]) -> FileFacts {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
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
    // (a statement list, the classes it is in, how deep it is)
    let mut work: Vec<Work<'_>> = vec![(root, Vec::new(), 0, Vec::new())];
    while let Some((list, classes, depth, tparams)) = work.pop() {
        for node in named_children(list) {
            out.statement(node, &classes, &tparams, depth, &mut work);
        }
    }
    out.imports(root);
    out.items.sort_by_key(|(start, _)| *start);
    out.references.sort_by_key(|(start, _)| *start);
    FileFacts {
        items: out.items.into_iter().map(|(_, i)| i).collect(),
        references: out.references.into_iter().map(|(_, r)| r).collect(),
        clean: !root.has_error(),
    }
}

/// The definition a statement holds, through any decorators.
fn definition(node: Node<'_>) -> Node<'_> {
    if node.kind() == "decorated_definition" {
        node.child_by_field_name("definition").unwrap_or(node)
    } else {
        node
    }
}

/// The decorators' names, as written, without arguments (`@a.b(1)` gives
/// `a.b`).
fn decorators(src: &[u8], node: Node<'_>) -> Vec<String> {
    if node.kind() != "decorated_definition" {
        return Vec::new();
    }
    named_children(node)
        .into_iter()
        .filter(|c| c.kind() == "decorator")
        .filter_map(|d| {
            let mut e = d.named_child(0)?;
            if e.kind() == "call" {
                e = e.child_by_field_name("function")?;
            }
            dotted(src, e).map(|p| p.join("."))
        })
        .collect()
}

impl Out<'_> {
    fn statement<'t>(
        &mut self,
        node: Node<'t>,
        classes: &[String],
        tparams: &[String],
        depth: usize,
        work: &mut Vec<Work<'t>>,
    ) {
        if depth >= MAX_DEPTH {
            return;
        }
        let def = definition(node);
        match def.kind() {
            "class_definition" => self.class(node, def, classes, tparams, depth, work),
            "function_definition" => self.function(node, def, classes, tparams),
            "type_alias_statement" if classes.is_empty() => self.alias(node),
            // A block's statements, read as the module's (or class's).
            "block" => work.push((def, classes.to_vec(), depth + 1, tparams.to_vec())),
            // Statements that hold definitions conditionally: their
            // definitions are still the module's (or class's). Their
            // blocks and clauses are read as lists of statements.
            "if_statement"
            | "try_statement"
            | "with_statement"
            | "for_statement"
            | "while_statement"
            | "match_statement"
            | "elif_clause"
            | "else_clause"
            | "except_clause"
            | "except_group_clause"
            | "finally_clause"
            | "case_clause" => {
                for c in named_children(def) {
                    if matches!(
                        c.kind(),
                        "block"
                            | "elif_clause"
                            | "else_clause"
                            | "except_clause"
                            | "except_group_clause"
                            | "finally_clause"
                            | "case_clause"
                    ) {
                        work.push((c, classes.to_vec(), depth + 1, tparams.to_vec()));
                    }
                }
            }
            // Parser recovery: what it could read is inside.
            "ERROR" => work.push((node, classes.to_vec(), depth + 1, tparams.to_vec())),
            _ => {}
        }
    }

    fn class<'t>(
        &mut self,
        outer: Node<'t>,
        node: Node<'t>,
        classes: &[String],
        tparams: &[String],
        depth: usize,
        work: &mut Vec<Work<'t>>,
    ) {
        let src = self.src;
        let Some(name) = field_text(src, node, "name") else {
            return;
        };
        let path = [classes.to_vec(), vec![name.clone()]].concat();
        let full = path.join(".");
        if let Some(bases) = node.child_by_field_name("superclasses") {
            for b in named_children(bases) {
                // `Generic[T]` is `Generic`; `metaclass=M` and `*bases` are
                // not bases.
                let target = if b.kind() == "subscript" {
                    b.child_by_field_name("value")
                } else {
                    Some(b)
                };
                if let Some(path) = target.and_then(|t| dotted(src, t)) {
                    let r = Reference::Impl {
                        self_type: vec![full.clone()],
                        r#trait: path,
                        module: Vec::new(),
                        extends: true,
                    };
                    self.references.push((b.start_byte(), r));
                }
            }
        }
        let block = node.child_by_field_name("body");
        let mut body: Vec<Node<'_>> = Vec::new();
        if let Some(block) = block {
            for (i, s) in named_children(block).into_iter().enumerate() {
                match definition(s).kind() {
                    "function_definition" | "class_definition" => body.push(s),
                    "expression_statement" if i == 0 && is_docstring(s) => body.push(s),
                    "expression_statement" => {
                        // `x: int = 1`: the value is body, the name and
                        // annotation signature.
                        match s.named_child(0) {
                            Some(a) if a.kind() == "assignment" => {
                                // `x = y = 1`: every target is a name, the
                                // last value the body.
                                let mut a = a;
                                while let Some(r) = a.child_by_field_name("right")
                                    && r.kind() == "assignment"
                                {
                                    a = r;
                                }
                                body.extend(a.child_by_field_name("right"));
                            }
                            _ => body.push(s),
                        }
                    }
                    "pass_statement" => {}
                    _ => body.push(s),
                }
            }
        }
        let mut skip: Vec<Node<'_>> = node.child_by_field_name("name").into_iter().collect();
        skip.extend(&body);
        let has_body = !body.is_empty();
        let item = Item {
            kind: ItemKind::Type {
                keyword: "class".into(),
            },
            name: full,
            module: Vec::new(),
            lines: (
                line(outer.start_position().row),
                line(outer.end_position().row),
            ),
            body_line: block
                .filter(|_| has_body)
                .map(|b| line(b.start_position().row)),
            visibility: Some(visibility(&name)),
            signature_hash: hash(src, &[outer], &skip),
            body_hash: has_body.then(|| hash(src, &body, &[])),
            calls: Vec::new(),
        };
        self.items.push((outer.start_byte(), item));
        if let Some(b) = block {
            // `class C[T]`: its body and methods see `T`.
            let inner = [tparams.to_vec(), type_param_names(src, node)].concat();
            work.push((b, path, depth + 1, inner));
        }
    }

    fn function(
        &mut self,
        outer: Node<'_>,
        node: Node<'_>,
        classes: &[String],
        tparams: &[String],
    ) {
        let src = self.src;
        let Some(name) = field_text(src, node, "name") else {
            return;
        };
        let block = node.child_by_field_name("body");
        let mut skip: Vec<Node<'_>> = block.into_iter().collect();
        skip.extend(node.child_by_field_name("name"));
        let method = !classes.is_empty();
        let decorators = decorators(src, outer);
        // The receiver: a method's first parameter, unless it is static.
        let receiver = (method
            && !decorators
                .iter()
                .any(|d| d.rsplit('.').next() == Some("staticmethod")))
        .then(|| {
            let params = node.child_by_field_name("parameters")?;
            let first = params.named_child(0)?;
            let id = match first.kind() {
                "identifier" => first,
                "typed_parameter" => first.named_child(0)?,
                "default_parameter" | "typed_default_parameter" => {
                    first.child_by_field_name("name")?
                }
                _ => return None,
            };
            (id.kind() == "identifier").then(|| text(src, id))
        })
        .flatten();
        let item = Item {
            kind: if method {
                ItemKind::Method {
                    self_type: classes.join("."),
                    r#trait: None,
                }
            } else {
                ItemKind::Function
            },
            visibility: Some(visibility(&name)),
            name,
            module: Vec::new(),
            lines: (
                line(outer.start_position().row),
                line(outer.end_position().row),
            ),
            body_line: block.map(|b| line(b.start_position().row)),
            signature_hash: hash(src, &[outer], &skip),
            body_hash: block.map(|b| hash(src, &[b], &[])),
            calls: block
                .map(|b| calls(src, node, b, receiver.as_deref(), tparams))
                .unwrap_or_default(),
        };
        self.items.push((outer.start_byte(), item));
    }

    fn alias(&mut self, node: Node<'_>) {
        let src = self.src;
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        // `type X[T] = …`: the name is the first identifier.
        let mut name_node = None;
        preorder(left, |n| {
            if name_node.is_none() && n.kind() == "identifier" {
                name_node = Some(n);
            }
            name_node.is_none()
        });
        let Some(name_node) = name_node else { return };
        let name = text(src, name_node);
        let item = Item {
            kind: ItemKind::Type {
                keyword: "type".into(),
            },
            visibility: Some(visibility(&name)),
            name,
            module: Vec::new(),
            lines: (
                line(node.start_position().row),
                line(node.end_position().row),
            ),
            body_line: None,
            // The name is not signature, as for classes: a rename is a rename.
            signature_hash: hash(src, &[node], &[name_node]),
            body_hash: None,
            calls: Vec::new(),
        };
        self.items.push((node.start_byte(), item));
    }

    /// Every import statement in the file, wherever it is.
    fn imports(&mut self, root: Node<'_>) {
        let src = self.src;
        let mut found: Vec<Node<'_>> = Vec::new();
        preorder(root, |n| match n.kind() {
            "import_statement" | "import_from_statement" => {
                found.push(n);
                false
            }
            // Leaves and expressions hold no statements worth the walk.
            "string" | "comment" => false,
            _ => true,
        });
        for n in found {
            let mut uses: Vec<(Vec<String>, Option<String>)> = Vec::new();
            let base = if n.kind() == "import_from_statement" {
                let Some(m) = n.child_by_field_name("module_name") else {
                    continue;
                };
                let Some(base) = module_path(src, m) else {
                    continue;
                };
                Some(base)
            } else {
                None
            };
            let mut cursor = n.walk();
            for name in n.children_by_field_name("name", &mut cursor) {
                let (target, alias) = if name.kind() == "aliased_import" {
                    (
                        name.child_by_field_name("name"),
                        field_text(src, name, "alias"),
                    )
                } else {
                    (Some(name), None)
                };
                let Some(path) = target.and_then(|t| dotted(src, t)) else {
                    continue;
                };
                let full = match &base {
                    Some(b) => [b.clone(), path].concat(),
                    None => path,
                };
                uses.push((full, alias));
            }
            if let Some(b) = &base
                && named_children(n)
                    .iter()
                    .any(|c| c.kind() == "wildcard_import")
            {
                uses.push(([b.clone(), vec!["*".into()]].concat(), None));
            }
            for (path, alias) in uses {
                let r = Reference::Use {
                    path,
                    alias,
                    module: Vec::new(),
                };
                self.references.push((n.start_byte(), r));
            }
        }
    }
}

/// A `from` import's module as segments: `a.b` is `["a", "b"]`, `..x` is
/// `["..", "x"]`, `.` is `["."]`.
fn module_path(src: &[u8], m: Node<'_>) -> Option<Vec<String>> {
    match m.kind() {
        "dotted_name" => dotted(src, m),
        "relative_import" => {
            let mut out = Vec::new();
            for c in named_children(m) {
                match c.kind() {
                    "import_prefix" => {
                        let dots: String = text(src, c).chars().filter(|&ch| ch == '.').collect();
                        if dots.is_empty() {
                            return None;
                        }
                        out.push(dots);
                    }
                    "dotted_name" => out.extend(dotted(src, c)?),
                    _ => {}
                }
            }
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

/// A name or attribute chain as segments: `a.b.c` gives `["a", "b", "c"]`;
/// `None` for anything else (`f().x`, `x[0]`).
fn dotted(src: &[u8], node: Node<'_>) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = node;
    loop {
        match cur.kind() {
            "identifier" => {
                out.push(text(src, cur));
                break;
            }
            "dotted_name" => {
                let mut parts: Vec<String> = named_children(cur)
                    .into_iter()
                    .filter(|c| c.kind() == "identifier")
                    .map(|c| text(src, c))
                    .collect();
                parts.reverse();
                out.extend(parts);
                break;
            }
            "attribute" => {
                out.push(field_text(src, cur, "attribute")?);
                cur = cur.child_by_field_name("object")?;
            }
            _ => return None,
        }
        if out.len() > MAX_DEPTH {
            return None;
        }
    }
    out.reverse();
    (!out.is_empty()).then_some(out)
}

/// Whether an expression statement is a bare string: a docstring when it
/// comes first.
fn is_docstring(s: Node<'_>) -> bool {
    s.named_child_count() == 1
        && s.named_child(0)
            .is_some_and(|c| matches!(c.kind(), "string" | "concatenated_string"))
}

/// Python's naming convention: `_x` is private, `__x__` and `x` public.
fn visibility(name: &str) -> Visibility {
    let dunder = name.len() > 4 && name.starts_with("__") && name.ends_with("__");
    if name.starts_with('_') && !dunder {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

/// Whether a node opens a scope of its own names.
fn is_scope(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "lambda"
            | "class_definition"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression"
    )
}

/// One scope in a function: the function itself, or a `def`, lambda,
/// class or comprehension inside it.
#[derive(Default)]
struct Scope {
    /// The scope it is written in; `None` for the function.
    parent: Option<usize>,
    class: bool,
    comprehension: bool,
    bound: HashSet<String>,
    globals: HashSet<String>,
}

/// Where a part of a scope node is evaluated, when not in the scope itself.
#[derive(Clone, Copy)]
enum Part {
    /// In the scope around it: a def's parameters (their defaults), a
    /// lambda's parameters, a class's bases when it has no type parameters,
    /// a comprehension's first iterable.
    Around,
    /// In the def's or class's type-parameter scope (PEP 695): its
    /// annotations, return annotation and bases.
    TypeParams,
}

/// The parts of a scope node that Python evaluates outside its body.
fn outer_parts(n: Node<'_>) -> Vec<(usize, Part)> {
    let generic = n.child_by_field_name("type_parameters").is_some();
    let typed = if generic {
        Part::TypeParams
    } else {
        Part::Around
    };
    let mut out = Vec::new();
    match n.kind() {
        "function_definition" => {
            if let Some(params) = n.child_by_field_name("parameters") {
                out.push((params.id(), Part::Around));
                if generic {
                    for p in named_children(params) {
                        out.extend(
                            p.child_by_field_name("type")
                                .map(|t| (t.id(), Part::TypeParams)),
                        );
                    }
                }
            }
            out.extend(
                n.child_by_field_name("return_type")
                    .map(|c| (c.id(), typed)),
            );
        }
        "lambda" => out.extend(
            n.child_by_field_name("parameters")
                .map(|c| (c.id(), Part::Around)),
        ),
        "class_definition" => out.extend(
            n.child_by_field_name("superclasses")
                .map(|c| (c.id(), typed)),
        ),
        k if is_comprehension(k) => out.extend(
            named_children(n)
                .into_iter()
                .find(|c| c.kind() == "for_in_clause")
                .and_then(|f| f.child_by_field_name("right"))
                .map(|c| (c.id(), Part::Around)),
        ),
        _ => {}
    }
    out
}

/// The id of a generic def's or class's type-parameter scope: its
/// `type_parameters` node.
fn type_param_scope(n: Node<'_>) -> Option<usize> {
    n.child_by_field_name("type_parameters").map(|t| t.id())
}

/// Walks `body` calling `f(node, entering, scope)`, where `scope` is the
/// scope the node is evaluated in: a scope node's outer parts are in the
/// scope around it, the rest of it in itself.
fn scoped_walk<'t>(
    body: Node<'t>,
    root: usize,
    mut f: impl FnMut(Node<'t>, bool, usize, Option<usize>),
) {
    // The current scope, and per open node how many entries it pushed.
    let mut current: Vec<usize> = vec![root];
    let mut pushed: Vec<u8> = Vec::new();
    // Outer parts of the open scope nodes, with the scope around each.
    let mut outer: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    walk(body, |n, entering| {
        if entering {
            let mut count = 0;
            if let Some(&around) = outer.get(&n.id()) {
                current.push(around);
                count += 1;
            }
            let here = *current.last().unwrap_or(&root);
            if is_scope(n.kind()) {
                // A scope's parent is where it is written.
                f(n, true, here, Some(here));
                for (part, at) in outer_parts(n) {
                    let scope = match (at, type_param_scope(n)) {
                        (Part::TypeParams, Some(tp)) => tp,
                        _ => here,
                    };
                    outer.insert(part, scope);
                }
                current.push(n.id());
                count += 1;
            } else {
                f(n, true, here, None);
            }
            pushed.push(count);
        } else {
            for _ in 0..pushed.pop().unwrap_or(0) {
                current.pop();
            }
            outer.remove(&n.id());
            f(n, false, *current.last().unwrap_or(&root), None);
        }
    });
}

/// The paths a function's body calls, in source order, without repeats.
fn calls(
    src: &[u8],
    def: Node<'_>,
    body: Node<'_>,
    receiver: Option<&str>,
    outer_type_params: &[String],
) -> Vec<Vec<String>> {
    use std::collections::HashMap;
    let root = def.id();
    let mut scopes: HashMap<usize, Scope> = HashMap::new();
    let mut own = Scope::default();
    bind_signature(src, def, &mut own.bound);
    // A method of `class C[T]`: `T` is seen from it. Node ids are
    // addresses, so `usize::MAX` is no node's.
    if !outer_type_params.is_empty() {
        own.parent = Some(usize::MAX);
        scopes.insert(
            usize::MAX,
            Scope {
                bound: outer_type_params.iter().cloned().collect(),
                ..Scope::default()
            },
        );
    }
    scopes.insert(root, own);
    let mut outward: Vec<(usize, String)> = Vec::new();
    scoped_walk(body, root, |n, entering, here, parent| {
        if !entering {
            return;
        }
        if let Some(parent) = parent {
            // A generic def or class sits in a scope of its type parameters.
            let parent = match type_param_scope(n) {
                Some(tp) => {
                    let names = type_param_names(src, n);
                    scopes.insert(
                        tp,
                        Scope {
                            parent: Some(parent),
                            bound: names.into_iter().collect(),
                            ..Scope::default()
                        },
                    );
                    tp
                }
                None => parent,
            };
            let mut scope = Scope {
                parent: Some(parent),
                class: n.kind() == "class_definition",
                comprehension: is_comprehension(n.kind()),
                ..Scope::default()
            };
            match n.kind() {
                "function_definition" => bind_signature(src, n, &mut scope.bound),
                "lambda" => {
                    if let Some(p) = n.child_by_field_name("parameters") {
                        bind_params(src, p, &mut scope.bound);
                    }
                }
                "class_definition" => {}
                // A comprehension's `for` targets.
                _ => {
                    for c in named_children(n) {
                        if c.kind() == "for_in_clause"
                            && let Some(left) = c.child_by_field_name("left")
                        {
                            bind_target(src, left, &mut scope.bound);
                        }
                    }
                }
            }
            scopes.insert(n.id(), scope);
            // A `def` or `class` binds its name where it is written.
            if matches!(n.kind(), "function_definition" | "class_definition")
                && let Some(name) = field_text(src, n, "name")
            {
                scopes.entry(here).or_default().bound.insert(name);
            }
            return;
        }
        let mut names = HashSet::new();
        let mut target = here;
        match n.kind() {
            "assignment" | "augmented_assignment" | "for_statement" => {
                if let Some(left) = n.child_by_field_name("left") {
                    bind_target(src, left, &mut names);
                }
            }
            // `(x := …)` binds in the nearest scope that is not a
            // comprehension (PEP 572).
            "named_expression" => {
                names.extend(field_text(src, n, "name"));
                let mut steps = 0;
                while let Some(s) = scopes.get(&target)
                    && s.comprehension
                    && let Some(p) = s.parent
                    && steps < MAX_DEPTH
                {
                    target = p;
                    steps += 1;
                }
            }
            // `with … as f`, `except E as e`.
            "as_pattern_target" => bind_target(src, n, &mut names),
            "case_clause" => {
                for c in named_children(n) {
                    if c.kind() == "case_pattern" {
                        bind_captures(src, c, &mut names);
                    }
                }
            }
            "global_statement" => {
                for c in named_children(n) {
                    let name = text(src, c);
                    scopes.entry(here).or_default().globals.insert(name.clone());
                    outward.push((here, name));
                }
            }
            "nonlocal_statement" => {
                for c in named_children(n) {
                    outward.push((here, text(src, c)));
                }
            }
            // `type X = …` binds `X`.
            "type_alias_statement" => {
                if let Some(left) = n.child_by_field_name("left") {
                    let mut first = None;
                    preorder(left, |c| {
                        if first.is_none() && c.kind() == "identifier" {
                            first = Some(text(src, c));
                        }
                        first.is_none()
                    });
                    names.extend(first);
                }
            }
            _ => {}
        }
        if !names.is_empty() {
            scopes.entry(target).or_default().bound.extend(names);
        }
    });
    // `global x` and `nonlocal x` refer outwards: not the scope's own.
    for (scope, name) in outward {
        if let Some(s) = scopes.get_mut(&scope) {
            s.bound.remove(&name);
        }
    }
    if let (Some(r), Some(s)) = (receiver, scopes.get_mut(&root)) {
        s.bound.remove(r);
    }
    // Python's lookup: the scope itself, then the functions it is written
    // in; a class's names are seen only from its own body. Scopes nested
    // deeper than MAX_DEPTH are not searched, so a lookup is bounded.
    let is_local = |name: &str, from: usize| {
        let mut cur = Some(from);
        for step in 0..MAX_DEPTH {
            let Some(s) = cur.and_then(|c| scopes.get(&c)) else {
                return false;
            };
            if step == 0 || !s.class {
                if s.globals.contains(name) {
                    return false;
                }
                if s.bound.contains(name) {
                    return true;
                }
            }
            cur = s.parent;
        }
        false
    };
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    scoped_walk(body, root, |n, entering, here, _| {
        if !entering || n.kind() != "call" {
            return;
        }
        let Some(mut path) = n
            .child_by_field_name("function")
            .and_then(|f| dotted(src, f))
        else {
            return;
        };
        let first = path.first().cloned().unwrap_or_default();
        let keep = if is_local(&first, here) {
            false
        } else if Some(first.as_str()) == receiver {
            // Only a member of the receiver is a call: `self()` is not.
            path[0] = "self".into();
            path.len() > 1
        } else {
            true
        };
        if keep && seen.insert(path.clone()) {
            out.push(path);
        }
    });
    out
}

/// Whether a scope node is a comprehension.
fn is_comprehension(kind: &str) -> bool {
    matches!(
        kind,
        "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression"
    )
}

/// The names a `def` or `class` declares as type parameters:
/// `[T, *Ts, **P]` gives each parameter's first name.
fn type_param_names(src: &[u8], node: Node<'_>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(tps) = node.child_by_field_name("type_parameters") {
        for t in named_children(tps) {
            let mut first = None;
            preorder(t, |c| {
                if first.is_none() && c.kind() == "identifier" {
                    first = Some(text(src, c));
                }
                first.is_none()
            });
            out.extend(first);
        }
    }
    out
}

/// Adds the names a function's parameters and type parameters bind.
fn bind_signature(src: &[u8], def: Node<'_>, names: &mut HashSet<String>) {
    if let Some(params) = def.child_by_field_name("parameters") {
        bind_params(src, params, names);
    }
    names.extend(type_param_names(src, def));
}

/// Adds the names a `case` pattern captures: `case [h, *rest]`,
/// `case {"k": v, **kw}`, `case P(x=k) as p`; not a class's name
/// (`Point(…)`), a dotted value (`m.CONST`) or a keyword's key.
fn bind_captures(src: &[u8], pattern: Node<'_>, names: &mut HashSet<String>) {
    walk_with_parent(pattern, |n, parent| match n.kind() {
        "dotted_name" if n.named_child_count() == 1 => {
            let class_name = parent.is_some_and(|p| {
                p.kind() == "class_pattern" && p.named_child(0).is_some_and(|c| c.id() == n.id())
            });
            if !class_name && let Some(id) = n.named_child(0) {
                let t = text(src, id);
                if t != "_" {
                    names.insert(t);
                }
            }
        }
        "identifier"
            if parent.is_some_and(|p| matches!(p.kind(), "splat_pattern" | "as_pattern")) =>
        {
            names.insert(text(src, n));
        }
        _ => {}
    });
}

/// Adds the names a parameter list binds to `names`.
fn bind_params(src: &[u8], params: Node<'_>, names: &mut HashSet<String>) {
    for p in named_children(params) {
        match p.kind() {
            "identifier" => {
                names.insert(text(src, p));
            }
            "default_parameter" | "typed_default_parameter" => {
                names.extend(field_text(src, p, "name"));
            }
            "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => {
                for c in named_children(p) {
                    if c.kind() == "identifier" {
                        names.insert(text(src, c));
                        break;
                    }
                    if matches!(c.kind(), "list_splat_pattern" | "dictionary_splat_pattern") {
                        names.extend(c.named_child(0).map(|i| text(src, i)));
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Adds the names an assignment target binds: `a`, `a, b`, `[a, *b]`; not
/// `a.x` or `a[0]`, which bind nothing new.
fn bind_target(src: &[u8], node: Node<'_>, names: &mut HashSet<String>) {
    preorder(node, |n| match n.kind() {
        "identifier" => {
            names.insert(text(src, n));
            false
        }
        "attribute" | "subscript" => false,
        _ => true,
    });
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

/// Visits `root` and its descendants with each one's parent within `root`.
fn walk_with_parent<'t>(root: Node<'t>, mut f: impl FnMut(Node<'t>, Option<Node<'t>>)) {
    let mut parents: Vec<Node<'t>> = Vec::new();
    walk(root, |n, entering| {
        if entering {
            f(n, parents.last().copied());
            parents.push(n);
        } else {
            parents.pop();
        }
    });
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

/// FNV-1a over the leaf tokens of `roots`, in order, each with its depth
/// below its root (Python's blocks are indentation, not tokens), skipping
/// comments, line continuations and the subtrees in `skip`.
fn hash(src: &[u8], roots: &[Node<'_>], skip: &[Node<'_>]) -> u64 {
    let skip: HashSet<usize> = skip.iter().map(Node::id).collect();
    let mut h = FNV_OFFSET;
    let mut mix = |b: u8| h = (h ^ u64::from(b)).wrapping_mul(FNV_PRIME);
    for root in roots {
        let mut cursor = root.walk();
        // The nodes above the cursor, within `root`.
        let mut above: Vec<Node<'_>> = Vec::new();
        loop {
            let n = cursor.node();
            let skipped =
                matches!(n.kind(), "comment" | "line_continuation") || skip.contains(&n.id());
            // A string is one token: its text between escapes is in no leaf.
            let atomic = n.kind() == "string";
            if !skipped && (atomic || n.child_count() == 0) && n.end_byte() > n.start_byte() {
                for b in above.len().to_le_bytes() {
                    mix(b);
                }
                let bytes = &src[n.byte_range()];
                if atomic && is_docstring_at(n, &above) {
                    // A docstring's indentation follows its definition's, so
                    // re-indenting the definition must not change it.
                    let mut line_start = false;
                    for &b in bytes {
                        if line_start && (b == b' ' || b == b'\t') {
                            continue;
                        }
                        line_start = b == b'\n';
                        mix(b);
                    }
                } else {
                    for &b in bytes {
                        mix(b);
                    }
                }
                mix(0x1f);
            }
            if !skipped && !atomic && cursor.goto_first_child() {
                above.push(n);
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
                above.pop();
            }
            if done {
                break;
            }
        }
    }
    h
}

/// Whether a string, with the nodes above it, is a docstring: the first
/// statement of a block or module, and only a string.
fn is_docstring_at(n: Node<'_>, above: &[Node<'_>]) -> bool {
    if n.kind() != "string" {
        return false;
    }
    // `"""a""" "b"`: the parts of a concatenated docstring.
    let above = match above.split_last() {
        Some((p, rest)) if p.kind() == "concatenated_string" => rest,
        _ => above,
    };
    let Some(&statement) = above.last() else {
        return false;
    };
    // At the root of a hash (a class's docstring statement), the block is
    // above the root: one parent lookup per root.
    let Some(block) = above
        .len()
        .checked_sub(2)
        .map(|i| above[i])
        .or_else(|| statement.parent())
    else {
        return false;
    };
    statement.kind() == "expression_statement"
        && statement.named_child_count() == 1
        && matches!(block.kind(), "block" | "module")
        && block
            .named_child(0)
            .is_some_and(|c| c.id() == statement.id())
}
