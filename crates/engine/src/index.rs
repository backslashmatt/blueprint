//! The persisted index (DR 0005, DR 0007, DR 0008), stored in the platform
//! cache so that reopening a repository does not analyse it again. It holds:
//!
//! - the L1–L2 model of each indexed tree, keyed by the commit's root tree
//!   id and [`ANALYSER_VERSION`];
//! - each language analyser's facts for each file, keyed by the file's blob
//!   id and that analyser's name and version, so an unchanged file is never
//!   parsed twice, whichever commit it is in.
//!
//! Both ids are hashes of content, so an entry is correct for any commit, in
//! any repository, with the same tree or blob. Nothing needs to be
//! invalidated when a repository changes; new content is simply a new key. A
//! damaged or foreign index is dropped and rebuilt (§5.5).

use std::path::{Path, PathBuf};

use blueprint_lang_rust::Rust;
use blueprint_model::lang::{FileFacts, LanguageAnalyser};
use blueprint_model::{Model, RepoPath, codec};
use blueprint_store::{KEY_LEN, Key, SegmentWriter, Store, StoreError};
use serde::{Deserialize, Serialize};

use crate::git::ObjectId;
use crate::l1::Diagnostic;

/// The version of every analyser's output. Bump it whenever any analyser
/// would produce a different model for the same files, so that indexes
/// written by older code are not used.
pub const ANALYSER_VERSION: u32 = 2;

/// Overrides the cache directory, for tests and for users who keep caches
/// elsewhere.
pub const CACHE_DIR_ENV: &str = "BLUEPRINT_CACHE_DIR";

static CACHE_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Sets the cache directory for the rest of the process, over
/// `$BLUEPRINT_CACHE_DIR` (see [`Index::location`]): for a front end with
/// its own setting, and for tests, which cannot set the environment safely
/// while other threads run. Returns `false`, changing nothing, if it was
/// already set to another directory.
pub fn set_cache_root(dir: PathBuf) -> bool {
    CACHE_ROOT.get_or_init(|| dir.clone()) == &dir
}

/// The store is compacted once it holds more segments than this.
const MAX_SEGMENTS: usize = 16;

/// What kind of value a key holds (byte 24 of the key).
const KIND_L1_L2_MODEL: u8 = 1;
/// A language analyser's [`FileFacts`] for one blob. Bytes 25..31 of the
/// key hold the start of the analyser's name, and byte 31 [`FACTS_FORMAT`].
const KIND_FACTS: u8 = 2;

/// The layout of [`FileFacts`] itself. Bump it whenever `FileFacts` or what
/// it holds changes shape, so stored facts that would still decode, with
/// defaults for new fields, are not reused.
const FACTS_FORMAT: u8 = 1;

/// A tree's whole L1–L4 model (DR 0009). Bytes 25..29 of the key hold the
/// Rust analyser's version and byte 29 [`codec::FORMAT`], since all three
/// shape it.
const KIND_L4_MODEL: u8 = 3;

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
    /// `<cache>/index/<hash of the canonical path>`. `<cache>` is the
    /// directory given to [`set_cache_root`] if any, else
    /// `$BLUEPRINT_CACHE_DIR` if set, otherwise the platform cache directory
    /// (`~/.cache/blueprint` on Linux).
    ///
    /// # Errors
    /// If `repo` cannot be canonicalised or there is no cache directory.
    pub fn location(repo: &Path) -> Result<PathBuf, String> {
        let canonical = repo
            .canonicalize()
            .map_err(|e| format!("{}: {e}", repo.display()))?;
        let root = match (CACHE_ROOT.get(), std::env::var_os(CACHE_DIR_ENV)) {
            (Some(dir), _) => dir.clone(),
            (None, Some(dir)) if !dir.is_empty() => PathBuf::from(dir),
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
        self.add(&writer)?;
        Ok(true)
    }
}

impl Index {
    /// The L1–L4 model stored for `tree`, with its diagnostics, if any. An
    /// entry that does not decode is treated as absent, so it is rebuilt.
    #[must_use]
    pub fn get_l4(&self, tree: ObjectId) -> Option<(Model, Vec<Diagnostic>)> {
        let bytes = self.store.get(&l4_key(tree)?)?;
        let (len, rest) = bytes.split_first_chunk::<4>()?;
        let len = usize::try_from(u32::from_le_bytes(*len)).ok()?;
        let (diags, model) = (rest.get(..len)?, rest.get(len..)?);
        let diags: Vec<(RepoPath, String)> = serde_json::from_slice(diags).ok()?;
        let model = codec::decode(model).ok()?;
        let diags = diags
            .into_iter()
            .map(|(path, message)| Diagnostic { path, message })
            .collect();
        Some((model, diags))
    }

    /// Stores the L1–L4 `model` of `tree` and its diagnostics, and returns
    /// whether it was stored (not for ids that are not SHA-1).
    ///
    /// # Errors
    /// If the segment cannot be written.
    pub fn put_l4(
        &mut self,
        tree: ObjectId,
        model: &Model,
        diagnostics: &[Diagnostic],
    ) -> Result<bool, String> {
        let Some(key) = l4_key(tree) else {
            return Ok(false);
        };
        let diags: Vec<(&RepoPath, &String)> =
            diagnostics.iter().map(|d| (&d.path, &d.message)).collect();
        let diags = serde_json::to_vec(&diags).map_err(|e| e.to_string())?;
        let len = u32::try_from(diags.len()).map_err(|e| e.to_string())?;
        let mut value = len.to_le_bytes().to_vec();
        value.extend_from_slice(&diags);
        value.extend_from_slice(&codec::encode(model));
        let mut writer = SegmentWriter::new();
        writer.insert(key, value);
        self.add(&writer)?;
        Ok(true)
    }

    /// `analyser`'s stored facts for the file whose blob is `blob`, if any.
    /// An entry that does not decode is treated as absent, so it is re-parsed.
    #[must_use]
    pub fn get_facts(&self, analyser: &dyn LanguageAnalyser, blob: ObjectId) -> Option<FileFacts> {
        let bytes = self.store.get(&facts_key(analyser, blob)?)?;
        serde_json::from_slice(bytes).ok()
    }

    /// Stores `analyser`'s facts for each blob, in one segment, and returns
    /// how many were stored. Blobs whose ids are not SHA-1 are skipped.
    ///
    /// # Errors
    /// If the segment cannot be written.
    pub fn put_facts<'f>(
        &mut self,
        analyser: &dyn LanguageAnalyser,
        facts: impl IntoIterator<Item = (ObjectId, &'f FileFacts)>,
    ) -> Result<usize, String> {
        let mut writer = SegmentWriter::new();
        for (blob, facts) in facts {
            if let Some(key) = facts_key(analyser, blob) {
                writer.insert(key, serde_json::to_vec(facts).map_err(|e| e.to_string())?);
            }
        }
        let stored = writer.len();
        if stored > 0 {
            self.add(&writer)?;
        }
        Ok(stored)
    }

    /// Adds a segment, compacting the store when it has too many. Entries
    /// written by another version of their analyser are dropped then.
    fn add(&mut self, writer: &SegmentWriter) -> Result<(), String> {
        self.store.add(writer).map_err(|e| e.to_string())?;
        if self.store.segment_count() > MAX_SEGMENTS {
            self.store
                .compact_retain(current_version)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// The key for `analyser`'s facts of `blob`: the 20-byte blob id,
/// `analyser`'s version, the kind, up to 6 bytes of its name, then
/// [`FACTS_FORMAT`]. `None` for ids of another length.
fn facts_key(analyser: &dyn LanguageAnalyser, blob: ObjectId) -> Option<Key> {
    let id = blob.as_bytes();
    if id.len() != 20 {
        return None;
    }
    let mut k = [0u8; KEY_LEN];
    k[..20].copy_from_slice(id);
    k[20..24].copy_from_slice(&analyser.version().to_le_bytes());
    k[24] = KIND_FACTS;
    k[25..31].copy_from_slice(&name_bytes(analyser));
    k[31] = FACTS_FORMAT;
    Some(Key(k))
}

/// The first 6 bytes of `analyser`'s name, zero-padded. Analysers' names
/// must differ within them.
fn name_bytes(analyser: &dyn LanguageAnalyser) -> [u8; 6] {
    let mut out = [0u8; 6];
    let name = analyser.name().as_bytes();
    let n = name.len().min(out.len());
    out[..n].copy_from_slice(&name[..n]);
    out
}

/// The key for `tree`'s L1–L4 model: the tree id, [`ANALYSER_VERSION`], the
/// kind, the Rust analyser's version and the codec's format.
fn l4_key(tree: ObjectId) -> Option<Key> {
    let mut k = key(tree)?;
    k.0[20..].copy_from_slice(&l4_tail());
    Some(k)
}

/// Bytes 20.. of every current L1–L4 model key.
fn l4_tail() -> [u8; KEY_LEN - 20] {
    let mut t = [0u8; KEY_LEN - 20];
    t[..4].copy_from_slice(&ANALYSER_VERSION.to_le_bytes());
    t[4] = KIND_L4_MODEL;
    t[5..9].copy_from_slice(&Rust.version().to_le_bytes());
    t[9..].copy_from_slice(&codec::FORMAT.to_le_bytes()[..KEY_LEN - 29]);
    t
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

/// Whether `key` was written by the current version of what wrote it.
fn current_version(key: &Key) -> bool {
    match key.0[24] {
        KIND_L1_L2_MODEL => key.0[20..24] == ANALYSER_VERSION.to_le_bytes(),
        KIND_L4_MODEL => key.0[20..] == l4_tail(),
        // Rust is the only language analyser so far; facts of any other
        // name are from a version that no longer exists.
        KIND_FACTS => {
            key.0[20..24] == Rust.version().to_le_bytes()
                && key.0[25..31] == name_bytes(&Rust)
                && key.0[31] == FACTS_FORMAT
        }
        _ => false,
    }
}

/// 64-bit FNV-1a: a stable directory name for a path. Collisions only cost
/// sharing a cache, which is safe because entries are content-addressed.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
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
    fn facts_keys_hold_the_blob_the_version_the_kind_and_the_name() {
        let blob = ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").ok();
        let k = blob.and_then(|b| facts_key(&Rust, b));
        let Some(k) = k else {
            panic!("a SHA-1 blob has a key");
        };
        assert_eq!(k.0[19], 0x67);
        assert_eq!(k.0[20..24], Rust.version().to_le_bytes());
        assert_eq!(k.0[24], KIND_FACTS);
        assert_eq!(&k.0[25..29], b"rust");
        assert_eq!(k.0[29..31], [0, 0]);
        assert_eq!(k.0[31], FACTS_FORMAT);
        assert!(current_version(&k), "kept by compaction");
        let mut old = k;
        old.0[20..24].copy_from_slice(&(Rust.version() - 1).to_le_bytes());
        assert!(!current_version(&old), "dropped by compaction");
        let mut other = k;
        other.0[25] = b'g';
        assert!(!current_version(&other), "no other analyser yet");
        let mut format = k;
        format.0[31] = FACTS_FORMAT + 1;
        assert!(!current_version(&format), "another facts layout");
    }

    #[test]
    fn stored_facts_are_read_back_by_blob() {
        let dir = std::env::temp_dir().join(format!("bp-index-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut index = Index::open_at(&dir).unwrap_or_else(|e| panic!("{e}"));
        let a = ObjectId::from_hex(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").ok();
        let b = ObjectId::from_hex(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").ok();
        let (Some(a), Some(b)) = (a, b) else {
            panic!("valid ids");
        };
        let facts = blueprint_lang_rust::Rust.analyse(
            &RepoPath::parse("lib.rs").unwrap_or_else(|_| RepoPath::root()),
            b"pub fn f() {}",
        );
        assert_eq!(index.put_facts(&Rust, [(a, &facts)]), Ok(1));
        assert_eq!(index.get_facts(&Rust, a), Some(facts));
        assert_eq!(index.get_facts(&Rust, b), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_names_are_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
