use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};

fn bench_repair(c: &mut Criterion) {
    let mut group = c.benchmark_group("Re-Pair");

    for &(name, repeat) in &[("small_1KB", 100), ("medium_64KB", 6000), ("large_1MB", 90000)] {
        let data: Vec<u8> = "the quick brown fox jumps over the lazy dog. "
            .repeat(repeat)
            .into_bytes();

        group.bench_with_input(
            BenchmarkId::new("encode", name),
            &data,
            |b, data| {
                b.iter(|| {
                    black_box(nexcomp::grammar::repair_encode(black_box(data)).unwrap());
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("decode", name),
            &data,
            |b, data| {
                let result = nexcomp::grammar::repair_encode(data).unwrap();
                b.iter(|| {
                    black_box(nexcomp::grammar::repair_decode(black_box(&result)));
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_repair);
criterion_main!(benches);
