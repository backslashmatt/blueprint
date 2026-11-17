//! The persisted index (DR 0005, DR 0007): the L1–L2 model of each indexed
//! tree, stored in the platform cache so that reopening a repository does not
//! analyse it again.
//!
//! An entry is keyed by the commit's root tree id and [`ANALYSER_VERSION`].
//! The tree id is a hash of every file in the commit, so an entry is correct
//! for any commit, in any repository, that has the same tree. Nothing needs
//! to be invalidated when a repository changes; a new tree is simply a new
//! key. A damaged or foreign index is dropped and rebuilt (§5.5).

use std::path::{Path, PathBuf};

use blueprint_model::{Model, RepoPath};
use blueprint_store::{KEY_LEN, Key, SegmentWriter, Store, StoreError};
use serde::{Deserialize, Serialize};

use crate::git::ObjectId;
use crate::l1::Diagnostic;

/// The version of every analyser's output. Bump it whenever any analyser
/// would produce a different model for the same files, so that indexes
/// written by older code are not used.
pub const ANALYSER_VERSION: u32 = 1;

/// Overrides the cache directory, for tests and for users who keep caches
/// elsewhere.
pub const CACHE_DIR_ENV: &str = "BLUEPRINT_CACHE_DIR";

/// The store is compacted once it holds more segments than this.
const MAX_SEGMENTS: usize = 16;

/// What kind of value a key holds (byte 24 of the key).
const KIND_L1_L2_MODEL: u8 = 1;

/// What the index holds for one tree: the model, and what the analysers
/// could not fully understand when they built it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Indexed {
    /// The L1–L2 model.
    pub model: Model,
    /// The analysers' diagnostics, as `(path, message)`.
    pub diagnostics: Vec<(RepoPath, String)>,
}

impl Indexed {
    /// Bundles a model with the diagnostics produced while building it.
    #[must_use]
    pub fn new(model: Model, diagnostics: &[Diagnostic]) -> Self {
        Self {
            model,
            diagnostics: diagnostics
                .iter()
                .map(|d| (d.path.clone(), d.message.clone()))
                .collect(),
        }
    }

    /// The diagnostics as [`Diagnostic`]s.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .map(|(path, message)| Diagnostic {
                path: path.clone(),
                message: message.clone(),
            })
            .collect()
    }
}

/// The index of one repository.
#[derive(Debug)]
pub struct Index {
    store: Store,
}

impl Index {
    /// Where the index of the repository at `repo` lives:
    /// `<cache>/index/<hash of the canonical path>`. `<cache>` is
    /// `$BLUEPRINT_CACHE_DIR` if set, otherwise the platform cache directory
    /// (`~/.cache/blueprint` on Linux).
    ///
    /// # Errors
    /// If `repo` cannot be canonicalised or there is no cache directory.
    pub fn location(repo: &Path) -> Result<PathBuf, String> {
        let canonical = repo
            .canonicalize()
            .map_err(|e| format!("{}: {e}", repo.display()))?;
        let root = match std::env::var_os(CACHE_DIR_ENV) {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => directories::ProjectDirs::from("", "", "blueprint")
                .map(|d| d.cache_dir().to_path_buf())
                .ok_or("no cache directory: set BLUEPRINT_CACHE_DIR")?,
        };
        let name = format!("{:016x}", fnv1a(canonical.as_os_str().as_encoded_bytes()));
        Ok(root.join("index").join(name))
    }

    /// Opens (creating if needed) the index of the repository at `repo`.
    /// Damaged segments are dropped, never fatal; see [`Index::dropped`].
    ///
    /// # Errors
    /// If the location cannot be worked out or the directory cannot be
    /// created or listed.
    pub fn open(repo: &Path) -> Result<Self, String> {
        let dir = Self::location(repo)?;
        Self::open_at(&dir).map_err(|e| e.to_string())
    }

    /// Opens the index of the repository at `repo` for reading only: `None`
    /// if there is none yet, and nothing is created. Damaged segments are
    /// still dropped, as by [`Index::open`].
    ///
    /// # Errors
    /// As [`Index::open`].
    pub fn open_existing(repo: &Path) -> Result<Option<Self>, String> {
        let dir = Self::location(repo)?;
        if !dir.is_dir() {
            return Ok(None);
        }
        Self::open_at(&dir).map(Some).map_err(|e| e.to_string())
    }

    /// Opens (creating if needed) an index in `dir`.
    ///
    /// # Errors
    /// [`StoreError::Io`] if the directory cannot be created or listed.
    pub fn open_at(dir: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            store: Store::open(dir)?,
        })
    }

    /// Why each discarded segment was dropped when the index was opened.
    #[must_use]
    pub fn dropped(&self) -> Vec<String> {
        self.store
            .dropped()
            .iter()
            .map(|d| format!("{}: {}", d.path.display(), d.reason))
            .collect()
    }

    /// The model stored for `tree`, if any. An entry that does not decode is
    /// treated as absent, so it is rebuilt.
    #[must_use]
    pub fn get(&self, tree: ObjectId) -> Option<Indexed> {
        let bytes = self.store.get(&key(tree)?)?;
        serde_json::from_slice(bytes).ok()
    }

    /// Stores `indexed` for `tree`, and returns whether it was stored. Trees
    /// whose ids are not SHA-1 are not; they are analysed every time.
    ///
    /// When the store has too many segments it is compacted, and entries
    /// written by another [`ANALYSER_VERSION`] are dropped then.
    ///
    /// # Errors
    /// If the segment cannot be written.
    pub fn put(&mut self, tree: ObjectId, indexed: &Indexed) -> Result<bool, String> {
        let Some(key) = key(tree) else {
            return Ok(false);
        };
        let value = serde_json::to_vec(indexed).map_err(|e| e.to_string())?;
        let mut writer = SegmentWriter::new();
        writer.insert(key, value);
        self.store.add(&writer).map_err(|e| e.to_string())?;
        if self.store.segment_count() > MAX_SEGMENTS {
            self.store
                .compact_retain(current_version)
                .map_err(|e| e.to_string())?;
        }
        Ok(true)
    }
}

/// The key for `tree`'s L1–L2 model: the 20-byte tree id, the analyser
/// version, then the kind. `None` for ids of another length.
fn key(tree: ObjectId) -> Option<Key> {
    let id = tree.as_bytes();
    if id.len() != 20 {
        return None;
    }
    let mut k = [0u8; KEY_LEN];
    k[..20].copy_from_slice(id);
    k[20..24].copy_from_slice(&ANALYSER_VERSION.to_le_bytes());
    k[24] = KIND_L1_L2_MODEL;
    Some(Key(k))
}

/// Whether `key` was written by this [`ANALYSER_VERSION`].
fn current_version(key: &Key) -> bool {
    key.0[20..24] == ANALYSER_VERSION.to_le_bytes()
}

/// 64-bit FNV-1a: a stable directory name for a path. Collisions only cost
/// sharing a cache, which is safe because entries are content-addressed.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_hold_the_tree_the_version_and_the_kind() {
        let tree = ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").ok();
        let k = tree.and_then(key).map(|k| k.0);
        assert_eq!(k.map(|k| k[0]), Some(0x01));
        assert_eq!(k.map(|k| k[19]), Some(0x67));
        assert_eq!(
            k.map(|k| k[20..24].to_vec()),
            Some(ANALYSER_VERSION.to_le_bytes().to_vec())
        );
        assert_eq!(k.map(|k| k[24]), Some(KIND_L1_L2_MODEL));
    }

    #[test]
    fn directory_names_are_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
