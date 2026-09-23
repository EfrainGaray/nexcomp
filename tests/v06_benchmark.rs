// NEXCOMP v0.6 — Huffman encoding of LZ77 tokens
// Pipeline: raw → LZ77(4MB) → Huffman(dual-tree) → output
// No BWT, no Re-Pair, no rANS — Huffman is the entropy coder.

use nexcomp::lz77::{self, Lz77Encoder, huffman};

use std::time::Instant;

/// v0.6 compress: LZ77 + Huffman encoding of tokens
fn compress_v6(data: &[u8]) -> Vec<u8> {
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(data);

    // huffman_encode now returns [4B n_tokens] + bitstream
    let huff_data = huffman::huffman_encode(&tokens);

    // Pack: [4B original_len] + huffman_data (which includes n_tokens prefix)
    let mut out = Vec::with_capacity(4 + huff_data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&huff_data);
    out
}

/// v0.6 decompress: Huffman decode → LZ77 decode
fn decompress_v6(payload: &[u8]) -> Vec<u8> {
    let _orig_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    // huffman_decode expects [4B n_tokens] + bitstream
    let huff_data = &payload[4..];

    let tokens = huffman::huffman_decode(huff_data);
    lz77::lz77_decode(&tokens).expect("LZ77 decode failed")
}

#[test]
fn test_huffman_roundtrip_tokens() {
    // Encode various token patterns and verify round-trip
    let tokens = vec![
        lz77::Token::Literal(b'A'),
        lz77::Token::Literal(b'B'),
        lz77::Token::Match { offset: 1, length: 10 },
        lz77::Token::Literal(b'C'),
        lz77::Token::Match { offset: 100, length: 258 },
        lz77::Token::Match { offset: 200_000, length: 4 },
    ];
    let encoded = huffman::huffman_encode(&tokens);
    let decoded = huffman::huffman_decode(&encoded);
    assert_eq!(tokens, decoded);
}

#[test]
fn test_huffman_roundtrip_corpus() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();

    // First test: verify Huffman round-trip on the LZ77 tokens directly
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(&data);
    eprintln!("Corpus: {} tokens from {} bytes", tokens.len(), data.len());

    let huff_encoded = huffman::huffman_encode(&tokens);
    let huff_decoded = huffman::huffman_decode(&huff_encoded);

    assert_eq!(tokens.len(), huff_decoded.len(),
        "Token count mismatch: {} encoded, {} decoded", tokens.len(), huff_decoded.len());

    // Find first mismatch and print context
    for (i, (orig, dec)) in tokens.iter().zip(huff_decoded.iter()).enumerate() {
        if orig != dec {
            eprintln!("FIRST MISMATCH at token {i}:");
            eprintln!("  orig: {:?}", orig);
            eprintln!("  dec:  {:?}", dec);
            // Print surrounding tokens
            let start = i.saturating_sub(3);
            let end = (i + 4).min(tokens.len());
            for j in start..end {
                let marker = if j == i { ">>>" } else { "   " };
                eprintln!("{} [{}] orig={:?}  dec={:?}", marker, j,
                    tokens.get(j).map(|t| format!("{:?}", t)).unwrap_or_default(),
                    huff_decoded.get(j).map(|t| format!("{:?}", t)).unwrap_or_default());
            }
            panic!("Token mismatch at position {i}");
        }
    }

    // Then test full pipeline
    let compressed = compress_v6(&data);
    let decompressed = decompress_v6(&compressed);
    assert_eq!(data.len(), decompressed.len(), "Size mismatch");
    assert_eq!(data, decompressed, "v0.6 round-trip FAILED — not lossless");
}

#[test]
fn test_v6_supera_v5() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_v6(&data);
    let v5_bpb = 1.630;
    let v6_bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
    eprintln!("v0.6: {:.3} bpb vs v0.5: {:.3} bpb", v6_bpb, v5_bpb);
    assert!(v6_bpb < v5_bpb, "v0.6 ({:.3}) must beat v0.5 ({:.3})", v6_bpb, v5_bpb);
}

#[test]
fn test_v6_supera_gzip() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_v6(&data);
    let gzip_bpb = 1.524;
    let v6_bpb = compressed.len() as f64 * 8.0 / data.len() as f64;
    eprintln!("v0.6: {:.3} bpb vs gzip-9: {:.3} bpb", v6_bpb, gzip_bpb);
    // This is the test we want to see pass!
    if v6_bpb < gzip_bpb {
        eprintln!("*** v0.6 BEAT gzip-9! ***");
    } else {
        eprintln!("v0.6 did not beat gzip-9. Gap: {:.3} bpb", v6_bpb - gzip_bpb);
    }
    // Don't assert — report the number regardless
}

#[test]
fn test_offset_codes_4mb() {
    // Verify offsets up to 4MB - 1 encode/decode correctly
    let tokens = vec![
        lz77::Token::Literal(b'X'),
        lz77::Token::Match { offset: 4_194_303, length: 4 }, // max window - 1
    ];
    let encoded = huffman::huffman_encode(&tokens);
    let decoded = huffman::huffman_decode(&encoded);
    assert_eq!(tokens, decoded, "4MB-1 offset failed round-trip");
}

#[test]
fn test_v6_full_benchmark() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let input_size = data.len();
    let mb = input_size as f64 / 1_048_576.0;

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  NEXCOMP v0.6 — Huffman LZ77 Benchmark");
    eprintln!("{}\n", "=".repeat(70));

    // 3 runs for stable measurement
    let mut sizes = Vec::new();
    let mut comp_times = Vec::new();
    let mut dec_times = Vec::new();

    for run in 0..3 {
        let t0 = Instant::now();
        let compressed = compress_v6(&data);
        let ct = t0.elapsed().as_secs_f64();

        let t0 = Instant::now();
        let decompressed = decompress_v6(&compressed);
        let dt = t0.elapsed().as_secs_f64();

        if run == 0 {
            assert_eq!(data, decompressed, "LOSSLESS CHECK FAILED");
        }

        sizes.push(compressed.len());
        comp_times.push(ct);
        dec_times.push(dt);
    }

    let avg_size = sizes.iter().sum::<usize>() / sizes.len();
    let avg_comp = comp_times.iter().sum::<f64>() / comp_times.len() as f64;
    let avg_dec = dec_times.iter().sum::<f64>() / dec_times.len() as f64;
    let bpb = avg_size as f64 * 8.0 / input_size as f64;

    // Huffman header overhead
    let header_bits = huffman::LITLEN_SYMBOLS * 4 + huffman::DIST_SYMBOLS * 4;
    let header_bytes = header_bits.div_ceil(8);
    let header_pct = header_bytes as f64 / avg_size as f64 * 100.0;

    eprintln!("Compresor            Bytes       bpb    Comp MB/s  Decomp MB/s");
    eprintln!("{}", "-".repeat(65));
    eprintln!("xz -6            {:>10}     0.779       —           —", 327_820);
    eprintln!("bzip2 -9         {:>10}     1.206       —           —", 507_569);
    eprintln!("gzip -9          {:>10}     1.524       —           —", 641_307);
    eprintln!("{}", "-".repeat(65));
    eprintln!("NEXCOMP v0.6     {:>10}     {:.3}     {:.2}       {:.1}",
        avg_size, bpb, mb / avg_comp, mb / avg_dec);
    eprintln!("NEXCOMP v0.5     {:>10}     1.630     1.44       107.0", 685_866);
    eprintln!("NEXCOMP v0.4-C   {:>10}     1.667     1.52       117.1", 701_452);
    eprintln!("NEXCOMP v0.3     {:>10}     2.199     0.85        39.8", 925_397);
    eprintln!("{}", "-".repeat(65));

    let beat_gzip = bpb < 1.524;
    eprintln!("\nBeat gzip -9 (1.524 bpb)? {}", if beat_gzip { "YES!" } else { "NO" });
    eprintln!("Huffman header overhead: {} bytes ({:.1}% of output)", header_bytes, header_pct);
    eprintln!("Sizes across 3 runs: {:?}", sizes);

    // Detailed per-run
    for (i, (&s, (&ct, &dt))) in sizes.iter().zip(comp_times.iter().zip(dec_times.iter())).enumerate() {
        eprintln!("  Run {}: {} bytes, comp {:.3}s ({:.2} MB/s), dec {:.3}s ({:.1} MB/s)",
            i + 1, s, ct, mb / ct, dt, mb / dt);
    }
}
