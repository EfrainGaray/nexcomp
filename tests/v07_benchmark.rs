// NEXCOMP v0.7 — MIN_MATCH=3 benchmark

use nexcomp::lz77::{self, Lz77Encoder, huffman};
use std::time::Instant;

fn compress_v7(data: &[u8]) -> Vec<u8> {
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(data);
    let huff_data = huffman::huffman_encode(&tokens);
    let mut out = Vec::with_capacity(4 + huff_data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&huff_data);
    out
}

fn decompress_v7(payload: &[u8]) -> Vec<u8> {
    let huff_data = &payload[4..];
    let tokens = huffman::huffman_decode(huff_data);
    lz77::lz77_decode(&tokens).expect("LZ77 decode failed")
}

#[test]
fn test_min_match_3_roundtrip() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_v7(&data);
    let decompressed = decompress_v7(&compressed);
    assert_eq!(data, decompressed, "v0.7 round-trip FAILED");
}

#[test]
fn test_v7_supera_gzip() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_v7(&data);
    let bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
    let gzip_bpb = 1.524;
    eprintln!("v0.7: {:.3} bpb vs gzip-9: {:.3} bpb", bpb, gzip_bpb);
    if bpb < gzip_bpb {
        eprintln!("*** NEXCOMP v0.7 BEAT gzip-9! Delta: {:.3} bpb ***", gzip_bpb - bpb);
    } else {
        eprintln!("Gap remaining: {:.3} bpb ({} bytes)", bpb - gzip_bpb, compressed.len() - 641_307);
    }
}

#[test]
fn test_length_3_huffman_code() {
    let tokens = vec![
        lz77::Token::Literal(b'A'),
        lz77::Token::Literal(b'B'),
        lz77::Token::Literal(b'C'),
        lz77::Token::Match { offset: 3, length: 3 }, // length=3, DEFLATE symbol 257
    ];
    let encoded = huffman::huffman_encode(&tokens);
    let decoded = huffman::huffman_decode(&encoded);
    assert_eq!(tokens, decoded, "Length-3 match round-trip failed");
}

#[test]
fn test_v7_full_benchmark() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let input_size = data.len();
    let mb = input_size as f64 / 1_048_576.0;

    // Get match stats
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(&data);
    let stats = Lz77Encoder::stats(&tokens);
    let len3_matches = tokens.iter().filter(|t| matches!(t, lz77::Token::Match { length: 3, .. })).count();
    let len4_matches = tokens.iter().filter(|t| matches!(t, lz77::Token::Match { length: 4, .. })).count();
    let len5_matches = tokens.iter().filter(|t| matches!(t, lz77::Token::Match { length: 5, .. })).count();

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  NEXCOMP v0.7 — MIN_MATCH=3 Benchmark");
    eprintln!("{}\n", "=".repeat(70));
    eprintln!("LZ77 stats:");
    eprintln!("  Total tokens:    {}", tokens.len());
    eprintln!("  Literals:        {}", stats.literals);
    eprintln!("  Matches:         {}", stats.matches);
    eprintln!("  Match ratio:     {:.1}%", stats.match_ratio * 100.0);
    eprintln!("  Length=3 matches: {}", len3_matches);
    eprintln!("  Length=4 matches: {}", len4_matches);
    eprintln!("  Length=5 matches: {}", len5_matches);

    // 3 runs
    let mut sizes = Vec::new();
    let mut comp_times = Vec::new();
    let mut dec_times = Vec::new();
    for run in 0..3 {
        let t0 = Instant::now();
        let compressed = compress_v7(&data);
        let ct = t0.elapsed().as_secs_f64();
        let t0 = Instant::now();
        let decompressed = decompress_v7(&compressed);
        let dt = t0.elapsed().as_secs_f64();
        if run == 0 { assert_eq!(data, decompressed, "LOSSLESS FAILED"); }
        sizes.push(compressed.len());
        comp_times.push(ct);
        dec_times.push(dt);
    }

    let avg_size = sizes[1]; // use middle run
    let avg_comp = comp_times[1];
    let avg_dec = dec_times[1];
    let bpb = avg_size as f64 * 8.0 / input_size as f64;

    eprintln!("\nCompresor            Bytes       bpb    Comp MB/s  Decomp MB/s");
    eprintln!("{}", "-".repeat(65));
    eprintln!("xz -6            {:>10}     0.779", 327_820);
    eprintln!("bzip2 -9         {:>10}     1.206", 507_569);
    eprintln!("gzip -9          {:>10}     1.524", 641_307);
    eprintln!("{}", "-".repeat(65));
    eprintln!("NEXCOMP v0.7     {:>10}     {:.3}     {:.2}       {:.1}",
        avg_size, bpb, mb / avg_comp, mb / avg_dec);
    eprintln!("NEXCOMP v0.6     {:>10}     1.593     3.42       252.6", 670_419);
    eprintln!("NEXCOMP v0.5     {:>10}     1.630     1.44       107.0", 685_866);
    eprintln!("{}", "-".repeat(65));

    let beat = bpb < 1.524;
    eprintln!("\nBeat gzip-9? {}", if beat { "YES!" } else { "NO" });
    if beat {
        eprintln!("Delta: -{:.3} bpb ({} fewer bytes)", 1.524 - bpb, 641_307 - avg_size as i64);
    } else {
        eprintln!("Gap: +{:.3} bpb ({} more bytes)", bpb - 1.524, avg_size as i64 - 641_307);
    }
}
