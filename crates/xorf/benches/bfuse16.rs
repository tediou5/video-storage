#[macro_use]
extern crate criterion;
extern crate core;
extern crate rand;
extern crate xorf;

use core::convert::TryFrom;
use criterion::{BenchmarkId, Criterion};
use rand::Rng;
use xorf::BinaryFuse16;

const SAMPLE_SIZE: u32 = 500_000;

fn from(c: &mut Criterion) {
    let mut group = c.benchmark_group("BinaryFuse16");
    let group = group.sample_size(10);

    let mut keys = vec![0u64; SAMPLE_SIZE as usize];
    rand::thread_rng().fill(keys.as_mut_slice());

    group.bench_with_input(BenchmarkId::new("from", SAMPLE_SIZE), &keys, |b, keys| {
        b.iter(|| BinaryFuse16::try_from(keys).unwrap());
    });
}

fn contains(c: &mut Criterion) {
    let mut group = c.benchmark_group("BinaryFuse16");

    let mut rng = rand::thread_rng();
    let mut keys = vec![0u64; SAMPLE_SIZE as usize];
    rng.fill(keys.as_mut_slice());
    let filter = BinaryFuse16::try_from(&keys).unwrap();

    group.bench_function(BenchmarkId::new("contains", SAMPLE_SIZE), |b| {
        let key: u64 = rng.r#gen();
        b.iter(|| filter.contains(&key));
    });
}

criterion_group!(bfuse16, from, contains);
criterion_main!(bfuse16);
