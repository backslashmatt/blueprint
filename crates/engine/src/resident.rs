//! A resident engine (DR 0008 decision 5): it keeps the L1–L4 model of a
//! tree, and what built it, so that a change to one file can be applied in
//! place instead of analysing everything again.
//!
//! The fast path covers the edit that happens most: a change inside items.
//! The file's `mod` declarations, items (kind, name and module, in order)
//! and references must be unchanged; then only the file's own nodes and
//! `calls` edges can change, and only those are updated. Any other change
//! rebuilds the whole model.

use std::collections::{BTreeMap, HashMap};

use blueprint_lang_rust::Rust;
use blueprint_model::lang::LanguageAnalyser;
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
    /// The whole model was built again.
    Rebuilt,
}

/// The model of one tree, kept up to date file by file.
pub struct Resident {
    paths: Vec<RepoPath>,
    model: Model,
    diags: Vec<Diagnostic>,
    state: l34::State,
    /// The L2 shape of every analysed Rust file.
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
        let (more, state) = l34::run(&placed, read, &|_| None, &mut |_, _| {}, &mut model);
        diags.extend(more);
        diags.sort();
        let shapes = shapes(&placed, &state, read);
        Resident {
            paths: paths.to_vec(),
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

    /// Applies a change to the file at `path`, which must be one of the
    /// tree's files; `read` reads the tree as it now is. The model is then
    /// what [`Resident::build`] would give for the new tree.
    pub fn update(&mut self, path: &RepoPath, read: Read<'_>) -> Update {
        if self.update_in_place(path, read) {
            Update::InPlace
        } else {
            let paths = std::mem::take(&mut self.paths);
            *self = Resident::build(&paths, read);
            Update::Rebuilt
        }
    }

    fn update_in_place(&mut self, path: &RepoPath, read: Read<'_>) -> bool {
        let Some(old) = self.shapes.get(path) else {
            return false;
        };
        let Some(bytes) = read(path) else {
            return false;
        };
        let shape = l2::shape(&bytes);
        if shape != *old {
            return false;
        }
        let facts = Rust.analyse(path, &bytes);
        l34::update_body(&mut self.model, &mut self.state, path, facts).is_ok()
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
            let shape = l2::shape(&b);
            (p, shape)
        })
        .collect()
}
