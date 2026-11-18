//! The segment checksum.
//!
//! It folds the input a little-endian word at a time with
//! `h = mix((h ^ w) * P)`, where `mix(x) = x ^ (x >> 29)`. Each step is a
//! bijection in `h` for a fixed word and in the word for a fixed `h`, so a
//! change confined to one 8-byte word (any single bit flip) always changes
//! the result. The shift feeds high bits back into low ones: without it, bit
//! 63 passes through the multiply untouched and two such flips cancel. Other
//! multi-word damage is caught with high probability but not guaranteed, as
//! with any 64-bit checksum. The length is folded in last.
//!
//! It is not cryptographic: the files are Blueprint's own, and the check is
//! there to catch damage, not tampering.

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

pub(crate) fn checksum(bytes: &[u8]) -> u64 {
    let mut h = OFFSET;
    let (words, rest) = bytes.as_chunks::<8>();
    for w in words {
        h = step(h, u64::from_le_bytes(*w));
    }
    for &b in rest {
        h = step(h, u64::from(b));
    }
    step(h, bytes.len() as u64)
}

fn step(h: u64, w: u64) -> u64 {
    let x = (h ^ w).wrapping_mul(PRIME);
    x ^ (x >> 29)
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
    fn flipping_the_same_bit_in_two_words_changes_the_checksum() {
        // Without the shift in `step`, bit 63 of two words cancelled out.
        let data = vec![0x5Au8; 64];
        let base = checksum(&data);
        for bit in [0, 31, 62, 63] {
            for a in 0..8 {
                for b in a + 1..8 {
                    let mut d = data.clone();
                    d[a * 8 + bit / 8] ^= 1 << (bit % 8);
                    d[b * 8 + bit / 8] ^= 1 << (bit % 8);
                    assert_ne!(checksum(&d), base, "bit {bit} in words {a} and {b}");
                }
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
