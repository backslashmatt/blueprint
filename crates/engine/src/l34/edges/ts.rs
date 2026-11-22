//! Edges from TypeScript and JavaScript references (#139, DR 0012).
//!
//! - **depends-on (L2):** each module specifier the file names.
//!   - A relative one (`./x`, `../y/z`) is the file it names, tried as
//!     written, with each TypeScript or JavaScript extension, as a `.ts` file
//!     for a `.js` specifier (TypeScript's ES-module convention), and as the
//!     directory's `index` file.
//!   - A bare one (`pkg`, `@scope/pkg/sub`) is a package of the repository
//!     with that name, only when this package's manifest declares it (the L1
//!     edge exists); a subpath is a file in it. Packages from outside the
//!     repository, Node's built-ins and `tsconfig` path aliases give no edge.
//!   - `exact` when the imported name is a type or free function of the
//!     target (or the whole module is imported), `inferred` otherwise.
//! - **extends and implements (L3):** `extends` and `implements` naming a class or
//!   interface of the same file, or one imported by name or through a
//!   namespace import.
//! - **calls (L4):** the analyser's call paths: a free function of the same
//!   file, or an imported one.

use std::collections::HashMap;

use blueprint_model::lang::{FileFacts, Reference};
use blueprint_model::{
    Confidence, Ecosystem, EdgeKey, EdgeKind, Level, Model, NodeId, RepoPath, SystemKey,
};

use super::super::{FUNCTIONS, Parsed};
use super::Resolver;

/// Extensions tried after a relative specifier, in order.
const EXTENSIONS: [&str; 9] = ["ts", "tsx", "d.ts", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// What TypeScript resolution needs besides the model: where each analysed
/// TypeScript or JavaScript file sits, and each package by name.
#[derive(Default)]
pub(crate) struct Packages {
    /// Each analysed file's path segments: its system and module.
    files: HashMap<Vec<String>, (SystemKey, Vec<String>)>,
    /// Each npm or Nx system, by its package name.
    by_name: HashMap<String, SystemKey>,
}

/// Whether files of `system` are TypeScript or JavaScript packages.
pub(super) fn is_package(system: &SystemKey) -> bool {
    matches!(system.ecosystem, Ecosystem::Npm | Ecosystem::Nx)
}

impl Packages {
    pub(super) fn build(model: &Model, parsed: &[Parsed<'_>]) -> Packages {
        let files = parsed
            .iter()
            .filter(|(_, place, _, _)| is_package(&place.system))
            .map(|(path, place, module, _)| {
                (
                    path.segments().to_vec(),
                    (place.system.clone(), (*module).clone()),
                )
            })
            .collect();
        let mut by_name = HashMap::new();
        for n in model.level(Level::System) {
            let key = n.id.system_key();
            if is_package(key) {
                // The first by key keeps a name two packages share.
                by_name.entry(n.name.clone()).or_insert_with(|| key.clone());
            }
        }
        Packages { files, by_name }
    }

    /// The analysed file a path names, tried as [`EXTENSIONS`] describe.
    fn file(&self, base: &[String]) -> Option<&(SystemKey, Vec<String>)> {
        let (last, dir) = base.split_last()?;
        if let Some(found) = self.files.get(base) {
            return Some(found);
        }
        let with = |name: String| [dir, &[name]].concat();
        for ext in EXTENSIONS {
            if let Some(found) = self.files.get(&with(format!("{last}.{ext}"))) {
                return Some(found);
            }
        }
        // `./x.js` written for `x.ts`.
        for (js, ts) in [
            ("js", "ts"),
            ("js", "tsx"),
            ("jsx", "tsx"),
            ("mjs", "mts"),
            ("cjs", "cts"),
        ] {
            if let Some(stem) = last.strip_suffix(&format!(".{js}"))
                && let Some(found) = self.files.get(&with(format!("{stem}.{ts}")))
            {
                return Some(found);
            }
        }
        for ext in EXTENSIONS {
            let index = [base, &[format!("index.{ext}")]].concat();
            if let Some(found) = self.files.get(&index) {
                return Some(found);
            }
        }
        None
    }

    /// The module `spec` names from the file at `from` in `system`, and
    /// whether that module is a file (not a package with no entry file).
    fn resolve(
        &self,
        model: &Model,
        from: &RepoPath,
        system: &SystemKey,
        spec: &str,
    ) -> Option<(SystemKey, Vec<String>, bool)> {
        if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
            let mut base: Vec<String> = from.segments().split_last()?.1.to_vec();
            for part in spec.split('/') {
                match part {
                    "" | "." => {}
                    ".." => {
                        base.pop()?;
                    }
                    p => base.push(p.to_owned()),
                }
            }
            let (s, m) = self.file(&base)?;
            return Some((s.clone(), m.clone(), true));
        }
        let parts: Vec<&str> = spec.split('/').collect();
        let n = if spec.starts_with('@') { 2 } else { 1 };
        if parts.len() < n {
            return None;
        }
        let target = self.by_name.get(&parts[..n].join("/"))?;
        let declared = model
            .edge(&EdgeKey {
                from: NodeId::system(system.clone()),
                to: NodeId::system(target.clone()),
                kind: EdgeKind::DependsOn,
            })
            .is_some();
        if !declared && target != system {
            return None;
        }
        let sub: Vec<String> = parts[n..].iter().map(|s| (*s).to_owned()).collect();
        let base = [target.dir.segments(), sub.as_slice()].concat();
        match self.file(&base) {
            Some((s, m)) => Some((s.clone(), m.clone(), true)),
            // The package itself, with no entry file this pass reads.
            None if sub.is_empty() => Some((target.clone(), Vec::new(), false)),
            None => None,
        }
    }
}

/// The names a file's imports bind: name -> `[specifier, imported]`, or
/// `[specifier, "*"]` for a namespace import.
fn imports(facts: &FileFacts) -> HashMap<&str, &[String]> {
    let mut out = HashMap::new();
    for r in &facts.references {
        if let Reference::Use { path, alias, .. } = r
            && path.len() == 2
        {
            let bound = alias.as_deref().or_else(|| path.last().map(String::as_str));
            if let Some(b) = bound.filter(|b| *b != "*") {
                out.insert(b, path.as_slice());
            }
        }
    }
    out
}

impl Resolver<'_> {
    /// One TypeScript or JavaScript file's direct edges.
    pub(super) fn ts_file_edges<'n>(
        &self,
        path: &RepoPath,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<(EdgeKey, Confidence)> {
        let mut found: Vec<(EdgeKey, Confidence)> = self
            .ts_file_calls(path, system, file_module, facts, node)
            .into_iter()
            .map(|k| (k, Confidence::Inferred))
            .collect();
        let from = NodeId::within(Level::Module, system.clone(), file_module.to_vec());
        let bound = imports(facts);
        for r in &facts.references {
            match r {
                Reference::Use { path: p, .. } => {
                    let Some((spec, name)) = p.split_first() else {
                        continue;
                    };
                    let Some((to_sys, to_mod, is_file)) =
                        self.packages.resolve(self.model, path, system, spec)
                    else {
                        continue;
                    };
                    let exact = is_file
                        && match name.first().map(String::as_str) {
                            None | Some("*") => true,
                            Some(n) => self.has_item(&to_sys, &to_mod, n),
                        };
                    let key = EdgeKey {
                        from: from.clone(),
                        to: NodeId::within(Level::Module, to_sys, to_mod),
                        kind: EdgeKind::DependsOn,
                    };
                    let conf = if exact {
                        Confidence::Exact
                    } else {
                        Confidence::Inferred
                    };
                    found.push((key, conf));
                }
                Reference::Impl {
                    self_type,
                    r#trait,
                    extends,
                    ..
                } => {
                    let [ty] = self_type.as_slice() else {
                        continue;
                    };
                    let ty_id = NodeId::within(
                        Level::Component,
                        system.clone(),
                        [file_module, std::slice::from_ref(ty)].concat(),
                    );
                    if self.model.node(&ty_id).is_none() {
                        continue;
                    }
                    let target = match r#trait.as_slice() {
                        [name] if self.has_item(system, file_module, name) => {
                            Some((system.clone(), file_module.to_vec(), name.clone()))
                        }
                        [name] => bound.get(name.as_str()).and_then(|imp| {
                            let (spec, imported) = (imp.first()?, imp.get(1)?);
                            let (s, m, _) =
                                self.packages.resolve(self.model, path, system, spec)?;
                            (imported != "*").then(|| (s, m, imported.clone()))
                        }),
                        [ns, name] => bound.get(ns.as_str()).and_then(|imp| {
                            let (spec, star) = (imp.first()?, imp.get(1)?);
                            let (s, m, _) =
                                self.packages.resolve(self.model, path, system, spec)?;
                            (star == "*").then(|| (s, m, name.clone()))
                        }),
                        _ => None,
                    };
                    if let Some((s, m, name)) = target {
                        let to = NodeId::within(Level::Component, s, [m, vec![name]].concat());
                        if self.model.node(&to).is_some() {
                            let key = EdgeKey {
                                from: ty_id,
                                to,
                                kind: if *extends {
                                    EdgeKind::Extends
                                } else {
                                    EdgeKind::Implements
                                },
                            };
                            found.push((key, Confidence::Exact));
                        }
                    }
                }
            }
        }
        found
    }

    /// The `calls` edges of one TypeScript or JavaScript file's functions.
    pub(super) fn ts_file_calls<'n>(
        &self,
        path: &RepoPath,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<EdgeKey> {
        let function = |s: SystemKey, m: Vec<String>, name: &str| {
            let id = NodeId::within(
                Level::Code,
                s,
                [m, vec![FUNCTIONS.to_owned(), name.to_owned()]].concat(),
            );
            self.model.node(&id).is_some().then_some(id)
        };
        let mut out = Vec::new();
        for (i, item) in facts.items.iter().enumerate() {
            let Some(caller) = node(i) else {
                continue;
            };
            for call in &item.calls {
                let callee = match call.as_slice() {
                    [name] => function(system.clone(), file_module.to_vec(), name),
                    [spec, name] => self
                        .packages
                        .resolve(self.model, path, system, spec)
                        .and_then(|(s, m, _)| function(s, m, name)),
                    _ => None,
                };
                if let Some(callee) = callee
                    && callee != *caller
                {
                    out.push(EdgeKey {
                        from: caller.clone(),
                        to: callee,
                        kind: EdgeKind::Calls,
                    });
                }
            }
        }
        out
    }
}
