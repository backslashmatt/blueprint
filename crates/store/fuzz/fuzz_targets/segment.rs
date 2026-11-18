//! Fuzz the segment reader (REQUIREMENTS §5.5). The input is used twice: as
//! is, which mostly exercises the checksum, and resealed with a matching
//! checksum, which reaches the structural checks behind it. Whatever validates
//! must then answer every lookup consistently without panicking.

#![no_main]

use blueprint_store::{Key, KEY_LEN, Segment, reseal};
use libfuzzer_sys::fuzz_target;

fn check(bytes: Vec<u8>) {
    if let Ok(seg) = Segment::from_bytes(bytes) {
        for (k, v) in seg.iter() {
            assert_eq!(seg.get(&k), Some(v));
        }
        let _ = seg.get(&Key([0xFF; KEY_LEN]));
    }
}

fuzz_target!(|data: &[u8]| {
    check(data.to_vec());
    let mut sealed = data.to_vec();
    reseal(&mut sealed);
    check(sealed);
});
