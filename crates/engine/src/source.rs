//! Where analysers read files from.
//!
//! [`Commit`] reads the files of a commit through git's object database
//! (#24). [`Directory`] reads a working tree from disk: it never runs
//! anything, never follows symbolic links, and skips version-control metadata
//! and common build output, which a commit would not contain.

use std::path::{Path, PathBuf};

use blueprint_model::RepoPath;

use crate::git::{EntryKind, GitError, ObjectId, Repo};
use crate::l1::Diagnostic;

/// Directory names that are never walked: version-control metadata and build
/// output that a commit would not contain.
pub const SKIPPED_DIRS: [&str; 5] = [".git", ".hg", ".svn", "target", "node_modules"];

/// Files under a directory on disk.
#[derive(Debug)]
pub struct Directory {
    root: PathBuf,
    paths: Vec<RepoPath>,
}

impl Directory {
    /// Lists every regular file under `root`, sorted, and returns what could
    /// not be listed (unreadable directories, names that are not UTF-8).
    ///
    /// # Errors
    /// Fails only if `root` itself is not a readable directory.
    pub fn open(root: &Path) -> std::io::Result<(Directory, Vec<Diagnostic>)> {
        let mut diags = Vec::new();
        let mut paths = Vec::new();
        std::fs::read_dir(root)?;
        let mut stack: Vec<Vec<String>> = vec![Vec::new()];
        while let Some(rel) = stack.pop() {
            let dir = rel.iter().fold(root.to_path_buf(), |p, s| p.join(s));
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    diags.push(Diagnostic {
                        path: repo_path(&rel),
                        message: format!("cannot list: {e}"),
                    });
                    continue;
                }
            };
            for entry in entries.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    diags.push(Diagnostic {
                        path: repo_path(&rel),
                        message: "a file name is not UTF-8 and was skipped".into(),
                    });
                    continue;
                };
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let mut child = rel.clone();
                child.push(name.clone());
                if kind.is_dir() {
                    if !SKIPPED_DIRS.contains(&name.as_str()) {
                        stack.push(child);
                    }
                } else if kind.is_file() {
                    match RepoPath::parse(&child.join("/")) {
                        Ok(p) => paths.push(p),
                        Err(_) => diags.push(Diagnostic {
                            path: repo_path(&rel),
                            message: format!("{name:?} is not a valid repository path"),
                        }),
                    }
                }
                // Symbolic links and other special files are not followed.
            }
        }
        paths.sort();
        diags.sort();
        Ok((
            Directory {
                root: root.to_path_buf(),
                paths,
            },
            diags,
        ))
    }

    /// Every file, sorted.
    #[must_use]
    pub fn paths(&self) -> &[RepoPath] {
        &self.paths
    }

    /// Reads one file's bytes.
    #[must_use]
    pub fn read(&self, path: &RepoPath) -> Option<Vec<u8>> {
        let full = path
            .segments()
            .iter()
            .fold(self.root.clone(), |p, s| p.join(s));
        std::fs::read(full).ok()
    }
}

fn repo_path(segs: &[String]) -> RepoPath {
    RepoPath::parse(&segs.join("/")).unwrap_or_else(|_| RepoPath::root())
}

/// The files of one commit, read from git's object database. Nothing is
/// checked out and the working tree is never read.
#[derive(Debug)]
pub struct Commit {
    repo: Repo,
    id: ObjectId,
    /// Sorted by path, so lookups are a binary search.
    files: Vec<(RepoPath, ObjectId)>,
    /// Files whose blob could not be read, for example in a partial clone.
    unreadable: std::cell::RefCell<Vec<RepoPath>>,
}

impl Commit {
    /// Opens the repository at `repo` and lists the files of `rev` (anything
    /// [`Repo::resolve`] accepts). Regular and executable files are listed.
    /// Symbolic links are not followed, as in [`Directory`]. Submodules and
    /// paths that are not UTF-8 are reported as diagnostics.
    ///
    /// # Errors
    /// Fails if the repository cannot be opened, `rev` does not name a
    /// commit, or its trees cannot be read.
    pub fn open(repo: &Path, rev: &str) -> Result<(Commit, Vec<Diagnostic>), GitError> {
        let repo = Repo::open(repo)?;
        let id = repo.resolve(rev)?;
        let mut diags = Vec::new();
        let mut files = Vec::new();
        for entry in repo.tree(id)? {
            let Some(text) = entry.path_str() else {
                diags.push(Diagnostic {
                    path: RepoPath::root(),
                    message: format!("{:?} is not UTF-8 and was skipped", entry.path_lossy()),
                });
                continue;
            };
            let path = match RepoPath::parse(text) {
                Ok(p) => p,
                Err(_) => {
                    diags.push(Diagnostic {
                        path: RepoPath::root(),
                        message: format!("{text:?} is not a valid repository path"),
                    });
                    continue;
                }
            };
            match entry.kind {
                EntryKind::Blob | EntryKind::Executable => files.push((path, entry.id)),
                EntryKind::Symlink => {}
                EntryKind::Submodule => diags.push(Diagnostic {
                    path,
                    message: format!("submodule at {} is not read", entry.id),
                }),
            }
        }
        files.sort();
        diags.sort();
        Ok((
            Commit {
                repo,
                id,
                files,
                unreadable: std::cell::RefCell::default(),
            },
            diags,
        ))
    }

    /// The commit id that the revision resolved to.
    #[must_use]
    pub fn id(&self) -> ObjectId {
        self.id
    }

    /// Every file, sorted.
    #[must_use]
    pub fn paths(&self) -> Vec<RepoPath> {
        self.files.iter().map(|(p, _)| p.clone()).collect()
    }

    /// Every file with its blob id, sorted by path.
    #[must_use]
    pub fn blob_ids(&self) -> &[(RepoPath, ObjectId)] {
        &self.files
    }

    /// Reads one file's bytes as stored in the commit. A file that is in the
    /// commit but whose blob cannot be read is remembered; see
    /// [`Commit::unreadable`].
    #[must_use]
    pub fn read(&self, path: &RepoPath) -> Option<Vec<u8>> {
        let i = self.files.binary_search_by(|(p, _)| p.cmp(path)).ok()?;
        let bytes = self.repo.blob(self.files[i].1).ok();
        if bytes.is_none() {
            self.unreadable.borrow_mut().push(path.clone());
        }
        bytes
    }

    /// Files in the commit that [`Commit::read`] was asked for but could not
    /// read, sorted. A model built while any were missing describes less than
    /// the commit, so it must not be stored as the commit's model.
    #[must_use]
    pub fn unreadable(&self) -> Vec<RepoPath> {
        let mut v = self.unreadable.borrow().clone();
        v.sort();
        v.dedup();
        v
    }
}
