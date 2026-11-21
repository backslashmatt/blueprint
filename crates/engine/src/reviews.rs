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

static DATA_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Sets the data directory for the rest of the process, over
/// `$BLUEPRINT_DATA_DIR`: for a front end with its own setting, and for
/// tests, which cannot set the environment safely while other threads run.
/// Returns `false`, changing nothing, if it was already set to another
/// directory.
pub fn set_data_root(dir: PathBuf) -> bool {
    DATA_ROOT.get_or_init(|| dir.clone()) == &dir
}

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
    /// False when the file holds marks this could not load and could not
    /// move aside: writing would destroy them.
    writable: bool,
}

impl Reviews {
    /// Where the marks for the repository at `repo` live:
    /// `<data>/reviews/<hash of the canonical path>.json`, where `<data>` is
    /// the directory given to [`set_data_root`] if any, else
    /// `$BLUEPRINT_DATA_DIR` if set, otherwise the platform data directory
    /// (`~/.local/share/blueprint` on Linux).
    ///
    /// # Errors
    /// If `repo` cannot be canonicalised or there is no data directory.
    pub fn location(repo: &Path) -> Result<PathBuf, String> {
        let canonical = repo
            .canonicalize()
            .map_err(|e| format!("{}: {e}", repo.display()))?;
        let root = match (DATA_ROOT.get(), std::env::var_os(DATA_DIR_ENV)) {
            (Some(dir), _) => dir.clone(),
            (None, Some(dir)) if !dir.is_empty() => PathBuf::from(dir),
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
    /// that does not parse is moved aside to `<path>.damaged` (or
    /// `.damaged.1` and so on, never over an earlier one), and its loss is
    /// reported by [`Reviews::warning`]: the session goes on with no marks.
    /// A file that cannot be read, or cannot be moved aside, or was written
    /// by a newer version, is left alone: the session goes on with no
    /// marks, and [`Reviews::set`] refuses to write over it.
    #[must_use]
    pub fn open_at(path: PathBuf) -> Reviews {
        let mut reviews = Reviews {
            path,
            marks: BTreeMap::new(),
            warning: None,
            writable: true,
        };
        let bytes = match std::fs::read(&reviews.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return reviews,
            Err(e) => {
                reviews.warning = Some(format!(
                    "review marks could not be read, and will not be saved: {}: {e}",
                    reviews.path.display()
                ));
                reviews.writable = false;
                return reviews;
            }
        };
        // The format first, so a newer file is recognised even when its
        // marks have a shape this version cannot parse.
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        if let Ok(Format { format }) = serde_json::from_slice::<Format>(&bytes)
            && format > FORMAT
        {
            reviews.warning = Some(format!(
                "review marks were saved by a newer Blueprint (format {format}); \
                 they are not shown, and new marks will not be saved"
            ));
            reviews.writable = false;
            return reviews;
        }
        let why = match serde_json::from_slice::<File>(&bytes) {
            Ok(file) if file.format == FORMAT => {
                for m in file.marks {
                    reviews.marks.insert((m.node.clone(), m.key), m);
                }
                return reviews;
            }
            Ok(file) => format!("unknown format {}", file.format),
            Err(e) => e.to_string(),
        };
        let aside = (0..)
            .map(|n| match n {
                0 => reviews.path.with_extension("json.damaged"),
                n => reviews.path.with_extension(format!("json.damaged.{n}")),
            })
            .find(|p| !p.exists())
            .unwrap_or_else(|| reviews.path.with_extension("json.damaged"));
        reviews.warning = Some(match std::fs::rename(&reviews.path, &aside) {
            Ok(()) => format!(
                "review marks could not be read ({why}); moved to {}",
                aside.display()
            ),
            Err(e) => {
                reviews.writable = false;
                format!(
                    "review marks could not be read ({why}) or moved aside, and will not be saved: {e}"
                )
            }
        });
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
    /// If the file cannot be written, or holds marks that could not be
    /// loaded (see [`Reviews::open_at`]); the mark is still held in memory.
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
        if !self.writable {
            return Err(format!(
                "{}: not saved, so as not to overwrite marks that could not be loaded",
                self.path.display()
            ));
        }
        let file = File {
            format: FORMAT,
            marks: self.marks.values().cloned().collect(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
        let err = |e: std::io::Error| format!("{}: {e}", self.path.display());
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        // Unique per write, so two writers in one process never share it.
        static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self
            .path
            .with_extension(format!("json.{}-{n}.tmp", std::process::id()));
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

        // A second damaged file does not replace the first one set aside.
        std::fs::write(&path, b"[").map_err(|e| e.to_string())?;
        let _ = Reviews::open_at(path.clone());
        let second = std::fs::read(path.with_extension("json.damaged.1"));
        assert_eq!(second.map_err(|e| e.to_string())?, b"[");
        let first = std::fs::read(path.with_extension("json.damaged"));
        assert_eq!(first.map_err(|e| e.to_string())?, b"{ not json");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap_or(dir));
        Ok(())
    }

    #[test]
    fn marks_from_a_newer_version_are_never_overwritten() -> Result<(), String> {
        let path = scratch("newer");
        let dir = path.parent().ok_or("no parent")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let newer = br#"{"format": 99, "marks": [{"renamed": 1}], "more": true}"#;
        std::fs::write(&path, newer).map_err(|e| e.to_string())?;

        let mut reviews = Reviews::open_at(path.clone());
        assert!(reviews.warning().is_some_and(|w| w.contains("newer")));
        assert!(reviews.set(mark(1, true, None)).is_err());
        assert_eq!(std::fs::read(&path).map_err(|e| e.to_string())?, newer);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap_or(dir));
        Ok(())
    }
}
