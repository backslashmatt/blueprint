//! Reading code at a commit through git's object database (DR 0005).
//!
//! Everything here goes through `gix`; the `git` binary is never run
//! (REQUIREMENTS §5.2). The working tree is never read.

use std::fmt;
use std::path::Path;

use blueprint_model::Confidence;
use gix::bstr::ByteSlice;

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

/// The most entries [`Repo::tree`] reads, counting directories. Far above any
/// real repository (rust-lang/rust has about 65,000 files), it stops a small
/// repository whose trees share subtrees from expanding without limit.
pub const MAX_TREE_ENTRIES: usize = 5_000_000;

/// Whether git would accept `name` as a tree entry name (as `git fsck` does).
fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0)
}

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

    /// The id of `commit`'s root tree: a hash of every file in the commit.
    pub fn tree_id(&self, commit: ObjectId) -> Result<ObjectId, GitError> {
        Ok(self
            .inner
            .find_commit(commit)
            .map_err(read_err)?
            .tree_id()
            .map_err(read_err)?
            .detach())
    }

    /// Lists every file, symlink and submodule in `commit`'s tree, sorted by path.
    ///
    /// Trees are read breadth first with an explicit queue, so deep nesting
    /// cannot overflow the stack. An entry whose name git itself would reject
    /// (empty, `.`, `..`, or containing `/` or NUL), or whose mode is not one
    /// git writes, is an error rather than a guess. So is a tree with more than
    /// [`MAX_TREE_ENTRIES`] entries, which bounds memory on hostile repositories
    /// whose trees share subtrees.
    pub fn tree(&self, commit: ObjectId) -> Result<Vec<TreeEntry>, GitError> {
        self.tree_limited(commit, MAX_TREE_ENTRIES)
    }

    /// [`Repo::tree`] with a different entry limit (counting directories).
    pub fn tree_limited(&self, commit: ObjectId, limit: usize) -> Result<Vec<TreeEntry>, GitError> {
        let root = self
            .inner
            .find_commit(commit)
            .map_err(read_err)?
            .tree_id()
            .map_err(read_err)?
            .detach();
        let mut out = Vec::new();
        let mut seen = 0usize;
        let mut queue = std::collections::VecDeque::from([(Vec::<u8>::new(), root)]);
        while let Some((prefix, id)) = queue.pop_front() {
            let tree = self.inner.find_tree(id).map_err(read_err)?;
            let decoded = tree.decode().map_err(read_err)?;
            for entry in decoded.entries {
                seen += 1;
                if seen > limit {
                    return Err(GitError::Read(format!(
                        "tree of {commit} has more than {limit} entries"
                    )));
                }
                let name: &[u8] = entry.filename;
                if !valid_name(name) {
                    return Err(GitError::Read(format!(
                        "tree {id} has an invalid entry name {:?}",
                        name.as_bstr()
                    )));
                }
                let mut path = Vec::with_capacity(prefix.len() + 1 + name.len());
                path.extend_from_slice(&prefix);
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(name);
                let oid = entry.oid.to_owned();
                let kind = match entry.mode.value() {
                    0o40000 => {
                        queue.push_back((path, oid));
                        continue;
                    }
                    0o100644 | 0o100664 => EntryKind::Blob,
                    0o100755 => EntryKind::Executable,
                    0o120000 => EntryKind::Symlink,
                    0o160000 => EntryKind::Submodule,
                    mode => {
                        return Err(GitError::Read(format!(
                            "tree {id} has entry {:?} with unknown mode {mode:o}",
                            name.as_bstr()
                        )));
                    }
                };
                out.push(TreeEntry {
                    path,
                    id: oid,
                    kind,
                });
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Reads a blob's bytes exactly as stored: no filters, line-ending
    /// conversion or LFS smudging are applied. For a symlink these are the
    /// link target.
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
