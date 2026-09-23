use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};

// Note: these benches require the nexcomp crate to export the entropy module.
// Add `pub mod entropy;` to lib.rs for benchmark access.

fn bench_rans_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("rANS");

    for size in [1024, 65536, 1_048_576] {
        // Generate test data with realistic English-like byte distribution
        let data: Vec<u8> = (0..size)
            .map(|i| {
                // printable ASCII range
                ((i * 7 + 13) % 96 + 32) as u8
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("encode", size),
            &data,
            |b, data| {
                // Pre-compute frequency table
                let mut counts = vec![0u64; 256];
                for &byte in data.iter() {
                    counts[byte as usize] += 1;
                }
                let freqs = nexcomp::entropy::normalize_freqs(&counts, 256);
                let table = nexcomp::entropy::build_table(&freqs).unwrap();

                b.iter(|| {
                    black_box(nexcomp::entropy::rans_encode(black_box(data), &table).unwrap());
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("decode", size),
            &data,
            |b, data| {
                let mut counts = vec![0u64; 256];
                for &byte in data.iter() {
                    counts[byte as usize] += 1;
                }
                let freqs = nexcomp::entropy::normalize_freqs(&counts, 256);
                let table = nexcomp::entropy::build_table(&freqs).unwrap();
                let dtable = nexcomp::entropy::build_decode_table(&table);
                let encoded = nexcomp::entropy::rans_encode(data, &table).unwrap();

                b.iter(|| {
                    black_box(
                        nexcomp::entropy::rans_decode(black_box(&encoded), &dtable, data.len())
                            .unwrap(),
                    );
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_rans_roundtrip);
criterion_main!(benches);
