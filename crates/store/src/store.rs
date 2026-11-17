//! A directory of segments.

use std::path::{Path, PathBuf};

use crate::StoreError;
use std::time::Duration;

use crate::segment::{FORMAT_VERSION, Key, Segment, SegmentWriter, TEMP_PREFIX, name_version};

/// A segment that [`Store::open`] discarded, and why.
#[derive(Debug)]
pub struct Dropped {
    /// The file that was discarded.
    pub path: PathBuf,
    /// Why it was discarded.
    pub reason: StoreError,
}

/// The index for one repository: every segment in one directory.
///
/// Keys are content-addressed and include the analyser version, so a key has
/// the same value in every segment that holds it, and lookup order does not
/// matter.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    segments: Vec<(PathBuf, Segment)>,
    dropped: Vec<Dropped>,
}

/// How old a temporary file must be before `Store::open` treats it as
/// abandoned by a writer that crashed.
const STALE_TEMP: Duration = Duration::from_secs(3600);

fn is_segment(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "seg")
}

impl Store {
    /// Opens the store in `dir`, creating the directory if needed.
    ///
    /// A segment that is corrupt or fails validation is deleted and listed in
    /// [`Store::dropped`]; its entries will be re-analysed. Segments named for
    /// an older format version are deleted; a newer version's are left alone
    /// for the Blueprint that wrote them. Temporary files are deleted once
    /// they are an hour old, which means their writer crashed.
    ///
    /// # Errors
    /// [`StoreError::Io`] only if the directory itself cannot be created or
    /// listed. Damage to the files in it is never an error.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| StoreError::io(dir, e))? {
            let entry = entry.map_err(|e| StoreError::io(dir, e))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(TEMP_PREFIX) {
                // Another process may still be writing it: only remove
                // temporary files old enough to be abandoned.
                let abandoned = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age >= STALE_TEMP);
                if abandoned {
                    let _ = std::fs::remove_file(&path);
                }
            } else if is_segment(&path) {
                match name_version(&name) {
                    // A newer Blueprint's segment: leave it for that version.
                    Some(v) if v > FORMAT_VERSION => {}
                    Some(v) if v < FORMAT_VERSION => {
                        let _ = std::fs::remove_file(&path);
                    }
                    _ => paths.push(path),
                }
            }
        }
        paths.sort();
        let mut store = Self {
            dir: dir.to_owned(),
            segments: Vec::new(),
            dropped: Vec::new(),
        };
        for path in paths {
            match Segment::open(&path) {
                Ok(seg) => store.segments.push((path, seg)),
                Err(reason) => {
                    if reason.is_rebuildable() {
                        // Best effort: if it cannot be deleted it is retried,
                        // and dropped again, on the next open.
                        let _ = std::fs::remove_file(&path);
                    }
                    store.dropped.push(Dropped { path, reason });
                }
            }
        }
        Ok(store)
    }

    /// The directory the store lives in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Segments discarded by [`Store::open`].
    #[must_use]
    pub fn dropped(&self) -> &[Dropped] {
        &self.dropped
    }

    /// The number of open segments.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// The value stored for `key`, if any.
    #[must_use]
    pub fn get(&self, key: &Key) -> Option<&[u8]> {
        self.segments.iter().find_map(|(_, s)| s.get(key))
    }

    /// Writes `writer`'s entries as a new segment and opens it.
    ///
    /// # Errors
    /// [`StoreError::Io`] if it cannot be written; anything else means the
    /// file was damaged between writing and reading it.
    pub fn add(&mut self, writer: &SegmentWriter) -> Result<(), StoreError> {
        if writer.is_empty() {
            return Ok(());
        }
        let path = writer.write_to(&self.dir)?;
        if self.segments.iter().any(|(p, _)| *p == path) {
            return Ok(());
        }
        let seg = Segment::open(&path)?;
        self.segments.push((path, seg));
        Ok(())
    }

    /// Merges every segment into one and deletes the others.
    ///
    /// # Errors
    /// [`StoreError::Io`] if the merged segment cannot be written. The old
    /// segments are deleted only after it has been written.
    pub fn compact(&mut self) -> Result<(), StoreError> {
        if self.segments.len() < 2 {
            return Ok(());
        }
        let mut merged = SegmentWriter::new();
        for (_, seg) in &self.segments {
            for (k, v) in seg.iter() {
                merged.insert(k, v);
            }
        }
        let path = merged.write_to(&self.dir)?;
        // Open the merged segment before letting go of the old ones, so a
        // failure here leaves the store as it was.
        let seg = Segment::open(&path)?;
        // Unmap before deleting: Windows refuses to delete a mapped file.
        let old: Vec<PathBuf> = self.segments.drain(..).map(|(p, _)| p).collect();
        self.segments.push((path.clone(), seg));
        for p in old.into_iter().filter(|p| *p != path) {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }
}
