use super::Error;

const MAX_LEN: usize = 4096;

/// Each value occupies consecutive low-to-high bits within a single word.
/// Unused high bits, including unused entries in the final word, stay zero.
#[derive(Debug)]
pub(super) struct Packed {
    bits: u8,
    len: usize,
    words: Vec<u64>,
}

impl Packed {
    pub(super) fn new(bits: u8, len: usize, allocation_limit: usize) -> Result<Self, Error> {
        let count = word_count(bits, len)?;
        if count * size_of::<u64>() > allocation_limit {
            return Err(Error::AllocationBudgetExceeded);
        }
        let mut words = Vec::new();
        words
            .try_reserve_exact(count)
            .map_err(|_| Error::AllocationFailed)?;
        if words.capacity() * size_of::<u64>() > allocation_limit {
            return Err(Error::AllocationBudgetExceeded);
        }
        words.resize(count, 0);
        Ok(Self { bits, len, words })
    }

    /// Takes an already-budgeted allocation without copying or growing it.
    pub(super) fn from_words(bits: u8, len: usize, words: Vec<u64>) -> Result<Self, Error> {
        let expected = word_count(bits, len)?;
        if words.len() != expected {
            return Err(Error::InvalidLength {
                expected,
                actual: words.len(),
            });
        }
        let per_word = 64 / usize::from(bits);
        for (index, &word) in words.iter().enumerate() {
            let entries = per_word.min(len - index * per_word);
            let used_bits = entries * usize::from(bits);
            if used_bits < 64 && word >> used_bits != 0 {
                return Err(Error::NonCanonicalPadding);
            }
        }
        Ok(Self { bits, len, words })
    }

    pub(super) fn get(&self, index: usize) -> Option<u32> {
        let (word, shift) = self.position(index)?;
        Some(((self.words[word] >> shift) & self.mask()) as u32)
    }

    pub(super) fn set(&mut self, index: usize, value: u32) -> Result<u32, Error> {
        let (word, shift) = self.position(index).ok_or(Error::IndexOutOfBounds)?;
        let mask = self.mask();
        if u64::from(value) > mask {
            return Err(Error::ValueOutOfRange(value));
        }
        let previous = ((self.words[word] >> shift) & mask) as u32;
        self.words[word] = (self.words[word] & !(mask << shift)) | (u64::from(value) << shift);
        Ok(previous)
    }

    pub(super) fn words(&self) -> &[u64] {
        &self.words
    }

    pub(super) fn bits(&self) -> u8 {
        self.bits
    }

    pub(super) fn heap_bytes(&self) -> usize {
        self.words.capacity() * size_of::<u64>()
    }

    fn position(&self, index: usize) -> Option<(usize, usize)> {
        if index >= self.len {
            return None;
        }
        let per_word = 64 / usize::from(self.bits);
        Some((
            index / per_word,
            (index % per_word) * usize::from(self.bits),
        ))
    }

    fn mask(&self) -> u64 {
        (1_u64 << self.bits) - 1
    }
}

fn word_count(bits: u8, len: usize) -> Result<usize, Error> {
    if !(1..=31).contains(&bits) {
        return Err(Error::InvalidBits(bits));
    }
    if !(1..=MAX_LEN).contains(&len) {
        return Err(Error::InvalidLength {
            expected: len.clamp(1, MAX_LEN),
            actual: len,
        });
    }
    Ok(len.div_ceil(64 / usize::from(bits)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_dimensions_before_allocation() {
        for bits in [0, 32, 64, u8::MAX] {
            assert!(matches!(
                Packed::new(bits, 1, usize::MAX),
                Err(Error::InvalidBits(actual)) if actual == bits
            ));
            assert!(matches!(
                Packed::from_words(bits, 1, vec![]),
                Err(Error::InvalidBits(actual)) if actual == bits
            ));
        }
        for len in [0, 4097, usize::MAX] {
            assert!(matches!(
                Packed::new(1, len, usize::MAX),
                Err(Error::InvalidLength { actual, .. }) if actual == len
            ));
            assert!(matches!(
                Packed::from_words(1, len, vec![]),
                Err(Error::InvalidLength { actual, .. }) if actual == len
            ));
        }
    }

    #[test]
    fn budget_covers_the_retained_allocation() {
        assert!(matches!(
            Packed::new(5, 13, 15),
            Err(Error::AllocationBudgetExceeded)
        ));
        let packed = Packed::new(5, 13, 16).unwrap();
        assert_eq!(packed.words(), &[0, 0]);
        assert_eq!(packed.bits(), 5);
        assert_eq!(packed.get(13), None);
        assert_eq!(packed.heap_bytes(), 16);

        let mut words = Vec::with_capacity(8);
        words.push(0);
        let bytes = words.capacity() * size_of::<u64>();
        let packed = Packed::from_words(1, 1, words).unwrap();
        assert_eq!(packed.heap_bytes(), bytes);
    }

    #[test]
    fn requires_exact_word_count() {
        for actual in [0, 1, 3] {
            assert!(matches!(
                Packed::from_words(5, 13, vec![0; actual]),
                Err(Error::InvalidLength { expected: 2, actual: count }) if count == actual
            ));
        }
    }

    #[test]
    fn known_word_boundary_and_top_bit_values() {
        let mut packed = Packed::new(5, 14, 16).unwrap();
        packed.set(0, 31).unwrap();
        packed.set(11, 17).unwrap();
        packed.set(12, 3).unwrap();
        packed.set(13, 1).unwrap();
        assert_eq!(packed.words(), &[0x0880_0000_0000_001f, 0x23]);

        let mut packed = Packed::new(8, 9, 16).unwrap();
        packed.set(7, 0x80).unwrap();
        packed.set(8, 0xff).unwrap();
        assert_eq!(packed.words(), &[0x8000_0000_0000_0000, 0xff]);
        assert_eq!(packed.get(7), Some(0x80));

        let mut packed = Packed::new(31, 3, 16).unwrap();
        packed.set(0, 0x7fff_ffff).unwrap();
        packed.set(1, 0x7fff_ffff).unwrap();
        packed.set(2, 0x4000_0000).unwrap();
        assert_eq!(packed.words(), &[0x3fff_ffff_ffff_ffff, 0x4000_0000]);
    }

    #[test]
    fn invalid_set_leaves_storage_unchanged() {
        for bits in 1..=31 {
            let mut packed = Packed::new(bits, 64, usize::MAX).unwrap();
            packed.set(0, 1).unwrap();
            let original = packed.words().to_vec();
            for index in [64, usize::MAX] {
                assert_eq!(packed.get(index), None);
                assert!(matches!(packed.set(index, 0), Err(Error::IndexOutOfBounds)));
            }
            let invalid = 1_u32 << bits;
            assert!(matches!(
                packed.set(0, invalid),
                Err(Error::ValueOutOfRange(value)) if value == invalid
            ));
            assert_eq!(packed.words(), original);
        }
    }

    #[test]
    fn rejects_every_unused_bit_in_full_and_partial_words() {
        for bits in 1..=31 {
            let per_word = 64 / usize::from(bits);
            let len = per_word + 1;
            for (word_index, used) in [(0, per_word * usize::from(bits)), (1, usize::from(bits))] {
                for bit in used..64 {
                    let mut words = vec![0, 0];
                    words[word_index] = 1_u64 << bit;
                    assert!(
                        matches!(
                            Packed::from_words(bits, len, words),
                            Err(Error::NonCanonicalPadding)
                        ),
                        "bits={bits}, word={word_index}, bit={bit}"
                    );
                }
            }
        }
    }

    #[test]
    fn accepts_all_used_bits_including_bit_63() {
        for bits in 1..=31 {
            let per_word = 64 / usize::from(bits);
            let used = per_word * usize::from(bits);
            let word = u64::MAX >> (64 - used);
            let packed = Packed::from_words(bits, per_word, vec![word]).unwrap();
            for index in 0..per_word {
                assert_eq!(packed.get(index), Some((1_u32 << bits) - 1));
            }
        }
    }

    #[test]
    fn every_width_matches_dense_values_at_all_positions() {
        let mut random = 0x741b_ae31_082f_96cd_u64;
        for bits in 1..=31 {
            let per_word = 64 / usize::from(bits);
            let mask = (1_u32 << bits) - 1;
            for len in [1, 2, per_word - 1, per_word, per_word + 1, 64, 4096] {
                let mut packed = Packed::new(bits, len, usize::MAX).unwrap();
                let mut dense = vec![0; len];
                for (index, value) in dense.iter_mut().enumerate() {
                    assert_eq!(packed.get(index), Some(0));
                    *value = mask;
                    assert_eq!(packed.set(index, *value).unwrap(), 0);
                }
                for _ in 0..len * 3 {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    let index = random as usize % len;
                    let value = (random >> 32) as u32 & mask;
                    let previous = std::mem::replace(&mut dense[index], value);
                    assert_eq!(packed.set(index, value).unwrap(), previous);
                    assert_eq!(packed.get(index), Some(value));
                }
                for (index, &value) in dense.iter().enumerate() {
                    assert_eq!(
                        packed.get(index),
                        Some(value),
                        "bits={bits}, len={len}, index={index}"
                    );
                }
                assert!(Packed::from_words(bits, len, packed.words().to_vec()).is_ok());
                for (index, value) in dense.iter().enumerate() {
                    assert_eq!(packed.set(index, 0).unwrap(), *value);
                }
                assert!(packed.words().iter().all(|&word| word == 0));
            }
        }
    }
}
