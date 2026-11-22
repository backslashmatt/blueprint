//! The TypeScript and JavaScript analyser (REQUIREMENTS §3.4): L3 types and
//! L4 functions and methods, read from one file at a time with
//! tree-sitter-typescript.
//!
//! Two analysers share this code, one per grammar: [`TypeScript`] for `.ts`,
//! `.mts` and `.cts`, and [`Tsx`] for `.tsx` and for JavaScript (`.js`,
//! `.jsx`, `.mjs`, `.cjs`), which the TSX grammar reads as a superset. The
//! grammars read some bytes differently (`<T>x` is a cast in one and JSX in
//! the other), and facts depend on the bytes alone, so each has its own name
//! in the cache key.
//!
//! What the model sees:
//! - **Types (L3):** `class` and `abstract class`, `interface`, `enum`,
//!   `type`. A class's heritage, fields and their types are its signature;
//!   its methods, field initial values and `static` blocks are its body (as
//!   a Rust trait's methods are). A method's decorators are the method's
//!   signature, not the class's. An interface, enum or type alias is all
//!   signature.
//! - **Functions (L4):** `function` declarations, and `const f = () => …` or
//!   `= function …` at module level. Overload signatures before an
//!   implementation are part of its signature.
//! - **Methods (L4):** a class's methods, accessors and fields that hold a
//!   function.
//! - **Modules:** `namespace` and `module` blocks give an item's
//!   [`Item::module`], as inline `mod` does in Rust.
//! - **Visibility:** at module level, `export` (also `export { f }` and
//!   `export default f` later in the file) is public and anything else
//!   private (`declare` items describe a public surface, so are public); a
//!   member is public unless `private` or `#name` (private) or `protected`
//!   (restricted).
//! - **References:** every module specifier the file names, kept whole as
//!   the first path segment: `import { a as b } from "./x"` gives
//!   `["./x", "a"]` aliased `b`; also `export … from`, `require("…")` and
//!   `import("…")`. `extends` and `implements` give [`Reference::Impl`].
//! - **Calls:** `f()`, and `ns.f()` where `ns` is imported (or bound by a
//!   `require` at the top of the file); imported names give their import's
//!   path. Names bound in the function are not calls to the module's.
//!
//! Functions nested in function bodies and classes in expressions are not
//! part of the architecture and are not items.

pub mod highlight;

use std::collections::{HashMap, HashSet};

use blueprint_model::lang::{FileFacts, Highlight, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};
use tree_sitter::{Language, Node, Parser};

/// Namespaces nested deeper than this are not read, so hostile input
/// cannot make the work list grow without bound. Matches the Rust analyser.
const MAX_DEPTH: usize = 64;

/// The analyser for TypeScript files (`.ts`, `.mts`, `.cts`).
#[derive(Debug, Default, Clone, Copy)]
pub struct TypeScript;

/// The analyser for TSX and JavaScript files (`.tsx`, `.js`, `.jsx`,
/// `.mjs`, `.cjs`).
#[derive(Debug, Default, Clone, Copy)]
pub struct Tsx;

/// The version of both analysers' output.
const VERSION: u32 = 2;

fn extension(path: &RepoPath) -> Option<&str> {
    path.segments().last()?.rsplit_once('.').map(|(_, e)| e)
}

impl LanguageAnalyser for TypeScript {
    fn name(&self) -> &'static str {
        "ts"
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn handles(&self, path: &RepoPath) -> bool {
        matches!(extension(path), Some("ts" | "mts" | "cts"))
    }

    fn analyse(&self, _path: &RepoPath, bytes: &[u8]) -> FileFacts {
        analyse(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), bytes)
    }

    fn highlight(&self, bytes: &[u8]) -> Vec<Highlight> {
        highlight::highlight(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), bytes)
    }
}

impl LanguageAnalyser for Tsx {
    fn name(&self) -> &'static str {
        "tsx"
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn handles(&self, path: &RepoPath) -> bool {
        matches!(extension(path), Some("tsx" | "js" | "jsx" | "mjs" | "cjs"))
    }

    fn analyse(&self, _path: &RepoPath, bytes: &[u8]) -> FileFacts {
        analyse(&tree_sitter_typescript::LANGUAGE_TSX.into(), bytes)
    }

    fn highlight(&self, bytes: &[u8]) -> Vec<Highlight> {
        highlight::highlight(&tree_sitter_typescript::LANGUAGE_TSX.into(), bytes)
    }
}

struct Out<'s> {
    src: &'s [u8],
    items: Vec<(usize, Item)>,
    references: Vec<(usize, Reference)>,
    /// What each imported name stands for, for calls: `["./x", "a"]`.
    imports: HashMap<String, Vec<String>>,
    /// Names exported by `export { a }` or `export default a`.
    exported: HashSet<String>,
}

/// The facts of one file, read with `language`.
#[must_use]
pub fn analyse(language: &Language, src: &[u8]) -> FileFacts {
    let mut parser = Parser::new();
    if parser.set_language(language).is_err() {
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
        imports: HashMap::new(),
        exported: HashSet::new(),
    };
    out.references_and_imports(root);
    // Explicit work list of (statement list, namespace, ambient): no
    // recursion, whatever the nesting.
    let mut work = vec![(root, Vec::<String>::new(), false)];
    while let Some((list, module, ambient)) = work.pop() {
        let mut overloads: Vec<Node<'_>> = Vec::new();
        for node in named_children(list) {
            out.statement(node, &module, ambient, &mut overloads, &mut work);
        }
    }
    // `const f = …; export { f }`: public, as `export const f` is.
    for (_, item) in &mut out.items {
        let top = item.module.is_empty() && !matches!(item.kind, ItemKind::Method { .. });
        if top && out.exported.contains(&item.name) {
            item.visibility = Some(Visibility::Public);
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
    /// One statement of a module or namespace body. `overloads` holds the
    /// bodiless `function` signatures seen just before it.
    fn statement<'t>(
        &mut self,
        node: Node<'t>,
        module: &[String],
        ambient: bool,
        overloads: &mut Vec<Node<'t>>,
        work: &mut Vec<(Node<'t>, Vec<String>, bool)>,
    ) {
        let src = self.src;
        // The declaration, and the node that spans it with its `export`.
        let (decl, outer, exported) = match node.kind() {
            "export_statement" => match node
                .child_by_field_name("declaration")
                .or_else(|| node.child_by_field_name("value"))
            {
                // `export declare …`: the declaration inside.
                Some(d) if d.kind() == "ambient_declaration" => {
                    match named_children(d)
                        .into_iter()
                        .find(|c| c.kind() != "comment")
                    {
                        Some(inner) => (inner, node, true),
                        None => return,
                    }
                }
                Some(d) => (d, node, true),
                None => return,
            },
            "ambient_declaration" => match named_children(node)
                .into_iter()
                .find(|c| c.kind() != "comment")
            {
                Some(d) => (d, node, true),
                None => return,
            },
            "expression_statement" => match node.named_child(0) {
                Some(d) if d.kind() == "internal_module" => (d, node, false),
                _ => return,
            },
            _ => (node, node, false),
        };
        let ambient = ambient
            || node.kind() == "ambient_declaration"
            || node
                .child_by_field_name("declaration")
                .is_some_and(|d| d.kind() == "ambient_declaration");
        let vis = if exported || ambient {
            Visibility::Public
        } else {
            Visibility::Private
        };
        let export = u64::from(exported);
        match decl.kind() {
            "function_signature" if !ambient => {
                overloads.push(outer);
                return;
            }
            "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "function_expression"
            | "generator_function"
            | "arrow_function" => {
                let name = field_text(src, decl, "name").unwrap_or_else(|| "default".into());
                let pending: Vec<Node<'_>> = std::mem::take(overloads)
                    .into_iter()
                    .filter(|o| function_name(src, *o).as_deref() == Some(name.as_str()))
                    .collect();
                let roots: Vec<Node<'_>> = pending.iter().copied().chain([outer]).collect();
                self.function(
                    name,
                    &roots,
                    decl,
                    decl,
                    module,
                    ItemKind::Function,
                    vis,
                    export,
                );
            }
            "lexical_declaration" | "variable_declaration" => {
                for d in named_children(decl)
                    .into_iter()
                    .filter(|d| d.kind() == "variable_declarator")
                {
                    let (Some(name), Some(value)) = (
                        d.child_by_field_name("name"),
                        d.child_by_field_name("value"),
                    ) else {
                        continue;
                    };
                    if name.kind() == "identifier" && is_function_value(value.kind()) {
                        let roots = [d];
                        let declarators: Vec<Node<'_>> = named_children(decl);
                        let header = mix(export, hash(src, &[decl], &declarators));
                        self.function(
                            text(src, name),
                            &roots,
                            d,
                            value,
                            module,
                            ItemKind::Function,
                            vis,
                            header,
                        );
                    }
                }
            }
            "class_declaration" | "abstract_class_declaration" | "class" => {
                self.class(decl, outer, module, vis, export, ambient);
            }
            "interface_declaration" | "enum_declaration" | "type_alias_declaration" => {
                let keyword = match decl.kind() {
                    "interface_declaration" => "interface",
                    "enum_declaration" => "enum",
                    _ => "type",
                };
                if decl.kind() == "interface_declaration" {
                    self.heritage(decl, module);
                }
                let kind = ItemKind::Type {
                    keyword: keyword.into(),
                };
                self.item(decl, outer, &[outer], &[], module, kind, vis, None, 0);
            }
            "internal_module" | "module" if module.len() < MAX_DEPTH => {
                let (Some(name), Some(body)) = (
                    decl.child_by_field_name("name"),
                    decl.child_by_field_name("body"),
                ) else {
                    return;
                };
                let mut inner = module.to_vec();
                match name.kind() {
                    "string" => inner.push(text(src, name).trim_matches(['"', '\'']).to_owned()),
                    _ => inner.extend(segments(src, name)),
                }
                work.push((body, inner, ambient));
            }
            _ => {}
        }
        overloads.clear();
    }

    /// A function-like item: `roots` give its signature, `func` its body.
    #[allow(clippy::too_many_arguments)]
    fn function(
        &mut self,
        name: String,
        roots: &[Node<'_>],
        named: Node<'_>,
        func: Node<'_>,
        module: &[String],
        kind: ItemKind,
        vis: Visibility,
        header: u64,
    ) {
        let src = self.src;
        let body = func.child_by_field_name("body");
        let mut skip: Vec<Node<'_>> = body.into_iter().collect();
        skip.extend(named.child_by_field_name("name"));
        // An overload's name is the same name: not hashed either.
        skip.extend(roots.iter().filter_map(|r| declared_name(*r)));
        let signature_hash = mix(hash(src, roots, &skip), header);
        let first = roots.first().copied().unwrap_or(named);
        let last = roots.last().copied().unwrap_or(named);
        let item = Item {
            kind,
            name,
            module: module.to_vec(),
            lines: (
                line(first.start_position().row),
                line(last.end_position().row),
            ),
            body_line: body.map(|b| line(b.start_position().row)),
            visibility: Some(vis),
            signature_hash,
            body_hash: body.map(|b| hash(src, &[b], &[])),
            calls: body.map(|b| self.calls(func, b)).unwrap_or_default(),
        };
        self.items.push((first.start_byte(), item));
    }

    /// A named item with no calls: a type.
    #[allow(clippy::too_many_arguments)]
    fn item(
        &mut self,
        decl: Node<'_>,
        outer: Node<'_>,
        roots: &[Node<'_>],
        not_signature: &[Node<'_>],
        module: &[String],
        kind: ItemKind,
        vis: Visibility,
        body_hash: Option<u64>,
        header: u64,
    ) {
        let src = self.src;
        // Only `export default class { … }` has no name.
        let name = decl.child_by_field_name("name");
        let mut skip: Vec<Node<'_>> = name.into_iter().collect();
        skip.extend_from_slice(not_signature);
        let item = Item {
            kind,
            name: name.map_or_else(|| "default".into(), |n| text(src, n)),
            module: module.to_vec(),
            lines: (
                line(outer.start_position().row),
                line(outer.end_position().row),
            ),
            body_line: body_hash
                .and(decl.child_by_field_name("body"))
                .map(|b| line(b.start_position().row)),
            visibility: Some(vis),
            signature_hash: mix(hash(src, roots, &skip), header),
            body_hash,
            calls: Vec::new(),
        };
        self.items.push((outer.start_byte(), item));
    }

    /// A class, its methods, and what it extends and implements.
    #[allow(clippy::too_many_arguments)]
    fn class(
        &mut self,
        decl: Node<'_>,
        outer: Node<'_>,
        module: &[String],
        vis: Visibility,
        export: u64,
        ambient: bool,
    ) {
        let src = self.src;
        let name = field_text(src, decl, "name").unwrap_or_else(|| "default".into());
        self.heritage(decl, module);
        let members = decl
            .child_by_field_name("body")
            .map(named_children)
            .unwrap_or_default();
        // Each member with the decorators just before it (`@Get() m() {}`:
        // in the grammar they are the class body's children, not the
        // member's).
        let mut decorated: Vec<(Vec<Node<'_>>, Node<'_>)> = Vec::new();
        let mut decorators = Vec::new();
        for m in members {
            match m.kind() {
                "decorator" => decorators.push(m),
                "comment" => {}
                _ => decorated.push((std::mem::take(&mut decorators), m)),
            }
        }
        // The class's body: its methods, field initialisers and static
        // blocks. Its signature: everything else, field types included.
        let mut body: Vec<Node<'_>> = Vec::new();
        let mut not_signature: Vec<Node<'_>> = Vec::new();
        for (decorators, m) in &decorated {
            match (m.kind(), method_of(*m)) {
                ("public_field_definition", Some(value)) => body.push(value),
                (_, Some(_)) => {
                    not_signature.extend(decorators);
                    body.push(*m);
                }
                ("public_field_definition", None) => body.extend(m.child_by_field_name("value")),
                ("class_static_block", None) => body.push(*m),
                _ => {}
            }
        }
        not_signature.extend(&body);
        let body_hash = (!body.is_empty()).then(|| hash(src, &body, &[]));
        let kind = ItemKind::Type {
            keyword: "class".into(),
        };
        self.item(
            decl,
            outer,
            &[outer],
            &not_signature,
            module,
            kind,
            vis,
            body_hash,
            export,
        );
        let header = decl
            .child_by_field_name("type_parameters")
            .map_or(0, |t| hash(src, &[t], &[]));
        let mut overloads: Vec<Node<'_>> = Vec::new();
        for (decorators, m) in decorated {
            let Some(func) = method_of(m) else { continue };
            let Some(member) = field_text(src, m, "name") else {
                continue;
            };
            // An overload, unless this is a declaration (`declare class`).
            if !ambient
                && func.child_by_field_name("body").is_none()
                && m.kind() == "method_signature"
            {
                overloads.extend(decorators);
                overloads.push(m);
                continue;
            }
            let pending: Vec<Node<'_>> = std::mem::take(&mut overloads)
                .into_iter()
                .filter(|o| {
                    o.kind() == "decorator"
                        || field_text(src, *o, "name").as_deref() == Some(member.as_str())
                })
                .collect();
            let roots: Vec<Node<'_>> = pending.into_iter().chain(decorators).chain([m]).collect();
            let kind = ItemKind::Method {
                self_type: name.clone(),
                r#trait: None,
            };
            self.function(
                member,
                &roots,
                m,
                func,
                module,
                kind,
                member_visibility(src, m),
                header,
            );
        }
    }

    /// `extends` and `implements` of a class or interface.
    fn heritage(&mut self, decl: Node<'_>, module: &[String]) {
        let src = self.src;
        let Some(name) = field_text(src, decl, "name") else {
            return;
        };
        let mut targets = Vec::new();
        preorder(decl, |n| match n.kind() {
            "class_body" | "interface_body" | "object_type" => false,
            "extends_clause" => {
                targets.extend(n.child_by_field_name("value").map(|v| (v, true)));
                false
            }
            "implements_clause" | "extends_type_clause" => {
                let extends = n.kind() == "extends_type_clause";
                targets.extend(
                    named_children(n)
                        .into_iter()
                        .filter(|c| c.kind() != "comment")
                        .map(|c| (c, extends)),
                );
                false
            }
            _ => true,
        });
        for (t, extends) in targets {
            let base = match t.kind() {
                "generic_type" => t.child_by_field_name("name").unwrap_or(t),
                _ => t,
            };
            let path = segments(src, base);
            if !path.is_empty() {
                let r = Reference::Impl {
                    self_type: vec![name.clone()],
                    r#trait: path,
                    module: module.to_vec(),
                    extends,
                };
                self.references.push((t.start_byte(), r));
            }
        }
    }

    /// Every module specifier the file names, as [`Reference::Use`], and
    /// what each imported name stands for.
    fn references_and_imports(&mut self, root: Node<'_>) {
        let src = self.src;
        let mut found: Vec<(usize, Reference)> = Vec::new();
        let mut bound: Vec<(String, Vec<String>)> = Vec::new();
        let mut exported: HashSet<String> = HashSet::new();
        let mut use_ref = |at: usize, path: Vec<String>, alias: Option<String>| {
            found.push((
                at,
                Reference::Use {
                    path,
                    alias,
                    module: Vec::new(),
                },
            ));
        };
        preorder_with_ancestors(root, |n, ancestors| {
            match n.kind() {
                "import_statement" | "export_statement" => {
                    if n.kind() == "export_statement" && n.child_by_field_name("source").is_none() {
                        exported_names(src, n, &mut exported);
                    }
                    let Some(spec) = n
                        .child_by_field_name("source")
                        .map(|s| string_value(src, s))
                    else {
                        // `export function …` may hold a `require`;
                        // `import x = require(…)` is read below.
                        return true;
                    };
                    let mut any = false;
                    preorder(n, |c| match c.kind() {
                        "import_specifier" | "export_specifier" => {
                            let name = c
                                .child_by_field_name("name")
                                .map(|n| name_value(src, n))
                                .unwrap_or_default();
                            let alias = c.child_by_field_name("alias").map(|a| name_value(src, a));
                            let path = vec![spec.clone(), name.clone()];
                            // A re-export binds no name in this file.
                            if c.kind() == "import_specifier" {
                                bound.push((alias.clone().unwrap_or(name), path.clone()));
                            }
                            use_ref(c.start_byte(), path, alias);
                            any = true;
                            false
                        }
                        "namespace_import" | "namespace_export" => {
                            let alias = c.named_child(0).map(|a| text(src, a));
                            if let Some(a) =
                                alias.as_ref().filter(|_| c.kind() == "namespace_import")
                            {
                                bound.push((a.clone(), vec![spec.clone()]));
                            }
                            use_ref(c.start_byte(), vec![spec.clone(), "*".into()], alias);
                            any = true;
                            false
                        }
                        "import_clause" => {
                            if let Some(d) = c.named_child(0).filter(|d| d.kind() == "identifier") {
                                let alias = text(src, d);
                                let path = vec![spec.clone(), "default".into()];
                                bound.push((alias.clone(), path.clone()));
                                use_ref(d.start_byte(), path, Some(alias));
                                any = true;
                            }
                            true
                        }
                        _ => true,
                    });
                    if !any {
                        let star = n.kind() == "export_statement";
                        let path = if star {
                            vec![spec, "*".into()]
                        } else {
                            vec![spec]
                        };
                        use_ref(n.start_byte(), path, None);
                    }
                    false
                }
                // `import x = require("./x")`.
                "import_require_clause" => {
                    if let Some(spec) = n
                        .child_by_field_name("source")
                        .map(|s| string_value(src, s))
                    {
                        if let Some(name) = n.named_child(0).filter(|c| c.kind() == "identifier") {
                            bound.push((text(src, name), vec![spec.clone()]));
                        }
                        use_ref(n.start_byte(), vec![spec], None);
                    }
                    false
                }
                "call_expression" => {
                    let callee = n.child_by_field_name("function");
                    let is_load = callee.is_some_and(|f| {
                        f.kind() == "import"
                            || (f.kind() == "identifier" && text(src, f) == "require")
                    });
                    let arg = n
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                        .filter(|a| a.kind() == "string");
                    if let (true, Some(arg)) = (is_load, arg) {
                        let spec = string_value(src, arg);
                        // `const x = require("./x")` at the top of the file
                        // binds `x`; anywhere else the name is local.
                        if let Some(name) = top_level_declarator(ancestors)
                            .and_then(|p| p.child_by_field_name("name"))
                            .filter(|p| p.kind() == "identifier")
                        {
                            bound.push((text(src, name), vec![spec.clone()]));
                        }
                        use_ref(n.start_byte(), vec![spec], None);
                    }
                    true
                }
                _ => true,
            }
        });
        self.references.extend(found);
        self.imports.extend(bound);
        self.exported.extend(exported);
    }

    /// The paths a function's body calls, in source order, without repeats:
    /// `f()` and, where `ns` is imported, `ns.f()`. A call to a name bound
    /// in the function (a parameter, a variable, a nested function) is not
    /// a call to a module's function, so it is skipped; a call to an
    /// imported name gives the import's path. Calls in nested arrow
    /// functions and function expressions count; in nested function
    /// declarations and classes they do not.
    fn calls(&self, func: Node<'_>, body: Node<'_>) -> Vec<Vec<String>> {
        let src = self.src;
        let mut locals: HashSet<String> = HashSet::new();
        if matches!(func.kind(), "function_expression" | "generator_function") {
            locals.extend(field_text(src, func, "name"));
        }
        for field in ["parameters", "parameter"] {
            if let Some(p) = func.child_by_field_name(field) {
                bind(src, p, &mut locals);
            }
        }
        preorder(body, |n| match n.kind() {
            "function_declaration" | "generator_function_declaration" | "class_declaration" => {
                locals.extend(field_text(src, n, "name"));
                false
            }
            "variable_declarator" => {
                if let Some(p) = n.child_by_field_name("name") {
                    bind(src, p, &mut locals);
                }
                true
            }
            // `for (const g of gs)`: `g` is local.
            "for_in_statement" => {
                if let Some(p) = n.child_by_field_name("left") {
                    bind(src, p, &mut locals);
                }
                true
            }
            // A function or class expression's own name is local to it.
            "function_expression" | "generator_function" | "class" => {
                locals.extend(field_text(src, n, "name"));
                true
            }
            "formal_parameters" | "catch_clause" => {
                bind(src, n, &mut locals);
                true
            }
            "arrow_function" => {
                if let Some(p) = n.child_by_field_name("parameter") {
                    bind(src, p, &mut locals);
                }
                true
            }
            _ => true,
        });
        let mut out: Vec<Vec<String>> = Vec::new();
        let mut seen: HashSet<Vec<String>> = HashSet::new();
        preorder(body, |n| {
            match n.kind() {
                "function_declaration" | "generator_function_declaration" | "class_declaration" => {
                    return false;
                }
                "call_expression" => {
                    let path = n
                        .child_by_field_name("function")
                        .and_then(|f| match f.kind() {
                            "identifier" => {
                                let name = text(src, f);
                                if locals.contains(&name) {
                                    None
                                } else {
                                    Some(
                                        self.imports
                                            .get(&name)
                                            .cloned()
                                            .unwrap_or_else(|| vec![name]),
                                    )
                                }
                            }
                            "member_expression" => {
                                let object = f
                                    .child_by_field_name("object")
                                    .filter(|o| o.kind() == "identifier")?;
                                let property = field_text(src, f, "property")?;
                                let name = text(src, object);
                                let target = self
                                    .imports
                                    .get(&name)
                                    .filter(|_| !locals.contains(&name))?;
                                Some([target.as_slice(), &[property]].concat())
                            }
                            _ => None,
                        });
                    if let Some(path) = path
                        && seen.insert(path.clone())
                    {
                        out.push(path);
                    }
                }
                _ => {}
            }
            true
        });
        out
    }
}

/// The function a class member defines, if it is a method: the member for
/// a method or accessor, the value for a field holding a function.
fn method_of(member: Node<'_>) -> Option<Node<'_>> {
    match member.kind() {
        "method_definition" | "method_signature" | "abstract_method_signature" => Some(member),
        "public_field_definition" => member
            .child_by_field_name("value")
            .filter(|v| is_function_value(v.kind())),
        _ => None,
    }
}

fn is_function_value(kind: &str) -> bool {
    matches!(
        kind,
        "arrow_function" | "function_expression" | "generator_function"
    )
}

fn member_visibility(src: &[u8], member: Node<'_>) -> Visibility {
    let name_private = member
        .child_by_field_name("name")
        .is_some_and(|n| n.kind() == "private_property_identifier");
    let modifier = named_children(member)
        .into_iter()
        .find(|c| c.kind() == "accessibility_modifier")
        .map(|m| text(src, m));
    match modifier.as_deref() {
        _ if name_private => Visibility::Private,
        Some("private") => Visibility::Private,
        Some("protected") => Visibility::Restricted,
        _ => Visibility::Public,
    }
}

/// The name of a `function` overload signature, through its `export`.
fn function_name(src: &[u8], outer: Node<'_>) -> Option<String> {
    let decl = match outer.kind() {
        "export_statement" => outer.child_by_field_name("declaration")?,
        _ => outer,
    };
    field_text(src, decl, "name")
}

/// A string literal's contents, without quotes.
fn string_value(src: &[u8], s: Node<'_>) -> String {
    text(src, s)
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_owned()
}

/// The names in a dotted name or type: `a.b.C` gives `["a", "b", "C"]`.
fn segments(src: &[u8], node: Node<'_>) -> Vec<String> {
    let mut out = Vec::new();
    preorder(node, |n| {
        match n.kind() {
            "type_arguments" | "arguments" => return false,
            "identifier" | "type_identifier" | "property_identifier" => out.push(text(src, n)),
            _ => {}
        }
        true
    });
    out
}

/// Adds every identifier a parameter list or pattern binds to `names`.
/// Type annotations and default values bind nothing.
fn bind(src: &[u8], pattern: Node<'_>, names: &mut HashSet<String>) {
    preorder_with_ancestors(pattern, |n, ancestors| match n.kind() {
        "type_annotation" | "statement_block" => false,
        "identifier" | "shorthand_property_identifier_pattern" => {
            // A default value (`x = d`, `{ a = d }`) is read, not bound.
            let is_default = ancestors.last().is_some_and(|p| {
                let field = match p.kind() {
                    "required_parameter" | "optional_parameter" => "value",
                    "assignment_pattern" | "object_assignment_pattern" => "right",
                    _ => return false,
                };
                p.child_by_field_name(field)
                    .is_some_and(|v| v.id() == n.id())
            });
            if !is_default {
                names.insert(text(src, n));
            }
            true
        }
        _ => true,
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

/// FNV-1a over the leaf tokens of `roots`, in order, skipping comments and
/// the subtrees in `skip`, with a separator after each token.
fn hash(src: &[u8], roots: &[Node<'_>], skip: &[Node<'_>]) -> u64 {
    let skip: HashSet<usize> = skip.iter().map(Node::id).collect();
    let mut h = FNV_OFFSET;
    for root in roots {
        preorder(*root, |n| {
            if n.kind() == "comment" || skip.contains(&n.id()) {
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

/// Combines two hashes, order-sensitively.
fn mix(a: u64, b: u64) -> u64 {
    b.to_le_bytes()
        .iter()
        .fold(a, |h, x| (h ^ u64::from(*x)).wrapping_mul(FNV_PRIME))
}

/// The name a declaration, or the declaration an `export` holds, declares.
fn declared_name(node: Node<'_>) -> Option<Node<'_>> {
    let decl = match node.kind() {
        "export_statement" => node.child_by_field_name("declaration")?,
        _ => node,
    };
    decl.child_by_field_name("name")
}

/// The names `export { a, b as c }` (no `from`) and `export default a`
/// make public: `a` and `b`, as declared.
fn exported_names(src: &[u8], export: Node<'_>, out: &mut HashSet<String>) {
    if let Some(v) = export
        .child_by_field_name("value")
        .filter(|v| v.kind() == "identifier")
    {
        out.insert(text(src, v));
    }
    preorder(export, |c| {
        if c.kind() == "export_specifier" {
            out.extend(c.child_by_field_name("name").map(|n| name_value(src, n)));
            return false;
        }
        true
    });
}

/// An import or export name, without quotes when it is a string
/// (`import { "a-b" as c }`).
fn name_value(src: &[u8], node: Node<'_>) -> String {
    match node.kind() {
        "string" => string_value(src, node),
        _ => text(src, node),
    }
}

/// The declarator a node is the value of, if that declaration is a
/// statement of the file itself (`const x = …` or `export const x = …`),
/// from the node's ancestors, nearest last.
fn top_level_declarator<'t>(ancestors: &[Node<'t>]) -> Option<Node<'t>> {
    let [.., outer, decl, declarator] = ancestors else {
        return None;
    };
    if declarator.kind() != "variable_declarator"
        || !matches!(decl.kind(), "lexical_declaration" | "variable_declaration")
    {
        return None;
    }
    let top = match outer.kind() {
        "program" => true,
        "export_statement" => ancestors.len() == 4 && ancestors[0].kind() == "program",
        _ => false,
    };
    top.then_some(*declarator)
}

/// As [`preorder`], also giving `visit` the node's ancestors, nearest
/// last, without asking the tree for parents (each such lookup walks down
/// from the root).
pub(crate) fn preorder_with_ancestors<'t>(
    root: Node<'t>,
    mut visit: impl FnMut(Node<'t>, &[Node<'t>]) -> bool,
) {
    let mut cursor = root.walk();
    let mut ancestors: Vec<Node<'t>> = Vec::new();
    loop {
        let node = cursor.node();
        if visit(node, &ancestors) && cursor.goto_first_child() {
            ancestors.push(node);
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
            ancestors.pop();
        }
    }
}
