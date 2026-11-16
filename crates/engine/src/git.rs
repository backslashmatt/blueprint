//! Reading code at a commit through git's object database (DR 0005).
//!
//! Everything here goes through `gix`; the `git` binary is never run
//! (REQUIREMENTS §5.2). The working tree is never read.

use std::fmt;
use std::path::Path;

use blueprint_model::Confidence;
use gix::bstr::{BString, ByteSlice};
use gix::objs::tree::EntryKind as GixKind;

pub use gix::ObjectId;

/// What a tree entry is. Directories are not listed; their contents are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryKind {
    /// A regular file.
    Blob,
    /// A regular file with the executable bit.
    Executable,
    /// A symbolic link. Its blob holds the target, which is recorded, never followed.
    Symlink,
    /// A submodule (gitlink). Its id names a commit in another repository.
    Submodule,
}

impl EntryKind {
    /// How much Blueprint can know about the entry's contents.
    /// Submodules point outside this repository, so they are `Unknown`.
    #[must_use]
    pub fn confidence(self) -> Confidence {
        match self {
            Self::Submodule => Confidence::Unknown,
            Self::Blob | Self::Executable | Self::Symlink => Confidence::Exact,
        }
    }
}

/// One entry of a commit's tree, flattened to its full path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreeEntry {
    /// The path from the repository root, `/`-separated, as raw bytes:
    /// git does not require paths to be UTF-8.
    pub path: Vec<u8>,
    /// The blob id (a commit id for a submodule). It doubles as the content hash.
    pub id: ObjectId,
    /// What the entry is.
    pub kind: EntryKind,
}

impl TreeEntry {
    /// The path as UTF-8, or `None` if it is not valid UTF-8.
    #[must_use]
    pub fn path_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.path).ok()
    }

    /// The path for display, with invalid bytes replaced.
    #[must_use]
    pub fn path_lossy(&self) -> String {
        self.path.to_str_lossy().into_owned()
    }
}

/// A failure reading the repository.
#[derive(Debug)]
pub enum GitError {
    /// The path is not a git repository, or it could not be opened.
    Open(String),
    /// The revision does not resolve to a commit.
    Resolve {
        /// The revision as given.
        rev: String,
        /// Why it did not resolve.
        reason: String,
    },
    /// An object could not be read or decoded.
    Read(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(e) => write!(f, "cannot open repository: {e}"),
            Self::Resolve { rev, reason } => {
                write!(f, "cannot resolve `{rev}` to a commit: {reason}")
            }
            Self::Read(e) => write!(f, "cannot read object: {e}"),
        }
    }
}

impl std::error::Error for GitError {}

fn read_err(e: impl fmt::Display) -> GitError {
    GitError::Read(e.to_string())
}

/// A repository opened for reading objects.
pub struct Repo {
    inner: gix::Repository,
}

impl fmt::Debug for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Repo")
            .field("git_dir", &self.inner.git_dir())
            .finish()
    }
}

impl Repo {
    /// Opens the repository at `path` (a working tree, or a bare repository).
    /// Repository hooks, filters and other configured programs are never run:
    /// only objects and refs are read.
    pub fn open(path: &Path) -> Result<Self, GitError> {
        let opts = gix::open::Options::isolated();
        let inner = gix::open_opts(path, opts).map_err(|e| GitError::Open(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Resolves a revision (a branch, tag, full or abbreviated id, `HEAD`, or
    /// any rev-parse expression) to a commit id. Annotated tags are peeled.
    pub fn resolve(&self, rev: &str) -> Result<ObjectId, GitError> {
        let err = |reason: String| GitError::Resolve {
            rev: rev.to_owned(),
            reason,
        };
        let id = self
            .inner
            .rev_parse_single(rev)
            .map_err(|e| err(e.to_string()))?;
        let commit = id
            .object()
            .map_err(|e| err(e.to_string()))?
            .peel_to_commit()
            .map_err(|e| err(e.to_string()))?;
        Ok(commit.id)
    }

    /// Lists every file, symlink and submodule in `commit`'s tree, sorted by path.
    pub fn tree(&self, commit: ObjectId) -> Result<Vec<TreeEntry>, GitError> {
        let tree = self
            .inner
            .find_commit(commit)
            .map_err(read_err)?
            .tree()
            .map_err(read_err)?;
        let mut rec = gix::traverse::tree::Recorder::default();
        tree.traverse().breadthfirst(&mut rec).map_err(read_err)?;
        let mut out: Vec<TreeEntry> = rec
            .records
            .into_iter()
            .filter_map(|r| {
                let kind = match r.mode.kind() {
                    GixKind::Tree => return None,
                    GixKind::Blob => EntryKind::Blob,
                    GixKind::BlobExecutable => EntryKind::Executable,
                    GixKind::Link => EntryKind::Symlink,
                    GixKind::Commit => EntryKind::Submodule,
                };
                Some(TreeEntry {
                    path: BString::into(r.filepath),
                    id: r.oid,
                    kind,
                })
            })
            .collect();
        out.sort_unstable();
        Ok(out)
    }

    /// Reads a blob's bytes. For a symlink these are the link target.
    pub fn blob(&self, id: ObjectId) -> Result<Vec<u8>, GitError> {
        let obj = self.inner.find_object(id).map_err(read_err)?;
        if obj.kind != gix::object::Kind::Blob {
            return Err(GitError::Read(format!(
                "{id} is a {}, not a blob",
                obj.kind
            )));
        }
        Ok(obj.detach().data)
    }
}
