//! The persisted index (DR 0005): immutable, checksummed segment files, read
//! through a memory map.
//!
//! A [`Segment`] maps 32-byte content keys to byte values. Segments are written
//! once by a [`SegmentWriter`] and never modified. A [`Store`] is a directory of
//! segments. Opening one drops any segment that is corrupt or from another
//! format version, so its entries are simply re-analysed: a bad index is
//! rebuilt, never fatal (REQUIREMENTS §5.5).
//!
//! See README.md for why this crate, alone, uses `unsafe`.

mod checksum;
mod segment;
mod store;

pub use segment::{FORMAT_VERSION, KEY_LEN, Key, Segment, SegmentWriter};
pub use store::{Dropped, Store};

use std::fmt;
use std::path::PathBuf;

/// A failure opening or writing the index.
#[derive(Debug)]
pub enum StoreError {
    /// A file or directory could not be read or written.
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The bytes are not a valid segment.
    Corrupt(String),
    /// The segment was written by another format version.
    Version {
        /// The version in the file.
        found: u32,
    },
}

impl StoreError {
    /// Whether the right response is to discard the data and rebuild it.
    #[must_use]
    pub fn is_rebuildable(&self) -> bool {
        matches!(self, Self::Corrupt(_) | Self::Version { .. })
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Corrupt(why) => write!(f, "corrupt index segment: {why}"),
            Self::Version { found } => {
                write!(
                    f,
                    "index segment has format version {found}, expected {FORMAT_VERSION}"
                )
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Corrupt(_) | Self::Version { .. } => None,
        }
    }
}

/// Rewrites the checksum trailer of `bytes` to match the rest. Only for tests
/// and the fuzz target: it lets damaged input reach the structural checks that
/// sit behind the checksum.
#[doc(hidden)]
pub fn reseal(bytes: &mut [u8]) {
    if let Some(body) = bytes.len().checked_sub(8) {
        let sum = checksum::checksum(&bytes[..body]);
        bytes[body..].copy_from_slice(&sum.to_le_bytes());
    }
}
