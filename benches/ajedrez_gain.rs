// benches/ajedrez_gain.rs
// Mide la ganancia real de separar P|I vs comprimir unificado
// Ejecutar con: cargo bench --bench ajedrez_gain

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use nexcomp::ajedrez;
use nexcomp::transform::apply_transform;
use nexcomp::classifier::DomainType;

fn benchmark_split_merge(c: &mut Criterion) {
    // 64KB of realistic post-BWT+MTF-like data (skewed toward 0x00)
    let data: Vec<u8> = (0..65536u32)
        .map(|i| {
            let h = i.wrapping_mul(2654435761) >> 16;
            if h % 5 == 0 { (h & 0x1F) as u8 } else { 0x00 }
        })
        .collect();

    c.bench_function("ajedrez::split 64KB", |b| {
        b.iter(|| {
            black_box(ajedrez::split(black_box(&data), ajedrez::BLOCK_WIDTH));
        });
    });

    let streams = ajedrez::split(&data, ajedrez::BLOCK_WIDTH);
    c.bench_function("ajedrez::merge 64KB", |b| {
        b.iter(|| {
            black_box(ajedrez::merge(black_box(&streams), ajedrez::BLOCK_WIDTH));
        });
    });
}

fn benchmark_entropy_gain(c: &mut Criterion) {
    // Create a realistic post-BWT+MTF stream from English text
    let text = "The quick brown fox jumps over the lazy dog. \
                Data compression is the dual of prediction. \
                The Burrows-Wheeler Transform concentrates runs. "
        .repeat(600);
    let text_bytes = text.as_bytes();

    // Apply BWT+MTF transform (what the real pipeline does before Re-Pair)
    let transformed = apply_transform(text_bytes, DomainType::Text);

    // Measure gain on the transformed stream
    let report = ajedrez::measure_gain(&transformed);

    println!();
    println!("Ajedrez Gain Analysis");
    println!("─────────────────────────────────────────────");
    println!("Stream size:       {:>6} bytes (post-BWT+MTF)", transformed.len());
    println!("Entropy unified:   {:.2} bpb", report.entropy_unified);
    println!("Entropy P:         {:.2} bpb", report.entropy_p);
    println!("Entropy I:         {:.2} bpb", report.entropy_i);
    println!("─────────────────────────────────────────────");
    println!("Size unified:      {:>6} bytes (estimado rANS)", report.size_unified);
    println!("Size P+I:          {:>6} bytes (estimado rANS)", report.size_p + report.size_i);
    println!("Overhead modelos:  {:>6} bytes (dos freq tables)", report.overhead_models);
    println!("Net gain:          {:>+6} bytes ({:+.1}%)", report.net_gain_bytes, report.net_gain_pct);
    println!(
        "Vale la pena:      {}",
        if report.worth_applying { "SI" } else { "NO" }
    );
    println!("─────────────────────────────────────────────");

    // Also test on the actual tar corpus if available
    if let Ok(tar_data) = std::fs::read("/tmp/nexcomp_bench/testdata.tar") {
        // Simulate pipeline: classify first block, transform, measure
        let block = &tar_data[..tar_data.len().min(65536)];
        let domain = nexcomp::classifier::classify_block(block).unwrap_or(DomainType::BinaryGeneric);
        let t = apply_transform(block, domain);
        let r = ajedrez::measure_gain(&t);
        println!();
        println!("Tar block 0 ({:?}):", domain);
        println!("  Entropy: unified={:.2}, P={:.2}, I={:.2}", r.entropy_unified, r.entropy_p, r.entropy_i);
        println!("  Net gain: {:+} bytes, worth: {}", r.net_gain_bytes, r.worth_applying);
    }

    // Bench the measure_gain function itself
    c.bench_function("ajedrez::measure_gain 64KB", |b| {
        b.iter(|| {
            black_box(ajedrez::measure_gain(black_box(&transformed)));
        });
    });
}

criterion_group!(benches, benchmark_split_merge, benchmark_entropy_gain);
criterion_main!(benches);
