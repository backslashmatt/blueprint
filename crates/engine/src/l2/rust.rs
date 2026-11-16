//! L2 for Rust: the module tree of a Cargo package.
//!
//! **Crate roots** come from the package's `Cargo.toml`: `[lib] path` (else
//! `src/lib.rs`), each `[[bin]] path`, and, unless `autobins = false`,
//! `src/main.rs`, `src/bin/*.rs` and `src/bin/*/main.rs`. Each root file is
//! parsed with `tree-sitter-rust` and only its `mod` items are read; every
//! module becomes an L2 node whose path is its module path (`l1::cargo` is
//! `["l1", "cargo"]`). Modules declared by several roots, or by several
//! `cfg` variants, become one node holding all of their children.
//!
//! File lookup follows the Rust reference:
//! - `mod x;` in a crate root, a `mod.rs`, or a file loaded by `#[path]`
//!   looks in that file's directory; in any other file `f.rs` it looks in
//!   `f/`. Inline `mod x { .. }` blocks add a directory level, or use their
//!   own `#[path]` directory.
//! - `x.rs` is tried before `x/mod.rs`.
//! - `#[path = ".."]` is relative to the current file's directory, or to the
//!   inline block's directory inside one.
//! - `mod x;` written inside a macro call (as with `cfg_if!`) is found by a
//!   shallow token scan.
//!
//! Confidence:
//! - `exact`: the module's file was found and parsed cleanly, or it is
//!   inline in a clean file;
//! - `inferred`: the file is missing, a `cfg_attr` may change its path, it
//!   was declared inside a macro, or nesting went past [`MAX_DEPTH`];
//! - `unknown`: the file cannot be read or has syntax errors. Modules are
//!   still read from tree-sitter's recovered tree.
//!
//! Recursion is bounded by [`MAX_DEPTH`] and the work per crate by
//! [`MAX_FILE_WALKS`], so hostile input cannot exhaust the stack or time.

use std::collections::BTreeSet;

use blueprint_model::{
    Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey, Visibility,
};
use toml::{Table, Value};
use tree_sitter::{Node as TsNode, Parser};

use crate::l1::{Diagnostic, Read};

/// Module nesting, inline or by file, deeper than this is not followed.
pub const MAX_DEPTH: usize = 64;

/// Files walked per crate before giving up (a file may legally be included
/// more than once, so this bounds repeated inclusion too).
pub const MAX_FILE_WALKS: usize = 20_000;

struct Ctx<'a> {
    system: &'a NodeId,
    key: &'a SystemKey,
    files: &'a BTreeSet<&'a RepoPath>,
    read: Read<'a>,
    parser: Parser,
    /// Files on the current inclusion chain, to stop `#[path]` cycles.
    stack: Vec<Vec<String>>,
    walks: usize,
    /// Every file walked: what the module tree reaches.
    reached: BTreeSet<RepoPath>,
    model: &'a mut Model,
    diags: &'a mut Vec<Diagnostic>,
}

/// A `mod` declaration.
struct ModItem<'t> {
    name: String,
    body: Option<TsNode<'t>>,
    path_attr: Option<String>,
    cfg_attr_path: bool,
    in_macro: bool,
    visibility: Visibility,
}

/// The visibility a `pub`, `pub(..)` or absent modifier gives.
fn visibility_of(modifier: Option<&str>) -> Visibility {
    match modifier.map(str::trim) {
        Some("pub") => Visibility::Public,
        Some(m) if m.starts_with("pub") || m == "crate" => Visibility::Restricted,
        _ => Visibility::Private,
    }
}

/// Adds the module tree of the package `system` to `model`, and returns the
/// files it reached; `None` if the package has no Rust crate root, so the
/// caller can fall back to directories.
pub fn analyse_crate(
    system: &NodeId,
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    read: Read<'_>,
    model: &mut Model,
    diags: &mut Vec<Diagnostic>,
) -> Option<BTreeSet<RepoPath>> {
    let roots = crate_roots(key, files, read);
    if roots.is_empty() {
        return None;
    }
    let mut parser = Parser::new();
    if let Err(e) = parser.set_language(&tree_sitter_rust::LANGUAGE.into()) {
        diags.push(Diagnostic {
            path: key.dir.clone(),
            message: format!("Rust grammar: {e}"),
        });
        return None;
    }
    let mut ctx = Ctx {
        system,
        key,
        files,
        read,
        parser,
        stack: Vec::new(),
        walks: 0,
        reached: BTreeSet::new(),
        model,
        diags,
    };
    for root in roots {
        let dir = root[..root.len() - 1].to_vec();
        ctx.walk_file(&root, &dir, &[], 0);
    }
    Some(ctx.reached)
}

/// The package's crate root files that exist, lib first, without duplicates.
fn crate_roots(key: &SystemKey, files: &BTreeSet<&RepoPath>, read: Read<'_>) -> Vec<Vec<String>> {
    let dir = key.dir.segments();
    let mut manifest_path = dir.to_vec();
    manifest_path.push("Cargo.toml".into());
    let manifest: Table = read(&repo_path(&manifest_path))
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();
    let mut rels: Vec<String> = Vec::new();
    let lib = manifest
        .get("lib")
        .and_then(|l| l.get("path"))
        .and_then(Value::as_str);
    rels.push(lib.unwrap_or("src/lib.rs").to_owned());
    for bin in manifest
        .get("bin")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(p) = bin.get("path").and_then(Value::as_str) {
            rels.push(p.to_owned());
        }
    }
    let autobins = manifest
        .get("package")
        .and_then(|p| p.get("autobins"))
        .and_then(Value::as_bool)
        != Some(false);
    if autobins {
        rels.push("src/main.rs".into());
        let mut bin_dir = dir.to_vec();
        bin_dir.extend(["src".to_owned(), "bin".to_owned()]);
        for f in files.iter().filter(|f| f.segments().starts_with(&bin_dir)) {
            match &f.segments()[bin_dir.len()..] {
                [file] if file.ends_with(".rs") => rels.push(format!("src/bin/{file}")),
                [sub, main] if main == "main.rs" => rels.push(format!("src/bin/{sub}/main.rs")),
                _ => {}
            }
        }
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    for rel in rels {
        if let Some(p) = normalise(dir, &rel)
            && files.contains(&repo_path(&p))
            && !out.contains(&p)
        {
            out.push(p);
        }
    }
    out
}

fn repo_path(segs: &[String]) -> RepoPath {
    RepoPath::parse(&segs.join("/")).unwrap_or_else(|_| RepoPath::root())
}

fn normalise(base: &[String], rel: &str) -> Option<Vec<String>> {
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || rel.contains(':') {
        return None;
    }
    let mut out = base.to_vec();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s.to_owned()),
        }
    }
    Some(out)
}

/// The text of a string literal node.
fn string_value(node: TsNode<'_>, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    let content = node
        .children(&mut cur)
        .find(|c| c.kind() == "string_content")?;
    content.utf8_text(src).ok().map(str::to_owned)
}

/// The leaf tokens under `node`, in order, without recursion.
fn leaves<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut out = Vec::new();
    let mut cur = node.walk();
    'walk: loop {
        let n = cur.node();
        if n.child_count() == 0 {
            out.push(n);
        } else if cur.goto_first_child() {
            continue;
        }
        loop {
            if cur.goto_next_sibling() {
                continue 'walk;
            }
            if !cur.goto_parent() || cur.node().id() == node.id() {
                break 'walk;
            }
        }
    }
    out
}

/// Whether a `cfg_attr` attribute sets `path = ..`.
fn sets_path(attr: TsNode<'_>, src: &[u8]) -> bool {
    let toks: Vec<&str> = leaves(attr)
        .iter()
        .filter_map(|t| t.utf8_text(src).ok())
        .collect();
    toks.windows(2).any(|w| w == ["path", "="])
}

/// The `mod` declarations directly inside `list` (a `source_file` or a
/// `declaration_list`), with the attributes written just before each, and
/// those found by a token scan of macro calls.
fn mod_items<'t>(list: TsNode<'t>, src: &[u8]) -> Vec<ModItem<'t>> {
    let mut out = Vec::new();
    let mut path_attr = None;
    let mut cfg_attr_path = false;
    let mut cur = list.walk();
    for child in list.named_children(&mut cur) {
        match child.kind() {
            "attribute_item" => {
                if let Some(attr) = child.named_child(0) {
                    let name = attr
                        .named_child(0)
                        .and_then(|n| n.utf8_text(src).ok())
                        .unwrap_or("");
                    if name == "path" {
                        path_attr = attr
                            .child_by_field_name("value")
                            .and_then(|v| string_value(v, src));
                    } else if name == "cfg_attr" {
                        cfg_attr_path |= sets_path(attr, src);
                    }
                }
                continue;
            }
            "line_comment" | "block_comment" => continue,
            "mod_item" => {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .map(|s| s.strip_prefix("r#").unwrap_or(s).to_owned());
                if let Some(name) = name {
                    out.push(ModItem {
                        name,
                        body: child.child_by_field_name("body"),
                        path_attr: path_attr.take(),
                        cfg_attr_path,
                        in_macro: false,
                        visibility: visibility_of(
                            child
                                .named_child(0)
                                .filter(|c| c.kind() == "visibility_modifier")
                                .and_then(|c| c.utf8_text(src).ok()),
                        ),
                    });
                }
            }
            "macro_invocation" | "expression_statement" => {
                let toks: Vec<TsNode<'t>> = leaves(child);
                let text = |i: usize| {
                    toks.get(i)
                        .and_then(|t| t.utf8_text(src).ok())
                        .unwrap_or("")
                };
                for i in 0..toks.len() {
                    if text(i) == "mod"
                        && text(i + 2) == ";"
                        && toks.get(i + 1).is_some_and(|t| t.kind() == "identifier")
                    {
                        let n = text(i + 1);
                        out.push(ModItem {
                            name: n.strip_prefix("r#").unwrap_or(n).to_owned(),
                            body: None,
                            path_attr: None,
                            cfg_attr_path: false,
                            in_macro: true,
                            // `pub mod x;` or `pub(crate) mod x;` in tokens.
                            visibility: match (
                                i.checked_sub(1).map(text),
                                i.checked_sub(4).map(text),
                            ) {
                                (Some("pub"), _) => Visibility::Public,
                                (Some(")"), Some("pub")) => Visibility::Restricted,
                                _ => Visibility::Private,
                            },
                        });
                    }
                }
            }
            _ => {}
        }
        path_attr = None;
        cfg_attr_path = false;
    }
    out
}

impl Ctx<'_> {
    fn id(&self, module: &[String]) -> NodeId {
        NodeId::within(Level::Module, self.key.clone(), module.to_vec())
    }

    /// Adds a module node unless it exists (another root or `cfg` variant
    /// declared it first). Returns whether it was added.
    fn add_module(
        &mut self,
        module: &[String],
        source: &[String],
        confidence: Confidence,
        visibility: Visibility,
    ) -> bool {
        let id = self.id(module);
        if self.model.node(&id).is_some() {
            return false;
        }
        let node = Node {
            id,
            kind: NodeKind::Module,
            name: module.join("::"),
            source: repo_path(source),
            parent: Some(self.system.clone()),
            confidence,
            visibility: Some(visibility),
        };
        match self.model.add_node(node) {
            Ok(()) => true,
            Err(e) => {
                self.diags.push(Diagnostic {
                    path: repo_path(source),
                    message: e.to_string(),
                });
                false
            }
        }
    }

    fn diag(&mut self, file: &[String], message: impl Into<String>) {
        self.diags.push(Diagnostic {
            path: repo_path(file),
            message: message.into(),
        });
    }

    /// Parses `file` (the source of `module`; empty for a crate root) and
    /// follows its `mod` items; `child_dir` is where its `mod x;` files live.
    /// Returns the confidence the file earns its module.
    fn walk_file(
        &mut self,
        file: &[String],
        child_dir: &[String],
        module: &[String],
        depth: usize,
    ) -> Confidence {
        if self.stack.iter().any(|f| f == file) {
            self.diag(file, "module file includes itself through #[path]");
            return Confidence::Inferred;
        }
        if self.walks >= MAX_FILE_WALKS {
            if self.walks == MAX_FILE_WALKS {
                self.diag(file, format!("stopped after {MAX_FILE_WALKS} module files"));
                self.walks += 1;
            }
            return Confidence::Inferred;
        }
        self.walks += 1;
        let path = repo_path(file);
        self.reached.insert(path.clone());
        let Some(bytes) = (self.read)(&path) else {
            self.diag(file, "cannot be read");
            return Confidence::Unknown;
        };
        let Some(tree) = self.parser.parse(&bytes, None) else {
            self.diag(file, "parse cancelled");
            return Confidence::Unknown;
        };
        let root = tree.root_node();
        let conf = if root.has_error() {
            self.diag(file, "syntax errors; modules read from the recovered tree");
            Confidence::Unknown
        } else {
            Confidence::Exact
        };
        let parent_dir = file[..file.len() - 1].to_vec();
        self.stack.push(file.to_vec());
        self.walk_items(
            root,
            &bytes,
            file,
            conf,
            &parent_dir,
            child_dir,
            module,
            depth,
        );
        self.stack.pop();
        conf
    }

    /// Follows the `mod` items in `list`, which is in `file` (whose own
    /// confidence is `file_conf`). `path_base` is what `#[path]` is
    /// relative to; `child_dir` is where `mod x;` looks.
    #[allow(clippy::too_many_arguments)]
    fn walk_items(
        &mut self,
        list: TsNode<'_>,
        src: &[u8],
        file: &[String],
        file_conf: Confidence,
        path_base: &[String],
        child_dir: &[String],
        module: &[String],
        depth: usize,
    ) {
        for item in mod_items(list, src) {
            let mut sub = module.to_vec();
            sub.push(item.name.clone());
            if depth >= MAX_DEPTH {
                if self.add_module(&sub, file, Confidence::Inferred, item.visibility) {
                    self.diag(
                        file,
                        format!("modules nested deeper than {MAX_DEPTH} are not followed"),
                    );
                }
                continue;
            }
            if let Some(body) = item.body {
                let inner = match &item.path_attr {
                    Some(p) => normalise(path_base, p),
                    None => Some([child_dir, std::slice::from_ref(&item.name)].concat()),
                };
                let conf = if inner.is_some() {
                    file_conf
                } else {
                    Confidence::Inferred
                };
                self.add_module(
                    &sub,
                    file,
                    conf.min(if item.cfg_attr_path {
                        Confidence::Inferred
                    } else {
                        conf
                    }),
                    item.visibility,
                );
                if let Some(inner) = inner {
                    self.walk_items(body, src, file, file_conf, &inner, &inner, &sub, depth + 1);
                }
                continue;
            }
            let (target, its_children) = match &item.path_attr {
                Some(p) => match normalise(path_base, p) {
                    Some(t) => {
                        let dir = t[..t.len().saturating_sub(1)].to_vec();
                        (Some(t), dir)
                    }
                    None => (None, Vec::new()),
                },
                None => {
                    let nested = [child_dir, std::slice::from_ref(&item.name)].concat();
                    let flat = [child_dir, &[format!("{}.rs", item.name)]].concat();
                    let modrs = [nested.as_slice(), &["mod.rs".to_owned()]].concat();
                    if self.files.contains(&repo_path(&flat)) {
                        (Some(flat), nested)
                    } else if self.files.contains(&repo_path(&modrs)) {
                        (Some(modrs), nested)
                    } else {
                        (None, nested)
                    }
                }
            };
            match target.filter(|t| self.files.contains(&repo_path(t))) {
                Some(t) => {
                    let mut conf = self.walk_file(&t, &its_children, &sub, depth + 1);
                    if item.cfg_attr_path || item.in_macro {
                        conf = conf.min(Confidence::Inferred);
                    }
                    self.add_module(&sub, &t, conf, item.visibility);
                }
                None => {
                    if self.add_module(&sub, file, Confidence::Inferred, item.visibility) {
                        self.diag(file, format!("file for `mod {}` not found", item.name));
                    }
                }
            }
        }
    }
}
