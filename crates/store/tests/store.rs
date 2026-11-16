//! Segment and store behaviour, including damage: truncation, bit flips,
//! version mismatch and random mutation (REQUIREMENTS §5.5).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use blueprint_store::{
    FORMAT_VERSION, KEY_LEN, Key, Segment, SegmentWriter, Store, StoreError, reseal,
};

fn key(n: u32) -> Key {
    let mut k = [0u8; KEY_LEN];
    k[..4].copy_from_slice(&n.to_be_bytes());
    k[KEY_LEN - 1] = 0xAB;
    Key(k)
}

fn writer(ids: impl IntoIterator<Item = u32>) -> SegmentWriter {
    let mut w = SegmentWriter::new();
    for i in ids {
        w.insert(key(i), format!("value {i}").repeat((i % 4) as usize));
    }
    w
}

fn fresh_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("store-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn round_trips_every_entry_in_key_order() -> Result<(), StoreError> {
    let seg = Segment::from_bytes(writer((0..500).rev()).to_bytes())?;
    assert_eq!(seg.len(), 500);
    for i in 0..500 {
        let want = format!("value {i}").repeat((i % 4) as usize);
        assert_eq!(seg.get(&key(i)), Some(want.as_bytes()), "key {i}");
    }
    assert_eq!(seg.get(&key(500)), None);
    assert_eq!(seg.get(&Key([0; KEY_LEN])), None);
    let keys: Vec<Key> = seg.iter().map(|(k, _)| k).collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]));
    Ok(())
}

#[test]
fn an_empty_segment_is_valid() -> Result<(), StoreError> {
    let seg = Segment::from_bytes(SegmentWriter::new().to_bytes())?;
    assert!(seg.is_empty());
    assert_eq!(seg.get(&key(1)), None);
    Ok(())
}

#[test]
fn every_truncation_is_rejected_as_rebuildable() {
    let bytes = writer(0..5).to_bytes();
    for len in 0..bytes.len() {
        let err = Segment::from_bytes(bytes[..len].to_vec()).err();
        assert!(
            err.as_ref().is_some_and(StoreError::is_rebuildable),
            "truncated to {len}: {err:?}"
        );
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(Segment::from_bytes(longer).is_err(), "trailing garbage");
}

#[test]
fn every_single_bit_flip_is_rejected_as_rebuildable() {
    let bytes = writer(0..5).to_bytes();
    for byte in 0..bytes.len() {
        for bit in 0..8 {
            let mut b = bytes.clone();
            b[byte] ^= 1 << bit;
            let err = Segment::from_bytes(b).err();
            assert!(
                err.as_ref().is_some_and(StoreError::is_rebuildable),
                "flip at {byte}.{bit}: {err:?}"
            );
        }
    }
}

#[test]
fn structural_damage_behind_a_valid_checksum_is_rejected() {
    let bytes = writer(0..3).to_bytes();
    let offsets_at = 32 + 3 * KEY_LEN;
    type Damage = Box<dyn Fn(&mut Vec<u8>)>;
    let cases: [(&str, Damage); 5] = [
        ("count too large", Box::new(|b| b[16] = 4)),
        (
            "keys out of order",
            Box::new(|b| b.swap(32 + 3, 32 + KEY_LEN + 3)),
        ),
        (
            "first offset non-zero",
            Box::new(move |b| b[offsets_at] = 1),
        ),
        (
            "offsets decrease",
            Box::new(move |b| b[offsets_at + 8] = 0xFF),
        ),
        ("key length", Box::new(|b| b[12] = 16)),
    ];
    for (what, damage) in cases {
        let mut b = bytes.clone();
        damage(&mut b);
        reseal(&mut b);
        assert!(
            matches!(Segment::from_bytes(b), Err(StoreError::Corrupt(_))),
            "{what}"
        );
    }
}

#[test]
fn another_format_version_is_reported_as_such() {
    let mut bytes = writer(0..3).to_bytes();
    bytes[8..12].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    match Segment::from_bytes(bytes) {
        Err(StoreError::Version { found }) => assert_eq!(found, FORMAT_VERSION + 1),
        other => panic!("expected a version mismatch, got {other:?}"),
    }
}

/// A deterministic stand-in for the fuzz target, so that CI on stable Rust
/// exercises the reader with mutated input on every run.
#[test]
fn random_mutations_never_panic() {
    let seeds = [
        writer(0..40).to_bytes(),
        SegmentWriter::new().to_bytes(),
        writer([7]).to_bytes(),
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20_000 {
        let mut b = seeds[(next() % 3) as usize].clone();
        for _ in 0..=(next() % 4) {
            match next() % 4 {
                0 => b.truncate((next() as usize) % (b.len() + 1)),
                1 if !b.is_empty() => {
                    let i = (next() as usize) % b.len();
                    b[i] = next() as u8;
                }
                2 => b.extend((0..next() % 16).map(|_| next() as u8)),
                _ if !b.is_empty() => {
                    // Change a byte, then fix the checksum, so that the
                    // structural checks behind it are exercised too.
                    let i = (next() as usize) % b.len();
                    b[i] = next() as u8;
                    reseal(&mut b);
                }
                _ => {}
            }
        }
        if let Ok(seg) = Segment::from_bytes(b) {
            for (k, v) in seg.iter() {
                assert_eq!(seg.get(&k), Some(v));
            }
        }
    }
}

#[test]
fn store_drops_and_deletes_damaged_segments_and_keeps_good_ones() -> Result<(), StoreError> {
    let dir = fresh_dir("damage");
    let mut store = Store::open(&dir)?;
    store.add(&writer(0..10))?;
    store.add(&writer(100..110))?;
    assert_eq!(store.segment_count(), 2);
    drop(store);

    let mut segs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| StoreError::Io {
            path: dir.clone(),
            source: e,
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    segs.sort();
    let damaged = &segs[0];
    let mut bytes = std::fs::read(damaged).map_err(|e| StoreError::Io {
        path: damaged.clone(),
        source: e,
    })?;
    bytes[40] ^= 0x10;
    std::fs::write(damaged, &bytes).map_err(|e| StoreError::Io {
        path: damaged.clone(),
        source: e,
    })?;
    std::fs::write(dir.join("empty.seg"), b"").map_err(|e| StoreError::Io {
        path: dir.clone(),
        source: e,
    })?;
    let io = |e| StoreError::Io {
        path: dir.clone(),
        source: e,
    };
    // One temporary file a crashed writer left two hours ago, one that
    // another process may still be writing.
    let abandoned = dir.join(".tmp-1-1-1");
    std::fs::write(&abandoned, b"partial").map_err(io)?;
    std::fs::File::options()
        .write(true)
        .open(&abandoned)
        .and_then(|f| f.set_modified(SystemTime::now() - Duration::from_secs(7200)))
        .map_err(io)?;
    std::fs::write(dir.join(".tmp-2-1-1"), b"in progress").map_err(io)?;

    let store = Store::open(&dir)?;
    assert_eq!(store.segment_count(), 1);
    assert_eq!(store.dropped().len(), 2);
    assert!(store.dropped().iter().all(|d| d.reason.is_rebuildable()));
    assert!(!damaged.exists(), "a corrupt segment is deleted");
    assert!(!dir.join("empty.seg").exists());
    assert!(!abandoned.exists(), "abandoned temporary files are removed");
    assert!(
        dir.join(".tmp-2-1-1").exists(),
        "a write that may be in progress is left alone"
    );
    let survivors = (0..10)
        .chain(100..110)
        .filter(|i| store.get(&key(*i)).is_some())
        .count();
    assert_eq!(
        survivors, 10,
        "the undamaged segment's entries are all still there"
    );
    Ok(())
}

#[test]
fn compact_merges_segments_and_survives_reopening() -> Result<(), StoreError> {
    let dir = fresh_dir("compact");
    let mut store = Store::open(&dir)?;
    store.add(&writer(0..50))?;
    store.add(&writer(25..75))?;
    store.add(&writer(0..50))?; // identical content: same file, not a new segment
    assert_eq!(store.segment_count(), 2);
    store.compact()?;
    assert_eq!(store.segment_count(), 1);
    drop(store);
    let store = Store::open(&dir)?;
    assert_eq!(store.segment_count(), 1);
    assert!(store.dropped().is_empty());
    for i in 0..75 {
        assert!(store.get(&key(i)).is_some(), "key {i}");
    }
    Ok(())
}

#[test]
fn segments_of_other_versions_are_handled_by_name() -> Result<(), StoreError> {
    let dir = fresh_dir("versions");
    let io = |e| StoreError::Io {
        path: dir.clone(),
        source: e,
    };
    let mut store = Store::open(&dir)?;
    store.add(&writer(0..3))?;
    drop(store);
    let older = dir.join(format!("v{}-00000000000000aa.seg", FORMAT_VERSION - 1));
    let newer = dir.join(format!("v{}-00000000000000bb.seg", FORMAT_VERSION + 1));
    std::fs::write(&older, b"old format").map_err(io)?;
    std::fs::write(&newer, b"new format").map_err(io)?;
    let store = Store::open(&dir)?;
    assert_eq!(store.segment_count(), 1);
    assert!(store.dropped().is_empty());
    assert!(!older.exists(), "an older version's segment is deleted");
    assert!(newer.exists(), "a newer Blueprint's segment is left for it");
    Ok(())
}

#[test]
fn flipping_the_top_bit_of_two_words_is_detected() {
    // Pairs like these passed the first version of the checksum. Bytes 32..64
    // are the one key, so only the checksum can tell the damage apart.
    let bytes = writer([1]).to_bytes();
    for (a, b) in [(39, 47), (47, 55), (55, 63), (39, 63)] {
        let mut d = bytes.clone();
        d[a] ^= 0x80;
        d[b] ^= 0x80;
        assert!(Segment::from_bytes(d).is_err(), "bytes {a} and {b}");
    }
}
