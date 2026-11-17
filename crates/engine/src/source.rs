//! Where analysers read files from.
//!
//! [`Directory`] reads a working tree from disk. It is the stand-in until
//! commits can be read through git (#24): it never runs anything, never
//! follows symbolic links, and skips version-control metadata and common
//! build output, which a commit would not contain.

use std::path::{Path, PathBuf};

use blueprint_model::RepoPath;

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
