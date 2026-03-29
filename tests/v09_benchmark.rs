// NEXCOMP v0.9 — Speed + Repeated Offsets benchmark

use nexcomp::lz77::{self, optimal, huffman, Lz77Encoder};
use std::time::Instant;

fn compress_opt(data: &[u8], iters: usize) -> Vec<u8> {
    let tokens = optimal::optimal_parse(data, iters);
    let huff = huffman::huffman_encode(&tokens);
    let mut out = Vec::with_capacity(4 + huff.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&huff);
    out
}

fn compress_greedy(data: &[u8]) -> Vec<u8> {
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(data);
    let huff = huffman::huffman_encode(&tokens);
    let mut out = Vec::with_capacity(4 + huff.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&huff);
    out
}

fn decompress(payload: &[u8]) -> Vec<u8> {
    let huff_data = &payload[4..];
    let tokens = huffman::huffman_decode(huff_data);
    lz77::lz77_decode(&tokens).expect("LZ77 decode failed")
}

#[test]
fn test_v9_lossless_corpus() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    for iters in [1, 2] {
        let compressed = compress_opt(&data, iters);
        let decompressed = decompress(&compressed);
        assert_eq!(data, decompressed, "Optimal×{iters} round-trip FAILED");
    }
}

#[test]
fn test_optimal_1iter_vs_gzip() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_opt(&data, 1);
    let bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
    eprintln!("Optimal×1: {:.3} bpb vs gzip-9: 1.524 bpb", bpb);
    // Report whether it beats gzip — don't assert since ×1 may not
}

#[test]
fn test_rep_match_ratio() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let tokens = optimal::optimal_parse(&data, 1);

    // Count how many matches reuse a recent offset
    let mut recent = [0u32; 3]; // 3-entry MRU cache
    let mut rep_matches = 0usize;
    let mut total_matches = 0usize;

    for token in &tokens {
        if let lz77::Token::Match { offset, .. } = token {
            total_matches += 1;
            if recent.contains(offset) {
                rep_matches += 1;
            }
            // Update MRU: shift and insert
            recent[2] = recent[1];
            recent[1] = recent[0];
            recent[0] = *offset;
        }
    }

    let ratio = rep_matches as f64 / total_matches.max(1) as f64;
    eprintln!("Rep-match analysis:");
    eprintln!("  Total matches:  {}", total_matches);
    eprintln!("  Rep-matches:    {} ({:.1}%)", rep_matches, ratio * 100.0);
    eprintln!("  Unique offsets:  {} ({:.1}%)", total_matches - rep_matches, (1.0 - ratio) * 100.0);

    // Estimate savings: rep-match costs ~3 bits vs ~15-25 bits for new offset
    let savings_bits = rep_matches as f64 * 12.0; // ~12 bits saved per rep-match
    let savings_bytes = (savings_bits / 8.0) as usize;
    eprintln!("  Estimated savings if rep-matches were free: ~{} bytes ({:.3} bpb)",
        savings_bytes, savings_bytes as f64 * 8.0 / data.len() as f64);
}

#[test]
fn test_v9_full_benchmark() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let input_size = data.len();
    let mb = input_size as f64 / 1_048_576.0;

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  NEXCOMP v0.9 — Speed + Rep-Offsets Analysis");
    eprintln!("{}\n", "=".repeat(70));

    // Greedy
    let t0 = Instant::now();
    let greedy = compress_greedy(&data);
    let gt = t0.elapsed().as_secs_f64();

    // Optimal ×1
    let t0 = Instant::now();
    let opt1 = compress_opt(&data, 1);
    let t1 = t0.elapsed().as_secs_f64();
    let d1 = decompress(&opt1);
    assert_eq!(data, d1);

    // Optimal ×2
    let t0 = Instant::now();
    let opt2 = compress_opt(&data, 2);
    let t2 = t0.elapsed().as_secs_f64();
    let d2 = decompress(&opt2);
    assert_eq!(data, d2);

    // Decompress time
    let t0 = Instant::now();
    let _ = decompress(&opt2);
    let dt = t0.elapsed().as_secs_f64();

    eprintln!("Compresor            Bytes       bpb    Comp MB/s  Decomp MB/s");
    eprintln!("{}", "-".repeat(65));
    eprintln!("xz -6            {:>10}     0.779", 327_820);
    eprintln!("bzip2 -9         {:>10}     1.206", 507_569);
    eprintln!("gzip -9          {:>10}     1.524", 641_307);
    eprintln!("{}", "-".repeat(65));
    eprintln!("Greedy (v0.7)    {:>10}     {:.3}     {:.2}",
        greedy.len(), greedy.len() as f64 * 8.0 / input_size as f64, mb / gt);
    eprintln!("Optimal×1        {:>10}     {:.3}     {:.2}       {:.1}",
        opt1.len(), opt1.len() as f64 * 8.0 / input_size as f64, mb / t1, mb / dt);
    eprintln!("Optimal×2        {:>10}     {:.3}     {:.2}       {:.1}",
        opt2.len(), opt2.len() as f64 * 8.0 / input_size as f64, mb / t2, mb / dt);
    eprintln!("{}", "-".repeat(65));

    // Cost of iteration 2
    let iter2_gain = opt1.len() as i64 - opt2.len() as i64;
    let iter2_bpb = iter2_gain as f64 * 8.0 / input_size as f64;
    eprintln!("\nIteration analysis:");
    eprintln!("  Iter 2 saves: {} bytes ({:.3} bpb)", iter2_gain, iter2_bpb);
    eprintln!("  Iter 2 costs: {:.2}s additional ({:.2} MB/s → {:.2} MB/s)",
        t2 - t1, mb / t1, mb / t2);
    eprintln!("  Worth it? {}", if iter2_gain > 5000 { "YES" } else { "MARGINAL" });
}
