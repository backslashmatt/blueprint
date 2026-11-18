//! L2 for Rust: the module tree of a Cargo package.
//!
//! **Crate roots** come from the package's `Cargo.toml`: `[lib] path` (else
//! `src/lib.rs`), each `[[bin]] path`, and, unless `autobins = false`,
//! `src/main.rs`, `src/bin/*.rs` and `src/bin/*/main.rs`. Each root file is
//! read by a shallow lexer for its `mod` items only (DR 0006); every
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
//!   read up to the damage.
//!
//! Recursion is bounded by [`MAX_DEPTH`] and the work per crate by
//! [`MAX_FILE_WALKS`], so hostile input cannot exhaust the stack or time.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey, Visibility,
};
use toml::{Table, Value};

use super::lexer::{ModItem, scan};
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
    /// Files on the current inclusion chain, to stop `#[path]` cycles.
    stack: Vec<Vec<String>>,
    walks: usize,
    /// Every file walked (what the module tree reaches), with the module it
    /// is. A file included twice keeps the first module.
    reached: BTreeMap<RepoPath, Vec<String>>,
    model: &'a mut Model,
    diags: &'a mut Vec<Diagnostic>,
}

/// Adds the module tree of the package `system` to `model`, and returns the
/// files it reached, each with the module path it is; `None` if the package
/// has no Rust crate root, so the caller can fall back to directories.
pub fn analyse_crate(
    system: &NodeId,
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    read: Read<'_>,
    model: &mut Model,
    diags: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<RepoPath, Vec<String>>> {
    let roots = crate_roots(key, files, read);
    if roots.is_empty() {
        return None;
    }
    let mut ctx = Ctx {
        system,
        key,
        files,
        read,
        stack: Vec::new(),
        walks: 0,
        reached: BTreeMap::new(),
        model,
        diags,
    };
    for (root, module) in roots {
        let dir = root[..root.len() - 1].to_vec();
        // The crate itself is an L2 module too, so its own items have a
        // parent: named for the package at the top, `(bin name)` below.
        let name = match module.first() {
            Some(bin) => bin.clone(),
            None => ctx
                .model
                .node(system)
                .map_or_else(|| "crate".to_owned(), |n| n.name.clone()),
        };
        let conf = ctx.walk_file(&root, &dir, &module, 0);
        ctx.add_root(&module, &name, &root, conf);
    }
    Some(ctx.reached)
}

/// The prefix of a binary's modules when the package has a primary crate
/// (see [`crate_roots`]): `(bin name)`.
pub fn bin_segment(name: &str) -> String {
    format!("(bin {name})")
}

/// The package's crate root files that exist, without duplicates, each with
/// the module path its modules sit under. The primary crate — the library,
/// else `src/main.rs`, else the only root — is at the top (`[]`); every
/// other binary gets its own `(bin <name>)` namespace, so binaries never
/// share ids and adding one never renames another's items.
fn crate_roots(
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    read: Read<'_>,
) -> Vec<(Vec<String>, Vec<String>)> {
    let dir = key.dir.segments();
    let mut manifest_path = dir.to_vec();
    manifest_path.push("Cargo.toml".into());
    let manifest: Table = read(&repo_path(&manifest_path))
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();
    // (relative path, binary name; `None` for the library)
    let mut rels: Vec<(String, Option<String>)> = Vec::new();
    let lib = manifest
        .get("lib")
        .and_then(|l| l.get("path"))
        .and_then(Value::as_str);
    rels.push((lib.unwrap_or("src/lib.rs").to_owned(), None));
    for bin in manifest
        .get("bin")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(p) = bin.get("path").and_then(Value::as_str) {
            let name = bin
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| stem(p));
            rels.push((p.to_owned(), Some(name)));
        }
    }
    let autobins = manifest
        .get("package")
        .and_then(|p| p.get("autobins"))
        .and_then(Value::as_bool)
        != Some(false);
    if autobins {
        rels.push(("src/main.rs".into(), Some("main".into())));
        let mut bin_dir = dir.to_vec();
        bin_dir.extend(["src".to_owned(), "bin".to_owned()]);
        for f in files.iter().filter(|f| f.segments().starts_with(&bin_dir)) {
            match &f.segments()[bin_dir.len()..] {
                [file] if file.ends_with(".rs") => {
                    rels.push((format!("src/bin/{file}"), Some(stem(file))));
                }
                [sub, main] if main == "main.rs" => {
                    rels.push((format!("src/bin/{sub}/main.rs"), Some(sub.clone())));
                }
                _ => {}
            }
        }
    }
    let mut found: Vec<(Vec<String>, Option<String>)> = Vec::new();
    for (rel, name) in rels {
        if let Some(p) = normalise(dir, &rel)
            && files.contains(&repo_path(&p))
            && !found.iter().any(|(f, _)| *f == p)
        {
            found.push((p, name));
        }
    }
    let primary = found
        .iter()
        .position(|(_, n)| n.is_none())
        .or_else(|| {
            found
                .iter()
                .position(|(f, _)| f.ends_with(&["src".into(), "main.rs".into()]))
        })
        .or_else(|| (found.len() == 1).then_some(0));
    found
        .into_iter()
        .enumerate()
        .map(|(i, (file, name))| {
            let module = if Some(i) == primary {
                Vec::new()
            } else {
                vec![bin_segment(name.as_deref().unwrap_or("main"))]
            };
            (file, module)
        })
        .collect()
}

/// `src/bin/tool.rs` gives `tool`.
fn stem(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".rs").unwrap_or(file).to_owned()
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
            code: None,
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

    /// Adds the L2 node of a crate root, unless another root added it.
    fn add_root(
        &mut self,
        module: &[String],
        name: &str,
        source: &[String],
        confidence: Confidence,
    ) {
        let id = self.id(module);
        if self.model.node(&id).is_some() {
            return;
        }
        let node = Node {
            id,
            kind: NodeKind::Module,
            name: name.to_owned(),
            source: repo_path(source),
            parent: Some(self.system.clone()),
            confidence,
            visibility: Some(Visibility::Public),
            code: None,
        };
        if let Err(e) = self.model.add_node(node) {
            self.diags.push(Diagnostic {
                path: repo_path(source),
                message: e.to_string(),
            });
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
        self.reached
            .entry(path.clone())
            .or_insert_with(|| module.to_vec());
        let Some(bytes) = (self.read)(&path) else {
            self.diag(file, "cannot be read");
            return Confidence::Unknown;
        };
        let text = String::from_utf8_lossy(&bytes);
        let found = scan(&text);
        let conf = if found.clean && std::str::from_utf8(&bytes).is_ok() {
            Confidence::Exact
        } else {
            self.diag(file, "unbalanced brackets, an unterminated comment or string, or not UTF-8; modules read up to the damage");
            Confidence::Unknown
        };
        let parent_dir = file[..file.len() - 1].to_vec();
        self.stack.push(file.to_vec());
        self.walk_items(
            &found.items,
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
        items: &[ModItem],
        file: &[String],
        file_conf: Confidence,
        path_base: &[String],
        child_dir: &[String],
        module: &[String],
        depth: usize,
    ) {
        for item in items {
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
            if let Some(body) = &item.body {
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
                    self.walk_items(body, file, file_conf, &inner, &inner, &sub, depth + 1);
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
