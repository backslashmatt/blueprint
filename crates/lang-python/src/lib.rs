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
//! - **Calls:** `f()`, `m.f()`, `a.b.f()` as written; a call through a name
//!   bound in the function (a parameter, an assignment, a loop or `with`
//!   target, a comprehension variable, a nested `def`) is skipped. In a
//!   method, a call through its first parameter (`self`, `cls`) is written
//!   `["self", …]`, whatever the parameter is called; a `@staticmethod` has
//!   no such parameter.
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
    let mut work: Vec<(Node<'_>, Vec<String>, usize)> = vec![(root, Vec::new(), 0)];
    while let Some((list, classes, depth)) = work.pop() {
        for node in named_children(list) {
            out.statement(node, &classes, depth, &mut work);
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
        depth: usize,
        work: &mut Vec<(Node<'t>, Vec<String>, usize)>,
    ) {
        if depth >= MAX_DEPTH {
            return;
        }
        let def = definition(node);
        match def.kind() {
            "class_definition" => self.class(node, def, classes, depth, work),
            "function_definition" => self.function(node, def, classes),
            "type_alias_statement" if classes.is_empty() => self.alias(node),
            // A block's statements, read as the module's (or class's).
            "block" => work.push((def, classes.to_vec(), depth + 1)),
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
                        work.push((c, classes.to_vec(), depth + 1));
                    }
                }
            }
            // Parser recovery: what it could read is inside.
            "ERROR" => work.push((node, classes.to_vec(), depth + 1)),
            _ => {}
        }
    }

    fn class<'t>(
        &mut self,
        outer: Node<'t>,
        node: Node<'t>,
        classes: &[String],
        depth: usize,
        work: &mut Vec<(Node<'t>, Vec<String>, usize)>,
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
            work.push((b, path, depth + 1));
        }
    }

    fn function(&mut self, outer: Node<'_>, node: Node<'_>, classes: &[String]) {
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
                .map(|b| calls(src, node, b, receiver.as_deref()))
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

/// The paths a function's body calls, in source order, without repeats.
///
/// Names are scoped as in Python: a nested `def`'s or a lambda's
/// parameters, and a comprehension's variables, are local to it only.
fn calls(src: &[u8], def: Node<'_>, body: Node<'_>, receiver: Option<&str>) -> Vec<Vec<String>> {
    use std::collections::HashMap;
    // The names each scope binds, by the scope's node; the function's own
    // scope is `def`.
    let mut bound: HashMap<usize, HashSet<String>> = HashMap::new();
    let mut own = HashSet::new();
    bind_signature(src, def, &mut own);
    bound.insert(def.id(), own);
    let mut stack: Vec<usize> = vec![def.id()];
    let mut declared_outer: Vec<(usize, String)> = Vec::new();
    walk(body, |n, entering| {
        if !entering {
            if is_scope(n.kind()) {
                stack.pop();
            }
            return;
        }
        let top = *stack.last().unwrap_or(&def.id());
        let mut here = |name: String| {
            bound.entry(top).or_default().insert(name);
        };
        match n.kind() {
            "assignment" | "augmented_assignment" | "for_statement" => {
                if let Some(left) = n.child_by_field_name("left") {
                    let mut names = HashSet::new();
                    bind_target(src, left, &mut names);
                    names.into_iter().for_each(&mut here);
                }
            }
            "named_expression" => field_text(src, n, "name").into_iter().for_each(&mut here),
            // `with … as f`, `except E as e`.
            "as_pattern_target" => {
                let mut names = HashSet::new();
                bind_target(src, n, &mut names);
                names.into_iter().for_each(&mut here);
            }
            "case_clause" => {
                let mut names = HashSet::new();
                for c in named_children(n) {
                    if c.kind() == "case_pattern" {
                        bind_captures(src, c, &mut names);
                    }
                }
                names.into_iter().for_each(&mut here);
            }
            "global_statement" | "nonlocal_statement" => {
                for c in named_children(n) {
                    declared_outer.push((top, text(src, c)));
                }
            }
            _ => {}
        }
        if is_scope(n.kind()) {
            // A `def` or `class` binds its name where it is written.
            if matches!(n.kind(), "function_definition" | "class_definition") {
                field_text(src, n, "name").into_iter().for_each(&mut here);
            }
            let mut inner = HashSet::new();
            match n.kind() {
                "function_definition" => bind_signature(src, n, &mut inner),
                "lambda" => {
                    if let Some(p) = n.child_by_field_name("parameters") {
                        bind_params(src, p, &mut inner);
                    }
                }
                "class_definition" => {}
                // A comprehension's `for` targets.
                _ => {
                    for c in named_children(n) {
                        if c.kind() == "for_in_clause"
                            && let Some(left) = c.child_by_field_name("left")
                        {
                            bind_target(src, left, &mut inner);
                        }
                    }
                }
            }
            bound.entry(n.id()).or_default().extend(inner);
            stack.push(n.id());
        }
    });
    // `global x` and `nonlocal x` refer outwards: not the scope's own.
    for (scope, name) in declared_outer {
        if let Some(set) = bound.get_mut(&scope) {
            set.remove(&name);
        }
    }
    if let (Some(r), Some(set)) = (receiver, bound.get_mut(&def.id())) {
        set.remove(r);
    }
    // How many scopes on the stack bind each name: a lookup is O(1) however
    // deep the scopes nest.
    let mut live: HashMap<String, u32> = HashMap::new();
    let enter = |live: &mut HashMap<String, u32>, id: usize| {
        for name in bound.get(&id).into_iter().flatten() {
            *live.entry(name.clone()).or_default() += 1;
        }
    };
    let leave = |live: &mut HashMap<String, u32>, id: usize| {
        for name in bound.get(&id).into_iter().flatten() {
            if let Some(c) = live.get_mut(name) {
                *c = c.saturating_sub(1);
            }
        }
    };
    enter(&mut live, def.id());
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut seen: HashSet<Vec<String>> = HashSet::new();
    walk(body, |n, entering| {
        if is_scope(n.kind()) {
            if entering {
                enter(&mut live, n.id());
            } else {
                leave(&mut live, n.id());
            }
        }
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
        let local = live.get(&first).is_some_and(|c| *c > 0);
        let keep = if local {
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

/// Adds the names a function's parameters and type parameters bind.
fn bind_signature(src: &[u8], def: Node<'_>, names: &mut HashSet<String>) {
    if let Some(params) = def.child_by_field_name("parameters") {
        bind_params(src, params, names);
    }
    // `def f[T, *Ts, **P]()`: each parameter's first name.
    if let Some(tps) = def.child_by_field_name("type_parameters") {
        for t in named_children(tps) {
            let mut first = None;
            preorder(t, |c| {
                if first.is_none() && c.kind() == "identifier" {
                    first = Some(text(src, c));
                }
                first.is_none()
            });
            names.extend(first);
        }
    }
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
    let [.., grand, parent] = above else {
        return false;
    };
    parent.kind() == "expression_statement"
        && parent.named_child_count() == 1
        && matches!(grand.kind(), "block" | "module")
        && grand.named_child(0).is_some_and(|c| c.id() == parent.id())
        && n.kind() == "string"
}
