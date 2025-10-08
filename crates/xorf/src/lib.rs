//! Simplified BinaryFuse16 filter implementation.
use core::convert::TryFrom;
use libm::{floor, fmax, log, round};
use rand::Rng;
use twox_hash::XxHash3_64 as XxHash;

pub trait GenKey {
    fn gen_key(&self) -> u64;
}

// 数字类型：直接转换/扩展
macro_rules! impl_int {
    ($($t:ty),* $(,)?) => {
        $(impl GenKey for $t {
            #[inline]
            fn gen_key(&self) -> u64 { *self as u64 }
        })*
    };
}
impl_int!(u8, u16, u32, u64, usize);

macro_rules! impl_str {
    ($($t:ty),* $(,)?) => {
        $(impl GenKey for $t {
            #[inline]
            fn gen_key(&self) -> u64 { XxHash::oneshot(self.as_ref()) }
        })*
    };
}
impl_str!(&str, &String, String);

const fn mix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.overflowing_mul(0xff51_afd7_ed55_8ccd).0;
    k ^= k >> 33;
    k = k.overflowing_mul(0xc4ce_b9fe_1a85_ec53).0;
    k ^= k >> 33;
    k
}

const fn splitmix64(seed: &mut u64) -> u64 {
    *seed = (*seed).overflowing_add(0x9e37_79b9_7f4a_7c15).0;
    let mut z = *seed;
    z = (z ^ (z >> 30)).overflowing_mul(0xbf58_476d_1ce4_e5b9).0;
    z = (z ^ (z >> 27)).overflowing_mul(0x94d0_49bb_1331_11eb).0;
    z ^ (z >> 31)
}

const fn mix(key: u64, seed: u64) -> u64 {
    mix64(key.overflowing_add(seed).0)
}

// Binary fuse helper functions
fn segment_length(arity: u32, size: u32) -> u32 {
    if size == 0 {
        return 4;
    }
    match arity {
        3 => 1 << (floor(log(size as f64) / log(3.33_f64) + 2.25) as u32),
        4 => 1 << (floor(log(size as f64) / log(2.91_f64) - 0.5) as u32),
        _ => 65536,
    }
}

fn size_factor(arity: u32, size: u32) -> f64 {
    match arity {
        3 => fmax(
            1.125_f64,
            0.875 + 0.25 * log(1000000_f64) / log(size as f64),
        ),
        4 => fmax(1.075_f64, 0.77 + 0.305 * log(600000_f64) / log(size as f64)),
        _ => 2.0,
    }
}

const fn hash_of_hash(
    hash: u64,
    segment_length: u32,
    segment_length_mask: u32,
    segment_count_length: u32,
) -> (u32, u32, u32) {
    let hi = ((hash as u128 * segment_count_length as u128) >> 64) as u64;
    let h0 = hi as u32;
    let mut h1 = h0 + segment_length;
    let mut h2 = h1 + segment_length;
    h1 ^= ((hash >> 18) as u32) & segment_length_mask;
    h2 ^= (hash as u32) & segment_length_mask;
    (h0, h1, h2)
}

const fn mod3(x: u8) -> u8 {
    if x > 2 { x - 3 } else { x }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub seed: u64,
    pub segment_length: u32,
    pub segment_length_mask: u32,
    pub segment_count_length: u32,
}

impl Descriptor {
    pub const LEN: usize = 20;
}

/// A `BinaryFuse16` filter is an Xor-like filter with 16-bit fingerprints arranged in a binary-partitioned fuse graph.
///
/// A `BinaryFuse16` filter uses ≈18 bits per entry of the set is it constructed from, and has a false
/// positive rate of ≈2^-16 (<0.002%). As with other probabilistic filters, a higher number of entries decreases
/// the bits per entry but increases the false positive rate.
///
/// A `BinaryFuse16` is constructed from a set of 64-bit unsigned integers and is immutable.
/// Construction may fail, but usually only if there are duplicate keys.
#[derive(Debug, Clone)]
pub struct BinaryFuse16 {
    descriptor: Descriptor,
    pub fingerprints: Box<[u16]>,
}

impl BinaryFuse16 {
    /// Returns `true` if the filter contains the specified key.
    /// Has a false positive rate of <0.4%.
    /// Has no false negatives.
    pub fn contains<K: GenKey>(&self, key: &K) -> bool {
        self.contains_u64(key.gen_key())
    }

    fn contains_u64(&self, key: u64) -> bool {
        let hash = mix(key, self.descriptor.seed);
        let mut f = (hash ^ (hash >> 32)) as u16;
        let (h0, h1, h2) = hash_of_hash(
            hash,
            self.descriptor.segment_length,
            self.descriptor.segment_length_mask,
            self.descriptor.segment_count_length,
        );
        f ^= self.fingerprints[h0 as usize]
            ^ self.fingerprints[h1 as usize]
            ^ self.fingerprints[h2 as usize];
        f == 0
    }

    /// Returns the number of fingerprints in the filter.
    pub fn len(&self) -> usize {
        self.fingerprints.len()
    }

    /// Returns true if the filter is empty.
    pub fn is_empty(&self) -> bool {
        self.fingerprints.is_empty()
    }

    /// Try to construct the filter from a key iterator.
    pub fn try_from_iterator<T>(keys: T) -> Result<Self, &'static str>
    where
        T: ExactSizeIterator<Item = u64> + Clone,
    {
        let size: usize = keys.len();

        // Handle empty set case
        if size == 0 {
            return Ok(Self {
                descriptor: Descriptor {
                    seed: 0,
                    segment_length: 0,
                    segment_length_mask: 0,
                    segment_count_length: 0,
                },
                fingerprints: Box::new([]),
            });
        }

        // Check for duplicates in debug mode
        #[cfg(debug_assertions)]
        {
            let mut s = std::collections::BTreeSet::new();
            let all_distinct = keys.clone().all(|x| s.insert(x));
            if !all_distinct {
                panic!(
                    "Binary Fuse filters must be constructed from a collection containing all distinct keys."
                );
            }
        }

        let arity = 3u32;
        let segment_length: u32 = segment_length(arity, size as u32).min(262144);
        let segment_length_mask: u32 = segment_length - 1;
        let size_factor: f64 = size_factor(arity, size as u32);
        let capacity: u32 = if size > 1 {
            round(size as f64 * size_factor) as u32
        } else {
            0
        };
        let init_segment_count = capacity.div_ceil(segment_length);
        let (fp_array_len, segment_count) = {
            let array_len = init_segment_count * segment_length;
            let segment_count: u32 = {
                let proposed = array_len.div_ceil(segment_length);
                if proposed < arity {
                    1
                } else {
                    proposed - (arity - 1)
                }
            };
            let array_len: u32 = (segment_count + arity - 1) * segment_length;
            (array_len as usize, segment_count)
        };
        let segment_count_length = segment_count * segment_length;

        // Initialize fingerprints with random values for better uniformity
        let mut fingerprints: Box<[u16]> = {
            let mut block = vec![0; fp_array_len];
            rand::thread_rng().fill(block.as_mut_slice());
            block.into_boxed_slice()
        };

        let mut rng = 1;
        let mut seed = splitmix64(&mut rng);
        let capacity = fingerprints.len();
        let mut alone: Box<[u32]> = vec![0u32; capacity].into_boxed_slice();
        let mut t2count: Box<[u8]> = vec![0u8; capacity].into_boxed_slice();
        let mut t2hash: Box<[u64]> = vec![0u64; capacity].into_boxed_slice();
        let mut reverse_h: Box<[u8]> = vec![0u8; size].into_boxed_slice();
        let size_plus_1: usize = size + 1;
        let mut reverse_order: Box<[u64]> = vec![0u64; size_plus_1].into_boxed_slice();
        reverse_order[size] = 1;

        let block_bits = {
            let mut block_bits = 1;
            while (1 << block_bits) < segment_count {
                block_bits += 1;
            }
            block_bits
        };

        let start_pos_len: usize = 1 << block_bits;
        let mut start_pos: Box<[usize]> = vec![0usize; start_pos_len].into_boxed_slice();
        let mut h012: [u32; 6] = [0; 6];
        let mut done = false;
        let mut ultimate_size = 0;

        const MAX_ITER: usize = 1000;
        for _ in 0..MAX_ITER {
            for i in 0..start_pos_len {
                start_pos[i] = (((i as u64) * (size as u64)) >> block_bits) as usize;
            }
            for key in keys.clone() {
                let hash = mix(key, seed);
                let mut segment_index = hash >> (64 - block_bits);
                while reverse_order[start_pos[segment_index as usize] as usize] != 0 {
                    segment_index += 1;
                    segment_index &= (1 << block_bits) - 1;
                }
                reverse_order[start_pos[segment_index as usize] as usize] = hash;
                start_pos[segment_index as usize] += 1;
            }

            let mut error = false;
            let mut duplicates = 0;
            for i in 0..size {
                let hash = reverse_order[i];
                let (index1, index2, index3) = hash_of_hash(
                    hash,
                    segment_length,
                    segment_length_mask,
                    segment_count_length,
                );
                let (index1, index2, index3) = (index1 as usize, index2 as usize, index3 as usize);
                t2count[index1] += 4;
                t2hash[index1] ^= hash;
                t2count[index2] += 4;
                t2count[index2] ^= 1;
                t2hash[index2] ^= hash;
                t2count[index3] += 4;
                t2count[index3] ^= 2;
                t2hash[index3] ^= hash;

                if t2hash[index1] & t2hash[index2] & t2hash[index3] == 0
                    && (((t2hash[index1] == 0) && (t2count[index1] == 8))
                        || ((t2hash[index2] == 0) && (t2count[index2] == 8))
                        || ((t2hash[index3] == 0) && (t2count[index3] == 8)))
                {
                    duplicates += 1;
                    t2count[index1] -= 4;
                    t2hash[index1] ^= hash;
                    t2count[index2] -= 4;
                    t2count[index2] ^= 1;
                    t2hash[index2] ^= hash;
                    t2count[index3] -= 4;
                    t2count[index3] ^= 2;
                    t2hash[index3] ^= hash;
                }
                error = t2count[index1] < 4 || t2count[index2] < 4 || t2count[index3] < 4;
            }
            if error {
                continue;
            }

            // Key addition complete. Perform enqueing.
            let mut qsize = 0;
            for i in 0..capacity {
                alone[qsize] = i as u32;
                if (t2count[i] >> 2) == 1 {
                    qsize += 1;
                }
            }
            let mut stack_size = 0;
            while qsize > 0 {
                qsize -= 1;
                let index = alone[qsize] as usize;
                if (t2count[index] >> 2) == 1 {
                    let hash = t2hash[index];
                    let found: u8 = t2count[index] & 3;
                    reverse_h[stack_size] = found;
                    reverse_order[stack_size] = hash;
                    stack_size += 1;

                    let (index1, index2, index3) = hash_of_hash(
                        hash,
                        segment_length,
                        segment_length_mask,
                        segment_count_length,
                    );

                    h012[1] = index2;
                    h012[2] = index3;
                    h012[3] = index1;
                    h012[4] = h012[1];

                    let other_index1 = h012[(found + 1) as usize] as usize;
                    alone[qsize] = other_index1 as u32;
                    if (t2count[other_index1] >> 2) == 2 {
                        qsize += 1;
                    }
                    t2count[other_index1] -= 4;
                    t2count[other_index1] ^= mod3(found + 1);
                    t2hash[other_index1] ^= hash;

                    let other_index2 = h012[(found + 2) as usize] as usize;
                    alone[qsize] = other_index2 as u32;
                    if (t2count[other_index2] >> 2) == 2 {
                        qsize += 1;
                    }
                    t2count[other_index2] -= 4;
                    t2count[other_index2] ^= mod3(found + 2);
                    t2hash[other_index2] ^= hash;
                }
            }

            if stack_size + duplicates == size {
                ultimate_size = stack_size;
                done = true;
                break;
            }

            // Filter failed to be created; reset for a retry.
            for i in 0..size {
                reverse_order[i] = 0;
            }
            for i in 0..capacity {
                t2count[i] = 0;
                t2hash[i] = 0;
            }
            seed = splitmix64(&mut rng);
        }
        if !done {
            return Err("Failed to construct binary fuse filter.");
        }

        // Construct all fingerprints
        let size = ultimate_size;
        for i in (0..size).rev() {
            let hash = reverse_order[i];
            let xor2 = (hash ^ (hash >> 32)) as u16;
            let (index1, index2, index3) = hash_of_hash(
                hash,
                segment_length,
                segment_length_mask,
                segment_count_length,
            );
            let found = reverse_h[i] as usize;
            h012[0] = index1;
            h012[1] = index2;
            h012[2] = index3;
            h012[3] = h012[0];
            h012[4] = h012[1];
            fingerprints[h012[found] as usize] = xor2
                ^ fingerprints[h012[found + 1] as usize]
                ^ fingerprints[h012[found + 2] as usize];
        }

        Ok(Self {
            descriptor: Descriptor {
                seed,
                segment_length,
                segment_length_mask,
                segment_count_length,
            },
            fingerprints,
        })
    }

    /// Deserialize a `BinaryFuse16` from a byte slice.
    ///
    /// | len | Descriptor | Fingerprints |
    /// |-----|------------|--------------|
    /// |   2 |         20 |      len * 2 |
    pub fn deserialize(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 22 {
            return Err("Input too short for BinaryFuse16");
        }

        let mut offset = 0;

        // len (2 bytes, u16)
        let len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        offset += 2;

        // Check total expected size
        let expected_size = 2 + Descriptor::LEN + (len as usize * 2);
        if bytes.len() != expected_size {
            return Err("Invalid BinaryFuse16 data length");
        }

        // Descriptor (20 bytes)
        let seed = u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .map_err(|_| "Failed to read seed")?,
        );
        offset += 8;

        let segment_length = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| "Failed to read segment_length")?,
        );
        offset += 4;

        let segment_length_mask = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| "Failed to read segment_length_mask")?,
        );
        offset += 4;

        let segment_count_length = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| "Failed to read segment_count_length")?,
        );
        offset += 4;

        // Fingerprints (len * 2 bytes)
        let mut fingerprints = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let fingerprint = u16::from_le_bytes(
                bytes[offset..offset + 2]
                    .try_into()
                    .map_err(|_| "Failed to read fingerprint")?,
            );
            fingerprints.push(fingerprint);
            offset += 2;
        }

        Ok(BinaryFuse16 {
            descriptor: Descriptor {
                seed,
                segment_length,
                segment_length_mask,
                segment_count_length,
            },
            fingerprints: fingerprints.into_boxed_slice(),
        })
    }

    /// Serialize a `BinaryFuse16` to a byte slice.
    ///
    /// | len | Descriptor | Fingerprints |
    /// |-----|------------|--------------|
    /// |   2 |         20 |      len * 2 |
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // len (2 bytes, u16)
        let len = self.fingerprints.len() as u16;
        bytes.extend_from_slice(&len.to_le_bytes());

        // Descriptor (20 bytes)
        bytes.extend_from_slice(&self.descriptor.seed.to_le_bytes()); // 8 bytes
        bytes.extend_from_slice(&self.descriptor.segment_length.to_le_bytes()); // 4 bytes
        bytes.extend_from_slice(&self.descriptor.segment_length_mask.to_le_bytes()); // 4 bytes
        bytes.extend_from_slice(&self.descriptor.segment_count_length.to_le_bytes()); // 4 bytes

        // Fingerprints (len * 2 bytes)
        for fingerprint in self.fingerprints.iter() {
            bytes.extend_from_slice(&fingerprint.to_le_bytes());
        }

        bytes
    }
}

impl<K: GenKey> TryFrom<&[K]> for BinaryFuse16 {
    type Error = &'static str;

    fn try_from(keys: &[K]) -> Result<Self, Self::Error> {
        let keys = keys.iter().map(K::gen_key);
        Self::try_from_iterator(keys)
    }
}

impl<K: GenKey> TryFrom<&Vec<K>> for BinaryFuse16 {
    type Error = &'static str;

    fn try_from(keys: &Vec<K>) -> Result<Self, Self::Error> {
        let keys = keys.iter().map(K::gen_key);
        Self::try_from_iterator(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialization() {
        const SAMPLE_SIZE: usize = 1_000_000;
        let keys: Vec<u64> = (0..SAMPLE_SIZE).map(|i| i as u64).collect();

        let filter = BinaryFuse16::try_from(&keys).unwrap();

        for key in keys {
            assert!(filter.contains(&key));
        }
    }

    #[test]
    fn test_bits_per_entry() {
        const SAMPLE_SIZE: usize = 1_000_000;
        let keys: Vec<u64> = (0..SAMPLE_SIZE).map(|i| i as u64).collect();

        let filter = BinaryFuse16::try_from(&keys).unwrap();
        let bpe = (filter.len() as f64) * 16.0 / (SAMPLE_SIZE as f64);

        assert!(bpe < 18.1, "Bits per entry is {}", bpe);
    }

    #[test]
    fn test_small_set() {
        let keys = vec![1u64, 2u64, 3u64];
        let filter = BinaryFuse16::try_from(&keys).unwrap();

        for key in &keys {
            assert!(filter.contains(key));
        }
    }

    #[test]
    fn test_empty_set() {
        let keys: Vec<u64> = vec![];
        let filter = BinaryFuse16::try_from(&keys).unwrap();
        assert!(filter.is_empty());
    }

    #[test]
    fn test_string_set() {
        let keys = vec!["1u64".to_string(), "2u64".to_string(), "3u64".to_string()];
        let filter = BinaryFuse16::try_from(keys.as_slice()).unwrap();

        for key in &keys {
            assert!(filter.contains(key));
        }
    }

    #[test]
    fn test_serialization() {
        let keys = vec![1u64, 2u64, 3u64];
        let filter = BinaryFuse16::try_from(&keys).unwrap();

        let serialized = filter.serialize();
        let deserialized = BinaryFuse16::deserialize(serialized.as_slice()).unwrap();

        for key in &keys {
            assert!(deserialized.contains(key));
        }
    }
}
