//! Review state on disk (DR 0011): the [`Mark`]s a reviewer made in one
//! repository, kept with the user's data, not in the index cache, because
//! unlike the index they cannot be rebuilt.
//!
//! One JSON file per repository, `<data>/reviews/<hash of the canonical
//! path>.json`, rewritten whole through a temporary file and a rename, so a
//! crash leaves the old file or the new one, never half of either.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use blueprint_model::NodeId;
use blueprint_model::review::Mark;
use serde::{Deserialize, Serialize};

/// Overrides the data directory, for tests and for users who keep data elsewhere.
pub const DATA_DIR_ENV: &str = "BLUEPRINT_DATA_DIR";

/// The file's layout version.
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    format: u32,
    marks: Vec<Mark>,
}

/// The marks stored for one repository.
#[derive(Debug)]
pub struct Reviews {
    path: PathBuf,
    marks: BTreeMap<(NodeId, u64), Mark>,
    warning: Option<String>,
}

impl Reviews {
    /// Where the marks for the repository at `repo` live:
    /// `<data>/reviews/<hash of the canonical path>.json`, where `<data>` is
    /// `$BLUEPRINT_DATA_DIR` if set, otherwise the platform data directory
    /// (`~/.local/share/blueprint` on Linux).
    ///
    /// # Errors
    /// If `repo` cannot be canonicalised or there is no data directory.
    pub fn location(repo: &Path) -> Result<PathBuf, String> {
        let canonical = repo
            .canonicalize()
            .map_err(|e| format!("{}: {e}", repo.display()))?;
        let root = match std::env::var_os(DATA_DIR_ENV) {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => directories::ProjectDirs::from("", "", "blueprint")
                .map(|d| d.data_dir().to_path_buf())
                .ok_or("no data directory: set BLUEPRINT_DATA_DIR")?,
        };
        let name = format!(
            "{:016x}.json",
            crate::index::fnv1a(canonical.as_os_str().as_encoded_bytes())
        );
        Ok(root.join("reviews").join(name))
    }

    /// Loads the marks for the repository at `repo`; see [`Reviews::open_at`].
    ///
    /// # Errors
    /// As [`Reviews::location`].
    pub fn open(repo: &Path) -> Result<Reviews, String> {
        Ok(Reviews::open_at(Reviews::location(repo)?))
    }

    /// Loads the marks stored at `path`. A missing file holds none. A file
    /// that cannot be read or does not parse is moved aside to
    /// `<path>.damaged`, never overwritten, and its loss is reported by
    /// [`Reviews::warning`]: the session goes on with no marks.
    #[must_use]
    pub fn open_at(path: PathBuf) -> Reviews {
        let mut reviews = Reviews {
            path,
            marks: BTreeMap::new(),
            warning: None,
        };
        let bytes = match std::fs::read(&reviews.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return reviews,
            Err(e) => {
                reviews.warning = Some(format!("{}: {e}", reviews.path.display()));
                return reviews;
            }
        };
        match serde_json::from_slice::<File>(&bytes) {
            Ok(file) if file.format == FORMAT => {
                for m in file.marks {
                    reviews.marks.insert((m.node.clone(), m.key), m);
                }
            }
            parsed => {
                let why = match parsed {
                    Ok(file) => format!("unknown format {}", file.format),
                    Err(e) => e.to_string(),
                };
                let aside = reviews.path.with_extension("json.damaged");
                let moved = std::fs::rename(&reviews.path, &aside);
                reviews.warning = Some(match moved {
                    Ok(()) => format!(
                        "review marks could not be read ({why}); moved to {}",
                        aside.display()
                    ),
                    Err(e) => format!("review marks could not be read ({why}) or moved: {e}"),
                });
            }
        }
        reviews
    }

    /// Why stored marks were lost when loading, if they were.
    #[must_use]
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// Every stored mark, ordered by node.
    pub fn marks(&self) -> impl Iterator<Item = &Mark> {
        self.marks.values()
    }

    /// Stores `mark`, replacing any mark on the same node and key, and
    /// writes the file. A mark that is neither reviewed nor has a note is
    /// removed instead.
    ///
    /// # Errors
    /// If the file cannot be written; the mark is still held in memory.
    pub fn set(&mut self, mark: Mark) -> Result<(), String> {
        let key = (mark.node.clone(), mark.key);
        let empty = !mark.reviewed && mark.note.as_deref().is_none_or(str::is_empty);
        if empty {
            self.marks.remove(&key);
        } else {
            self.marks.insert(key, mark);
        }
        self.write()
    }

    fn write(&self) -> Result<(), String> {
        let file = File {
            format: FORMAT,
            marks: self.marks.values().cloned().collect(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
        let err = |e: std::io::Error| format!("{}: {e}", self.path.display());
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json).map_err(err)?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            err(e)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blueprint_model::{Ecosystem, RepoPath, SystemKey};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bp-reviews-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("reviews").join("repo.json")
    }

    fn mark(key: u64, reviewed: bool, note: Option<&str>) -> Mark {
        Mark {
            node: NodeId::system(SystemKey {
                ecosystem: Ecosystem::Cargo,
                dir: RepoPath::root(),
            }),
            key,
            reviewed,
            note: note.map(str::to_string),
        }
    }

    #[test]
    fn marks_persist_across_restarts() -> Result<(), String> {
        let path = scratch("persist");
        let mut reviews = Reviews::open_at(path.clone());
        assert_eq!(reviews.marks().count(), 0);
        reviews.set(mark(1, true, None))?;
        reviews.set(mark(2, false, Some("why this lock?")))?;
        reviews.set(mark(1, true, Some("fine")))?;

        let again = Reviews::open_at(path.clone());
        assert_eq!(again.warning(), None);
        let marks: Vec<&Mark> = again.marks().collect();
        assert_eq!(
            marks,
            [
                &mark(1, true, Some("fine")),
                &mark(2, false, Some("why this lock?"))
            ]
        );

        let mut again = again;
        again.set(mark(2, false, None))?;
        assert_eq!(
            Reviews::open_at(path.clone()).marks().count(),
            1,
            "an empty mark is removed"
        );
        let _ = std::fs::remove_dir_all(path.parent().and_then(Path::parent).unwrap_or(&path));
        Ok(())
    }

    #[test]
    fn a_damaged_file_is_set_aside_not_overwritten() -> Result<(), String> {
        let path = scratch("damaged");
        let dir = path.parent().ok_or("no parent")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, b"{ not json").map_err(|e| e.to_string())?;

        let mut reviews = Reviews::open_at(path.clone());
        assert!(reviews.warning().is_some_and(|w| w.contains("damaged")));
        assert_eq!(reviews.marks().count(), 0);
        let aside =
            std::fs::read(path.with_extension("json.damaged")).map_err(|e| e.to_string())?;
        assert_eq!(aside, b"{ not json");

        reviews.set(mark(3, true, None))?;
        assert_eq!(Reviews::open_at(path.clone()).marks().count(), 1);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap_or(dir));
        Ok(())
    }
}
