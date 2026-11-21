//! A resident engine (DR 0008 decision 5): it keeps the L1–L4 model of a
//! tree, and what built it, so that a change to one file can be applied in
//! place instead of analysing everything again.
//!
//! Two paths apply a change in place; anything else rebuilds the model.
//!
//! - **Inside items** (the edit that happens most): the file's `mod`
//!   declarations, items (kind, name and module, in order) and references
//!   are unchanged, so only its own nodes and `calls` edges change.
//! - **Items or references** (#81): the file's `mod` declarations are
//!   unchanged, and it is not a crate's root file. Its nodes are made again,
//!   and its references re-resolved with those of every file that mentions
//!   a name it added or removed.

use std::collections::{BTreeMap, HashMap};

use blueprint_model::{Model, RepoPath};
use rayon::prelude::*;

use crate::l1::{self, Diagnostic, Read};
use crate::l2::{self, Placement, Shape};
use crate::l34;

/// How [`Resident::update`] applied a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    /// Only the file's own nodes and calls were updated.
    InPlace,
    /// The file's items or references changed: its nodes, and the edges of
    /// the files that mention what changed, were updated.
    Items,
    /// The whole model was built again.
    Rebuilt,
}

/// The model of one tree, kept up to date file by file.
pub struct Resident {
    paths: Vec<RepoPath>,
    model: Model,
    diags: Vec<Diagnostic>,
    state: l34::State,
    /// The L2 shape of every analysed file.
    shapes: HashMap<RepoPath, Shape>,
}

impl Resident {
    /// Builds the L1–L4 model of `paths`, as [`crate::analyse_deep`] does.
    #[must_use]
    pub fn build(paths: &[RepoPath], read: Read<'_>) -> Self {
        let mut model = Model::default();
        let mut diags = l1::analyse(paths, read, &mut model);
        let (more, placed) = l2::analyse_placed(paths, read, &mut model);
        diags.extend(more);
        let (more, state) = l34::run(&placed, read, &|_| None, &mut |_, _| {}, &mut model, true);
        let state = state.unwrap_or_else(|| unreachable!("kept"));
        diags.extend(more);
        diags.sort();
        let shapes = shapes(&placed, &state, read);
        Resident {
            paths: {
                let mut p = paths.to_vec();
                p.sort();
                p.dedup();
                p
            },
            model,
            diags,
            state,
            shapes,
        }
    }

    /// The current model.
    #[must_use]
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// What the analysers could not fully understand, sorted.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diags
    }

    /// Applies a change to the file at `path`, which may have been edited,
    /// created or deleted; `read` reads the tree as it now is, and returns
    /// `None` for a deleted file. The model is then what
    /// [`Resident::build`] would give for the new tree.
    pub fn update(&mut self, path: &RepoPath, read: Read<'_>) -> Update {
        if let Some(how) = self.update_in_place(path, read) {
            return how;
        }
        let mut paths = std::mem::take(&mut self.paths);
        match (paths.binary_search(path), read(path).is_some()) {
            (Err(at), true) => paths.insert(at, path.clone()),
            (Ok(at), false) => {
                paths.remove(at);
            }
            _ => {}
        }
        *self = Resident::build(&paths, read);
        Update::Rebuilt
    }

    fn update_in_place(&mut self, path: &RepoPath, read: Read<'_>) -> Option<Update> {
        let old = self.shapes.get(path)?;
        let bytes = read(path)?;
        if file_shape(path, &bytes) != *old {
            return None;
        }
        let facts = crate::lang::analyser_for(path)?.analyse(path, &bytes);
        match l34::update_body(&mut self.model, &mut self.state, path, facts.clone()) {
            Ok(()) => Some(Update::InPlace),
            Err(l34::NotBody::NotAnalysed) => None,
            Err(l34::NotBody::Structure) => {
                l34::update_items(&mut self.model, &mut self.state, path, facts)
                    .ok()
                    .map(|()| Update::Items)
            }
        }
    }
}

/// The L2 shape of every file the L3–L4 pass analysed.
fn shapes(
    placed: &BTreeMap<RepoPath, Placement>,
    state: &l34::State,
    read: Read<'_>,
) -> HashMap<RepoPath, Shape> {
    let bytes: Vec<(RepoPath, Vec<u8>)> = placed
        .keys()
        .filter(|p| state.facts(p).is_some())
        .filter_map(|p| Some((p.clone(), read(p)?)))
        .collect();
    bytes
        .into_par_iter()
        .map(|(p, b)| {
            let shape = file_shape(&p, &b);
            (p, shape)
        })
        .collect()
}

/// What of a file's contents its L2 placement depends on: a Rust file's
/// `mod` items; nothing for other languages, whose modules are their paths.
fn file_shape(path: &RepoPath, bytes: &[u8]) -> Shape {
    if crate::lang::is_rust(path) {
        l2::shape(bytes)
    } else {
        l2::shape(b"")
    }
}
