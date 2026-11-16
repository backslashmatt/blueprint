//! L2 for Rust: the module tree of a Cargo package.
//!
//! Starting from `src/lib.rs` and `src/main.rs`, each file is parsed with
//! `tree-sitter-rust` and only its `mod` items are read. Each module becomes
//! an L2 node whose path is its module path (`l1::cargo` is `["l1", "cargo"]`).
//!
//! File lookup follows the Rust reference:
//! - `mod x;` in `lib.rs`, `main.rs`, `mod.rs`, or a file named by
//!   `#[path]`, looks in that file's directory; in any other file `f.rs` it
//!   looks in `f/`. Inline `mod x { .. }` blocks add a directory level.
//! - `x.rs` is tried before `x/mod.rs`.
//! - `#[path = ".."]` is relative to the current file's directory, or to the
//!   inline block's directory inside one.
//! - `#[cfg(..)]` modules are included: L2 shows every module that exists.
//!
//! Confidence:
//! - `exact` when the module's file was found and parsed cleanly, or the
//!   module is inline;
//! - `inferred` when its file is missing, or a `cfg_attr` may change its path;
//! - `unknown` when its file cannot be read or does not parse cleanly.
//!   Parsing continues with what tree-sitter recovered.

use std::collections::BTreeSet;

use blueprint_model::{Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey};
use tree_sitter::{Node as TsNode, Parser};

use crate::l1::{Diagnostic, Read};

/// Module nesting deeper than this is not followed (it is almost certainly a
/// `#[path]` loop, which `visited` also stops).
const MAX_DEPTH: usize = 64;

/// The crate roots tried, relative to the package directory.
const ROOTS: [&str; 2] = ["src/lib.rs", "src/main.rs"];

struct Ctx<'a> {
    system: &'a NodeId,
    key: &'a SystemKey,
    files: &'a BTreeSet<&'a RepoPath>,
    read: Read<'a>,
    parser: Parser,
    visited: BTreeSet<Vec<String>>,
    model: &'a mut Model,
    diags: &'a mut Vec<Diagnostic>,
}

/// A parsed `mod` item.
struct ModItem {
    name: String,
    path_attr: Option<String>,
    cfg_attr_path: bool,
}

/// Adds the module tree of the package `system` to `model`. Returns false if
/// the package has no Rust crate root, so the caller can fall back.
pub fn analyse_crate(
    system: &NodeId,
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    read: Read<'_>,
    model: &mut Model,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    let mut parser = Parser::new();
    if let Err(e) = parser.set_language(&tree_sitter_rust::LANGUAGE.into()) {
        diags.push(Diagnostic {
            path: key.dir.clone(),
            message: format!("Rust grammar: {e}"),
        });
        return false;
    }
    let roots: Vec<Vec<String>> = ROOTS
        .iter()
        .map(|r| {
            let mut p = key.dir.segments().to_vec();
            p.extend(r.split('/').map(str::to_owned));
            p
        })
        .filter(|p| files.contains(&repo_path(p)))
        .collect();
    if roots.is_empty() {
        return false;
    }
    let mut ctx = Ctx {
        system,
        key,
        files,
        read,
        parser,
        visited: BTreeSet::new(),
        model,
        diags,
    };
    for root in roots {
        let dir = root[..root.len() - 1].to_vec();
        ctx.walk_file(&root, &dir, &[], 0);
    }
    true
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

/// The text of a string literal node, without quotes or escapes handled
/// beyond the common ones a path would use.
fn string_value(node: TsNode<'_>, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    let content = node
        .children(&mut cur)
        .find(|c| c.kind() == "string_content")?;
    content.utf8_text(src).ok().map(str::to_owned)
}

/// Reads the `mod` items directly inside `list` (a `source_file` or a
/// `declaration_list`), with the attributes written just before each.
fn mod_items<'t>(list: TsNode<'t>, src: &[u8]) -> Vec<(ModItem, Option<TsNode<'t>>)> {
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
                        let text = attr.utf8_text(src).unwrap_or("");
                        cfg_attr_path |= text.contains("path");
                    }
                }
            }
            "line_comment" | "block_comment" => {}
            "mod_item" => {
                let name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .map(|s| s.strip_prefix("r#").unwrap_or(s).to_owned());
                let body = child.child_by_field_name("body");
                if let Some(name) = name {
                    out.push((
                        ModItem {
                            name,
                            path_attr: path_attr.take(),
                            cfg_attr_path,
                        },
                        body,
                    ));
                }
                path_attr = None;
                cfg_attr_path = false;
            }
            _ => {
                path_attr = None;
                cfg_attr_path = false;
            }
        }
    }
    out
}

impl Ctx<'_> {
    fn add_module(&mut self, module: &[String], source: &[String], confidence: Confidence) -> bool {
        let id = NodeId::within(Level::Module, self.key.clone(), module.to_vec());
        if self.model.node(&id).is_some() {
            // `lib.rs` and `main.rs` may both declare it; keep the first.
            return false;
        }
        let node = Node {
            id,
            kind: NodeKind::Module,
            name: module.join("::"),
            source: repo_path(source),
            parent: Some(self.system.clone()),
            confidence,
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

    /// Parses `file` (the source of module `module`; empty for a crate
    /// root) and follows its `mod` items. `child_dir` is where its `mod x;`
    /// files live. Returns the confidence the file earns its module.
    fn walk_file(
        &mut self,
        file: &[String],
        child_dir: &[String],
        module: &[String],
        depth: usize,
    ) -> Confidence {
        if depth > MAX_DEPTH || !self.visited.insert(file.to_vec()) {
            self.diags.push(Diagnostic {
                path: repo_path(file),
                message: "module loop or nesting too deep".into(),
            });
            return Confidence::Inferred;
        }
        let path = repo_path(file);
        let Some(bytes) = (self.read)(&path) else {
            self.diags.push(Diagnostic {
                path,
                message: "cannot be read".into(),
            });
            return Confidence::Unknown;
        };
        let Some(tree) = self.parser.parse(&bytes, None) else {
            self.diags.push(Diagnostic {
                path,
                message: "parse cancelled".into(),
            });
            return Confidence::Unknown;
        };
        let root = tree.root_node();
        let parent_dir = file[..file.len() - 1].to_vec();
        self.walk_items(root, &bytes, file, &parent_dir, child_dir, module, depth);
        if root.has_error() {
            self.diags.push(Diagnostic {
                path,
                message: "syntax errors; modules read from the recovered tree".into(),
            });
            Confidence::Unknown
        } else {
            Confidence::Exact
        }
    }

    /// Follows the `mod` items in `list`. `path_base` is what `#[path]` is
    /// relative to; `child_dir` is where `mod x;` looks.
    #[allow(clippy::too_many_arguments)]
    fn walk_items(
        &mut self,
        list: TsNode<'_>,
        src: &[u8],
        file: &[String],
        path_base: &[String],
        child_dir: &[String],
        module: &[String],
        depth: usize,
    ) {
        for (item, body) in mod_items(list, src) {
            let mut sub = module.to_vec();
            sub.push(item.name.clone());
            if let Some(body) = body {
                if !self.add_module(&sub, file, Confidence::Exact) {
                    continue;
                }
                let mut inner = child_dir.to_vec();
                inner.push(item.name.clone());
                self.walk_items(body, src, file, &inner, &inner, &sub, depth + 1);
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
                    let mut flat = child_dir.to_vec();
                    flat.push(format!("{}.rs", item.name));
                    let mut nested = child_dir.to_vec();
                    nested.push(item.name.clone());
                    let mut modrs = nested.clone();
                    modrs.push("mod.rs".into());
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
                    if self.exists(&sub) {
                        continue;
                    }
                    // Every L2 node's parent is the system, so the file can be
                    // walked first and the module added with what it earned.
                    let mut conf = self.walk_file(&t, &its_children, &sub, depth + 1);
                    if item.cfg_attr_path {
                        conf = conf.min(Confidence::Inferred);
                    }
                    self.add_module(&sub, &t, conf);
                }
                None => {
                    if self.add_module(&sub, file, Confidence::Inferred) {
                        self.diags.push(Diagnostic {
                            path: repo_path(file),
                            message: format!("file for `mod {}` not found", item.name),
                        });
                    }
                }
            }
        }
    }

    fn exists(&self, module: &[String]) -> bool {
        self.model
            .node(&NodeId::within(
                Level::Module,
                self.key.clone(),
                module.to_vec(),
            ))
            .is_some()
    }
}
