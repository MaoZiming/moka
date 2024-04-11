use criterion::{black_box, criterion_group, criterion_main, Criterion};
use moka::sync::Cache;
use std::time::Duration;

pub fn insert_benchmark(c: &mut Criterion) {
    c.bench_function("cache_insert", |b| {
        let cache = Cache::builder().max_capacity(100_000).build();

        b.iter(|| {
            for i in 0..10_000 {
                cache.insert(i, i);
            }
        });
    });
}

pub fn invalidate_benchmark(c: &mut Criterion) {
    c.bench_function("cache_invalidate", |b| {
        let cache = Cache::builder().max_capacity(100_000).build();

        // Pre-populate the cache with items to invalidate.
        for i in 0..10_000 {
            cache.insert(i, i);
        }

        b.iter(|| {
            for i in 0..10_000 {
                cache.invalidate(&i);
            }
        });
    });
}

criterion_group!(benches, insert_benchmark, invalidate_benchmark);
criterion_main!(benches);
