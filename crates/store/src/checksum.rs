//! The segment checksum.
//!
//! It folds the input a little-endian word at a time with
//! `h = (h ^ w) * P (mod 2^64)`. For a fixed `h` that step is a bijection in
//! `w`, and for a fixed `w` it is a bijection in `h`. So changing any one word
//! (any single bit flip, or any burst within 8 aligned bytes) always changes
//! the result. The length is folded in last, so truncation is caught as well.
//! It is not cryptographic: the files are Blueprint's own, and the check is
//! there to catch damage, not tampering.

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

pub(crate) fn checksum(bytes: &[u8]) -> u64 {
    let mut h = OFFSET;
    let mut words = bytes.chunks_exact(8);
    for w in &mut words {
        let mut a = [0u8; 8];
        a.copy_from_slice(w);
        h = (h ^ u64::from_le_bytes(a)).wrapping_mul(PRIME);
    }
    for &b in words.remainder() {
        h = (h ^ u64::from(b)).wrapping_mul(PRIME);
    }
    (h ^ bytes.len() as u64).wrapping_mul(PRIME)
}

#[cfg(test)]
mod tests {
    use super::checksum;

    #[test]
    fn every_single_bit_flip_changes_the_checksum() {
        let data: Vec<u8> = (0..67u8).map(|i| i.wrapping_mul(37)).collect();
        let base = checksum(&data);
        for byte in 0..data.len() {
            for bit in 0..8 {
                let mut d = data.clone();
                d[byte] ^= 1 << bit;
                assert_ne!(checksum(&d), base, "flip at byte {byte} bit {bit}");
            }
        }
    }

    #[test]
    fn truncation_and_extension_change_the_checksum() {
        let data = vec![0u8; 40];
        assert_ne!(checksum(&data), checksum(&data[..39]));
        assert_ne!(checksum(&data), checksum(&[data.as_slice(), &[0]].concat()));
    }
}
