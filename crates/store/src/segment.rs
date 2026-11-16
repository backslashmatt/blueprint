//! One segment file.
//!
//! Layout, all integers little-endian:
//!
//! ```text
//! header   magic "BPINDEX\0" (8) | version u32 | key length u32 | count u64 | values length u64
//! keys     count × 32 bytes, strictly ascending
//! offsets  (count + 1) × u64: offsets[0] = 0, non-decreasing, offsets[count] = values length
//! values   values length bytes; value i is values[offsets[i]..offsets[i + 1]]
//! trailer  checksum u64 of every byte before it
//! ```
//!
//! Every field is checked when a segment is opened, so reads after that cannot
//! go out of bounds.

use std::collections::BTreeMap;
use std::fs::File;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::Mmap;

use crate::StoreError;
use crate::checksum::checksum;

/// The format version this build reads and writes. Bump it whenever the layout
/// or the meaning of a value changes; older segments are then rebuilt.
pub const FORMAT_VERSION: u32 = 1;

/// The length of every key, in bytes.
pub const KEY_LEN: usize = 32;

const MAGIC: [u8; 8] = *b"BPINDEX\0";
const HEADER: usize = 32;
const TRAILER: usize = 8;

/// A content key: for example a git object id plus an analyser id and version.
/// The store does not interpret it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(pub [u8; KEY_LEN]);

enum Bytes {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Mapped(m) => m,
            Self::Owned(v) => v,
        }
    }
}

/// An open, validated segment.
pub struct Segment {
    bytes: Bytes,
    count: usize,
    offsets_at: usize,
    values_at: usize,
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("entries", &self.count)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

fn corrupt(why: impl Into<String>) -> StoreError {
    StoreError::Corrupt(why.into())
}

impl Segment {
    /// Opens and validates the segment file at `path`.
    ///
    /// # Errors
    /// [`StoreError::Io`] if the file cannot be read; [`StoreError::Corrupt`]
    /// or [`StoreError::Version`] if it should be discarded and rebuilt.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let file = File::open(path).map_err(|e| StoreError::io(path, e))?;
        let len = file.metadata().map_err(|e| StoreError::io(path, e))?.len();
        if len < (HEADER + 8 + TRAILER) as u64 {
            // Too short to be a segment; also avoids mapping an empty file,
            // which some platforms refuse.
            return Err(corrupt(format!("{len} bytes is shorter than any segment")));
        }
        #[allow(unsafe_code)]
        // SAFETY: a memory map is only sound while no one else modifies or
        // truncates the file. Segment files belong to Blueprint, live in its
        // cache directory, are written to a temporary name and renamed into
        // place, and are never written again: a change is always a new file.
        // Deleting a mapped file is safe on Unix and refused on Windows. The
        // remaining risk, another program truncating the file while it is open,
        // is documented in README.md.
        let map = unsafe { Mmap::map(&file) }.map_err(|e| StoreError::io(path, e))?;
        Self::validate(Bytes::Mapped(map))
    }

    /// Validates a segment held in memory. Used by tests and the fuzz target.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] or [`StoreError::Version`].
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, StoreError> {
        Self::validate(Bytes::Owned(bytes))
    }

    fn validate(bytes: Bytes) -> Result<Self, StoreError> {
        let b: &[u8] = &bytes;
        if b.len() < HEADER + 8 + TRAILER {
            return Err(corrupt(format!(
                "{} bytes is shorter than any segment",
                b.len()
            )));
        }
        if b[..8] != MAGIC {
            return Err(corrupt("not a Blueprint index segment"));
        }
        let version = u32_at(b, 8);
        if version != FORMAT_VERSION {
            return Err(StoreError::Version { found: version });
        }
        if u32_at(b, 12) as usize != KEY_LEN {
            return Err(corrupt("unexpected key length"));
        }
        let count = usize::try_from(u64_at(b, 16)).map_err(|_| corrupt("entry count too large"))?;
        let values_len =
            usize::try_from(u64_at(b, 24)).map_err(|_| corrupt("values length too large"))?;
        let expected = count
            .checked_mul(KEY_LEN)
            .and_then(|k| count.checked_add(1)?.checked_mul(8)?.checked_add(k))
            .and_then(|n| n.checked_add(values_len)?.checked_add(HEADER + TRAILER))
            .ok_or_else(|| corrupt("sizes overflow"))?;
        if expected != b.len() {
            return Err(corrupt(format!(
                "length is {} bytes, header implies {expected}",
                b.len()
            )));
        }
        let body = b.len() - TRAILER;
        if checksum(&b[..body]) != u64_at(b, body) {
            return Err(corrupt("checksum mismatch"));
        }
        let offsets_at = HEADER + count * KEY_LEN;
        let values_at = offsets_at + (count + 1) * 8;
        if u64_at(b, offsets_at) != 0 {
            return Err(corrupt("first offset is not zero"));
        }
        let mut prev = 0u64;
        for i in 1..=count {
            let o = u64_at(b, offsets_at + i * 8);
            if o < prev {
                return Err(corrupt("offsets decrease"));
            }
            prev = o;
        }
        if prev != values_len as u64 {
            return Err(corrupt("last offset does not match the values length"));
        }
        for i in 1..count {
            let (a, k) = (HEADER + (i - 1) * KEY_LEN, HEADER + i * KEY_LEN);
            if b[a..a + KEY_LEN] >= b[k..k + KEY_LEN] {
                return Err(corrupt("keys are not strictly ascending"));
            }
        }
        Ok(Self {
            bytes,
            count,
            offsets_at,
            values_at,
        })
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the segment has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn key(&self, i: usize) -> &[u8] {
        let at = HEADER + i * KEY_LEN;
        &self.bytes[at..at + KEY_LEN]
    }

    fn value(&self, i: usize) -> &[u8] {
        // Validation guarantees offsets are in range and non-decreasing.
        let from = u64_at(&self.bytes, self.offsets_at + i * 8) as usize;
        let to = u64_at(&self.bytes, self.offsets_at + (i + 1) * 8) as usize;
        &self.bytes[self.values_at + from..self.values_at + to]
    }

    /// The value stored for `key`, if any.
    #[must_use]
    pub fn get(&self, key: &Key) -> Option<&[u8]> {
        let (mut lo, mut hi) = (0, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.key(mid).cmp(&key.0[..]) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(self.value(mid)),
            }
        }
        None
    }

    /// Every entry, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (Key, &[u8])> + '_ {
        (0..self.count).map(|i| {
            let mut k = [0u8; KEY_LEN];
            k.copy_from_slice(self.key(i));
            (Key(k), self.value(i))
        })
    }
}

/// Collects entries and writes them as one segment.
#[derive(Debug, Default)]
pub struct SegmentWriter {
    entries: BTreeMap<Key, Vec<u8>>,
}

impl SegmentWriter {
    /// An empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces an entry.
    pub fn insert(&mut self, key: Key, value: impl Into<Vec<u8>>) {
        self.entries.insert(key, value.into());
    }

    /// The number of entries so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no entries have been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The segment's bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let count = self.entries.len();
        let values_len: usize = self.entries.values().map(Vec::len).sum();
        let mut out =
            Vec::with_capacity(HEADER + count * KEY_LEN + (count + 1) * 8 + values_len + TRAILER);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(KEY_LEN as u32).to_le_bytes());
        out.extend_from_slice(&(count as u64).to_le_bytes());
        out.extend_from_slice(&(values_len as u64).to_le_bytes());
        for k in self.entries.keys() {
            out.extend_from_slice(&k.0);
        }
        let mut offset = 0u64;
        out.extend_from_slice(&offset.to_le_bytes());
        for v in self.entries.values() {
            offset += v.len() as u64;
            out.extend_from_slice(&offset.to_le_bytes());
        }
        for v in self.entries.values() {
            out.extend_from_slice(v);
        }
        let sum = checksum(&out);
        out.extend_from_slice(&sum.to_le_bytes());
        out
    }

    /// Writes the segment into `dir` and returns its path. The name holds the
    /// format version and the checksum, so identical content gets the same
    /// name and different versions never collide.
    ///
    /// The file is written under a unique temporary name, synced, and renamed
    /// into place, so a reader never sees a partial segment. If a valid file
    /// with the same name already exists it is reused.
    ///
    /// # Errors
    /// [`StoreError::Io`] if the file cannot be written.
    pub fn write_to(&self, dir: &Path) -> Result<PathBuf, StoreError> {
        let bytes = self.to_bytes();
        let path = dir.join(segment_name(checksum(&bytes)));
        if Segment::open(&path).is_ok() {
            return Ok(path);
        }
        let tmp = dir.join(temp_name());
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, &path)
        };
        match write() {
            Ok(()) => Ok(path),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                // Another process may have written the same segment first
                // (on Windows the rename then fails if it is mapped).
                if Segment::open(&path).is_ok() {
                    Ok(path)
                } else {
                    Err(StoreError::io(&path, e))
                }
            }
        }
    }
}

/// The prefix of every temporary file.
pub(crate) const TEMP_PREFIX: &str = ".tmp-";

/// A temporary name no other writer, in this process or another, will pick:
/// the process id, a counter and the time.
fn temp_name() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{TEMP_PREFIX}{}-{n}-{nanos}", std::process::id())
}

/// The file name of a segment of this format version with `sum`.
fn segment_name(sum: u64) -> String {
    format!("v{FORMAT_VERSION}-{sum:016x}.seg")
}

/// The format version encoded in a segment file name, if it has one.
pub(crate) fn name_version(name: &str) -> Option<u32> {
    name.strip_prefix('v')?.split_once('-')?.0.parse().ok()
}
