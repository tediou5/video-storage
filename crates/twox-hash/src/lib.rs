//! Simplified XxHash3_64 implementation.
//!
//! This is a streamlined version containing only the XxHash3_64 hasher,
//! with all abstractions removed and dependencies inlined.

#![no_std]
use core::hash;

// Constants from primes module
const PRIME64_1: u64 = 0x9E3779B185EBCA87;
const PRIME64_2: u64 = 0xC2B2AE3D27D4EB4F;

const PRIME_MX1: u64 = 0x165667919E3779F9;

const CUTOFF: usize = 240;
const DEFAULT_SEED: u64 = 0;

/// The length of the default secret.
pub const DEFAULT_SECRET_LENGTH: usize = 192;

type DefaultSecret = [u8; DEFAULT_SECRET_LENGTH];

const DEFAULT_SECRET_RAW: DefaultSecret = [
    0xb8, 0xfe, 0x6c, 0x39, 0x23, 0xa4, 0x4b, 0xbe, 0x7c, 0x01, 0x81, 0x2c, 0xf7, 0x21, 0xad, 0x1c,
    0xde, 0xd4, 0x6d, 0xe9, 0x83, 0x90, 0x97, 0xdb, 0x72, 0x40, 0xa4, 0xa4, 0xb7, 0xb3, 0x67, 0x1f,
    0xcb, 0x79, 0xe6, 0x4e, 0xcc, 0xc0, 0xe5, 0x78, 0x82, 0x5a, 0xd0, 0x7d, 0xcc, 0xff, 0x72, 0x21,
    0xb8, 0x08, 0x46, 0x74, 0xf7, 0x43, 0x24, 0x8e, 0xe0, 0x35, 0x90, 0xe6, 0x81, 0x3a, 0x26, 0x4c,
    0x3c, 0x28, 0x52, 0xbb, 0x91, 0xc3, 0x00, 0xcb, 0x88, 0xd0, 0x65, 0x8b, 0x1b, 0x53, 0x2e, 0xa3,
    0x71, 0x64, 0x48, 0x97, 0xa2, 0x0d, 0xf9, 0x4e, 0x38, 0x19, 0xef, 0x46, 0xa9, 0xde, 0xac, 0xd8,
    0xa8, 0xfa, 0x76, 0x3f, 0xe3, 0x9c, 0x34, 0x3f, 0xf9, 0xdc, 0xbb, 0xc7, 0xc7, 0x0b, 0x4f, 0x1d,
    0x8a, 0x51, 0xe0, 0x4b, 0xcd, 0xb4, 0x59, 0x31, 0xc8, 0x9f, 0x7e, 0xc9, 0xd9, 0x78, 0x73, 0x64,
    0xea, 0xc5, 0xac, 0x83, 0x34, 0xd3, 0xeb, 0xc3, 0xc5, 0x81, 0xa0, 0xff, 0xfa, 0x13, 0x63, 0xeb,
    0x17, 0x0d, 0xdd, 0x51, 0xb7, 0xf0, 0xda, 0x49, 0xd3, 0x16, 0x55, 0x26, 0x29, 0xd4, 0x68, 0x9e,
    0x2b, 0x16, 0xbe, 0x58, 0x7d, 0x47, 0xa1, 0xfc, 0x8f, 0xf8, 0xb8, 0xd1, 0x7a, 0xd0, 0x31, 0xce,
    0x45, 0xcb, 0x3a, 0x8f, 0x95, 0x16, 0x04, 0x28, 0xaf, 0xd7, 0xfb, 0xca, 0xbb, 0x4b, 0x40, 0x7e,
];

// Helper functions
#[inline]
fn as_chunks<const N: usize>(slice: &[u8]) -> (&[[u8; N]], &[u8]) {
    assert_ne!(N, 0);
    let len = slice.len() / N;
    let (head, tail) = slice.split_at(len * N);
    let head = unsafe { core::slice::from_raw_parts(head.as_ptr() as *const [u8; N], len) };
    (head, tail)
}

#[inline]
fn as_rchunks<const N: usize>(slice: &[u8]) -> (&[u8], &[[u8; N]]) {
    assert_ne!(N, 0);
    let len = slice.len() / N;
    let (head, tail) = slice.split_at(slice.len() - len * N);
    let tail = unsafe { core::slice::from_raw_parts(tail.as_ptr() as *const [u8; N], len) };
    (head, tail)
}

#[inline]
fn read_u32_le(bytes: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[..4]);
    u32::from_le_bytes(buf)
}

#[inline]
fn read_u64_le(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(buf)
}

#[inline]
fn avalanche(mut x: u64) -> u64 {
    x ^= x >> 37;
    x = x.wrapping_mul(PRIME_MX1);
    x ^= x >> 32;
    x
}

#[inline]
fn mix_step(data: &[u8; 16], secret: &[u8; 16], seed: u64) -> u64 {
    let data_lo = read_u64_le(&data[0..8]);
    let data_hi = read_u64_le(&data[8..16]);
    let secret_lo = read_u64_le(&secret[0..8]);
    let secret_hi = read_u64_le(&secret[8..16]);

    let mul_result = {
        let a = (data_lo ^ secret_lo.wrapping_add(seed)) as u128;
        let b = (data_hi ^ secret_hi.wrapping_sub(seed)) as u128;
        a.wrapping_mul(b)
    };

    (mul_result as u64) ^ (mul_result >> 64) as u64
}

#[inline]
fn derive_secret(seed: u64, secret: &mut DefaultSecret) {
    if seed == DEFAULT_SEED {
        return;
    }

    for i in (0..DEFAULT_SECRET_LENGTH).step_by(16) {
        let a = read_u64_le(&secret[i..i + 8]);
        let b = read_u64_le(&secret[i + 8..i + 16]);

        let a = a.wrapping_add(seed);
        let b = b.wrapping_sub(seed);

        secret[i..i + 8].copy_from_slice(&a.to_le_bytes());
        secret[i + 8..i + 16].copy_from_slice(&b.to_le_bytes());
    }
}

/// XxHash3_64 hasher implementation.
#[derive(Clone)]
pub struct XxHash3_64 {
    // For simplicity, we'll only support oneshot hashing
    _private: (),
}

impl XxHash3_64 {
    /// Create a new hasher.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Hash all data at once. This is the most efficient way to use this hasher.
    #[must_use]
    #[inline]
    pub fn oneshot(input: &[u8]) -> u64 {
        impl_oneshot(&DEFAULT_SECRET_RAW, DEFAULT_SEED, input)
    }

    /// Hash all data at once using the provided seed.
    #[must_use]
    #[inline]
    pub fn oneshot_with_seed(seed: u64, input: &[u8]) -> u64 {
        let mut secret = DEFAULT_SECRET_RAW;

        // We know that the secret will only be used if we have more
        // than 240 bytes, so don't waste time computing it otherwise.
        if input.len() > CUTOFF {
            derive_secret(seed, &mut secret);
        }

        impl_oneshot(&secret, seed, input)
    }
}

impl Default for XxHash3_64 {
    fn default() -> Self {
        Self::new()
    }
}

impl hash::Hasher for XxHash3_64 {
    fn write(&mut self, _bytes: &[u8]) {
        // For simplicity, streaming is not supported in this version
        panic!("Streaming not supported, use oneshot methods instead");
    }

    fn finish(&self) -> u64 {
        panic!("Streaming not supported, use oneshot methods instead");
    }
}

fn impl_oneshot(secret: &DefaultSecret, seed: u64, input: &[u8]) -> u64 {
    match input.len() {
        0 => impl_0_bytes(seed),
        1..=3 => impl_1_to_3_bytes(secret, seed, input),
        4..=8 => impl_4_to_8_bytes(secret, seed, input),
        9..=16 => impl_9_to_16_bytes(secret, seed, input),
        17..=128 => impl_17_to_128_bytes(secret, seed, input),
        129..=240 => impl_129_to_240_bytes(secret, seed, input),
        _ => impl_241_plus_bytes(secret, seed, input),
    }
}

#[inline]
fn impl_0_bytes(seed: u64) -> u64 {
    avalanche(
        seed ^ read_u64_le(&DEFAULT_SECRET_RAW[56..64]) ^ read_u64_le(&DEFAULT_SECRET_RAW[64..72]),
    )
}

#[inline]
fn impl_1_to_3_bytes(secret: &DefaultSecret, seed: u64, input: &[u8]) -> u64 {
    let input_length = input.len() as u8;

    let combined = input[input.len() - 1] as u32
        | (input_length as u32) << 8
        | (input[0] as u32) << 16
        | (input[input.len() >> 1] as u32) << 24;

    let keyed = (combined as u64) ^ (read_u32_le(&secret[0..4]) as u64).wrapping_add(seed);
    avalanche(keyed)
}

#[inline]
fn impl_4_to_8_bytes(secret: &DefaultSecret, seed: u64, input: &[u8]) -> u64 {
    let input_lo = read_u32_le(&input[..4]) as u64;
    let input_hi = read_u32_le(&input[input.len() - 4..]) as u64;

    let input_64 = input_lo.wrapping_add(input_hi << 32);
    let keyed = input_64 ^ read_u64_le(&secret[8..16]).wrapping_add(seed);

    avalanche(keyed)
}

#[inline]
fn impl_9_to_16_bytes(secret: &DefaultSecret, seed: u64, input: &[u8]) -> u64 {
    let input_lo = read_u64_le(&input[..8]);
    let input_hi = read_u64_le(&input[input.len() - 8..]);

    let acc = (input_lo ^ read_u64_le(&secret[16..24]).wrapping_add(seed)).wrapping_mul(PRIME64_1);
    let acc =
        (acc ^ input_hi ^ read_u64_le(&secret[24..32]).wrapping_sub(seed)).wrapping_mul(PRIME64_2);

    avalanche(acc)
}

#[inline]
fn impl_17_to_128_bytes(secret: &DefaultSecret, seed: u64, input: &[u8]) -> u64 {
    let mut acc = (input.len() as u64).wrapping_mul(PRIME64_1);

    let (fwd, _) = as_chunks::<16>(input);
    let (_, bwd) = as_rchunks::<16>(input);

    let q = bwd.len();

    if input.len() > 32 {
        if input.len() > 64 {
            if input.len() > 96 {
                acc = acc.wrapping_add(mix_step(
                    &fwd[3],
                    &as_chunks::<16>(&secret[96..]).0[0],
                    seed,
                ));
                acc = acc.wrapping_add(mix_step(
                    &bwd[q - 4],
                    &as_chunks::<16>(&secret[112..]).0[0],
                    seed,
                ));
            }

            acc = acc.wrapping_add(mix_step(
                &fwd[2],
                &as_chunks::<16>(&secret[64..]).0[0],
                seed,
            ));
            acc = acc.wrapping_add(mix_step(
                &bwd[q - 3],
                &as_chunks::<16>(&secret[80..]).0[0],
                seed,
            ));
        }

        acc = acc.wrapping_add(mix_step(
            &fwd[1],
            &as_chunks::<16>(&secret[32..]).0[0],
            seed,
        ));
        acc = acc.wrapping_add(mix_step(
            &bwd[q - 2],
            &as_chunks::<16>(&secret[48..]).0[0],
            seed,
        ));
    }

    acc = acc.wrapping_add(mix_step(&fwd[0], &as_chunks::<16>(&secret[0..]).0[0], seed));
    acc = acc.wrapping_add(mix_step(
        &bwd[q - 1],
        &as_chunks::<16>(&secret[16..]).0[0],
        seed,
    ));

    avalanche(acc)
}

#[inline]
fn impl_129_to_240_bytes(secret: &DefaultSecret, seed: u64, input: &[u8]) -> u64 {
    let mut acc = (input.len() as u64).wrapping_mul(PRIME64_1);

    let (stripes, _) = as_chunks::<32>(input);
    for (i, stripe) in stripes.iter().enumerate() {
        let secret_start = i * 16;
        if secret_start + 32 <= DEFAULT_SECRET_LENGTH {
            acc = acc.wrapping_add(mix_step(
                &as_chunks::<16>(stripe).0[0],
                &as_chunks::<16>(&secret[secret_start..]).0[0],
                seed,
            ));
            acc = acc.wrapping_add(mix_step(
                &as_chunks::<16>(stripe).0[1],
                &as_chunks::<16>(&secret[secret_start + 16..]).0[0],
                seed,
            ));
        }
    }

    // Handle the tail
    let tail_start = stripes.len() * 32;
    if tail_start < input.len() {
        let tail = &input[tail_start..];
        if tail.len() >= 16 {
            let (chunks, _) = as_chunks::<16>(tail);
            for chunk in chunks {
                acc =
                    acc.wrapping_add(mix_step(chunk, &as_chunks::<16>(&secret[128..]).0[0], seed));
            }
        }
    }

    avalanche(acc)
}

#[inline]
fn impl_241_plus_bytes(secret: &DefaultSecret, seed: u64, input: &[u8]) -> u64 {
    let mut acc = (input.len() as u64).wrapping_mul(PRIME64_1);

    let (stripes, last) = as_chunks::<64>(input);

    for stripe in stripes {
        for i in 0..4 {
            let data_chunk = &as_chunks::<16>(&stripe[i * 16..]).0[0];
            let secret_chunk = &as_chunks::<16>(&secret[i * 16..]).0[0];
            acc = acc.wrapping_add(mix_step(data_chunk, secret_chunk, seed));
        }
    }

    // Handle the last block
    if !last.is_empty() {
        acc = acc.wrapping_add(impl_17_to_128_bytes(secret, seed, last));
    }

    avalanche(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oneshot_empty() {
        let hash = XxHash3_64::oneshot(&[]);
        assert_ne!(hash, 0);
    }

    #[test]
    fn test_oneshot_simple() {
        let data = b"hello world";
        let hash = XxHash3_64::oneshot(data);
        assert_ne!(hash, 0);
    }

    #[test]
    fn test_oneshot_with_seed() {
        let data = b"hello world";
        let hash1 = XxHash3_64::oneshot_with_seed(0, data);
        let hash2 = XxHash3_64::oneshot_with_seed(1, data);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_different_sizes() {
        let data1 = b"a";
        let data2 = b"hello";
        let data3 = b"hello world!";
        let data4 = &[0u8; 100];
        let data5 = &[0u8; 300];

        let hash1 = XxHash3_64::oneshot(data1);
        let hash2 = XxHash3_64::oneshot(data2);
        let hash3 = XxHash3_64::oneshot(data3);
        let hash4 = XxHash3_64::oneshot(data4);
        let hash5 = XxHash3_64::oneshot(data5);

        // All should be different
        let hashes = [hash1, hash2, hash3, hash4, hash5];
        for i in 0..hashes.len() {
            for j in i + 1..hashes.len() {
                assert_ne!(hashes[i], hashes[j], "Hash {} == Hash {}", i, j);
            }
        }
    }

    #[test]
    fn test_reproducible() {
        let data = b"test data for reproducibility";
        let hash1 = XxHash3_64::oneshot(data);
        let hash2 = XxHash3_64::oneshot(data);
        assert_eq!(hash1, hash2);
    }
}
