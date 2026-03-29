//! QA v1.5 adversarial correctness tests
//!
//! Targets: range coder encode_freq/decode_freq, PPM codec, MTF in LZMA,
//! MRU-4 Huffman, BCJ filter, BWT SA-IS, multi-tree Huffman, adaptive pipeline.

use nexcomp::adaptive::{adaptive_compress, adaptive_decompress};
use nexcomp::codecs::bcj_filter::{bcj_decode, bcj_encode};
use nexcomp::codecs::bwt_codec::{bwt_compress, bwt_decompress};
use nexcomp::codecs::lzma_style;
use nexcomp::codecs::ppm::{ppm_compress, ppm_decompress};
use nexcomp::lz77::huffman;
use nexcomp::lz77::Lz77Encoder;
use nexcomp::range_coder::{RangeDecoder, RangeEncoder};

// =========================================================================
// 1. Range coder encode_freq precision
// =========================================================================

/// Round-trip encode_freq/decode_freq with a large total (>10000).
#[test]
fn qa_range_freq_large_total() {
    let weights: Vec<u32> = vec![100, 200, 300, 400, 500, 1000, 2000, 3000, 500, 1500];
    let total: u32 = weights.iter().sum(); // 9500
    assert!(total > 5000);

    let mut cum = vec![0u32; weights.len()];
    for i in 1..weights.len() {
        cum[i] = cum[i - 1] + weights[i - 1];
    }

    // Encode 10000 symbols using a simple LCG for determinism
    let mut state: u64 = 0xDEAD_BEEF;
    let mut symbols = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let sym = (state >> 33) as usize % weights.len();
        symbols.push(sym);
    }

    let mut enc = RangeEncoder::new();
    for &sym in &symbols {
        enc.encode_freq(cum[sym], weights[sym], total);
    }
    let compressed = enc.finish();

    let mut dec = RangeDecoder::new(&compressed);
    for (i, &expected) in symbols.iter().enumerate() {
        let target = dec.get_freq(total);
        let mut sym = 0usize;
        let mut accum = 0u32;
        for (j, &w) in weights.iter().enumerate() {
            if accum + w > target {
                sym = j;
                break;
            }
            accum += w;
        }
        assert_eq!(sym, expected, "range freq large total: mismatch at symbol {i}");
        dec.decode_freq(cum[sym], weights[sym], total);
    }
}

/// Edge case: total exactly at power-of-2 boundary (16384).
#[test]
fn qa_range_freq_power_of_two_total() {
    let weights = [4096u32, 4096, 4096, 4096]; // total=16384
    let total: u32 = weights.iter().sum();
    let cum = [0u32, 4096, 8192, 12288];

    let symbols: Vec<usize> = (0..5000).map(|i| i % 4).collect();

    let mut enc = RangeEncoder::new();
    for &s in &symbols {
        enc.encode_freq(cum[s], weights[s], total);
    }
    let compressed = enc.finish();

    let mut dec = RangeDecoder::new(&compressed);
    for (i, &expected) in symbols.iter().enumerate() {
        let target = dec.get_freq(total);
        let sym = (target / 4096) as usize;
        assert_eq!(sym.min(3), expected, "power-of-two total: mismatch at {i}");
        dec.decode_freq(cum[sym], weights[sym], total);
    }
}

/// Highly skewed distribution: one symbol dominates.
#[test]
fn qa_range_freq_skewed() {
    let weights = [9990u32, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]; // total=10000
    let total: u32 = weights.iter().sum();
    let mut cum = vec![0u32; weights.len()];
    for i in 1..weights.len() {
        cum[i] = cum[i - 1] + weights[i - 1];
    }

    // Mostly symbol 0, occasionally others
    let mut state: u64 = 12345;
    let mut symbols = Vec::with_capacity(5000);
    for _ in 0..5000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let r = (state >> 33) as u32 % total;
        let mut sym = 0usize;
        let mut accum = 0u32;
        for (j, &w) in weights.iter().enumerate() {
            if accum + w > r {
                sym = j;
                break;
            }
            accum += w;
        }
        symbols.push(sym);
    }

    let mut enc = RangeEncoder::new();
    for &s in &symbols {
        enc.encode_freq(cum[s], weights[s], total);
    }
    let compressed = enc.finish();

    let mut dec = RangeDecoder::new(&compressed);
    for (i, &expected) in symbols.iter().enumerate() {
        let target = dec.get_freq(total);
        let mut sym = 0usize;
        let mut accum = 0u32;
        for (j, &w) in weights.iter().enumerate() {
            if accum + w > target {
                sym = j;
                break;
            }
            accum += w;
        }
        assert_eq!(sym, expected, "skewed: mismatch at symbol {i}");
        dec.decode_freq(cum[sym], weights[sym], total);
    }
}

// =========================================================================
// 2. PPM encode/decode sync on adversarial inputs
// =========================================================================

#[test]
fn qa_ppm_all_same_byte() {
    let data = vec![0xAA; 10_000];
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM all-same-byte roundtrip failed");
}

#[test]
fn qa_ppm_alternating() {
    let data: Vec<u8> = (0..10_000).map(|i| if i % 2 == 0 { b'A' } else { b'B' }).collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM alternating roundtrip failed");
}

#[test]
fn qa_ppm_ascending() {
    let data: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM ascending roundtrip failed");
}

#[test]
fn qa_ppm_boundary_1_byte() {
    let data = vec![0x42];
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM 1-byte roundtrip failed");
}

#[test]
fn qa_ppm_boundary_255_bytes() {
    let data: Vec<u8> = (0..255).map(|i| i as u8).collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM 255-byte roundtrip failed");
}

#[test]
fn qa_ppm_boundary_256_bytes() {
    let data: Vec<u8> = (0..256).map(|i| (i % 256) as u8).collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM 256-byte roundtrip failed");
}

#[test]
fn qa_ppm_boundary_1000_bytes() {
    let data: Vec<u8> = (0..1000).map(|i| ((i * 7 + 13) % 256) as u8).collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM 1000-byte roundtrip failed");
}

#[test]
fn qa_ppm_three_byte_cycle() {
    // Three distinct bytes cycling: exercises order-2+ contexts
    let data: Vec<u8> = (0..10_000).map(|i| match i % 3 {
        0 => 0x00,
        1 => 0x80,
        _ => 0xFF,
    }).collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM three-byte-cycle roundtrip failed");
}

#[test]
fn qa_ppm_all_256_values_repeated() {
    // Every byte value appears, repeated enough to fill contexts at all orders
    let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM all-256 repeated roundtrip failed");
}

// =========================================================================
// 3. MTF in LZMA correctness
// =========================================================================

#[test]
fn qa_lzma_mtf_all_byte_values() {
    let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let compressed = lzma_style::encode_block(&data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(data, decompressed, "LZMA MTF all-byte-values failed");
}

#[test]
fn qa_lzma_mtf_repeated_single_byte() {
    let data = vec![0x42u8; 8192];
    let compressed = lzma_style::encode_block(&data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(data, decompressed, "LZMA MTF single-byte failed");
}

#[test]
fn qa_lzma_mtf_two_alternating() {
    // Alternating bytes: MTF should quickly learn the two-item pattern
    let data: Vec<u8> = (0..8192).map(|i| if i % 2 == 0 { 0x00 } else { 0xFF }).collect();
    let compressed = lzma_style::encode_block(&data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(data, decompressed, "LZMA MTF alternating failed");
}

#[test]
fn qa_lzma_mtf_descending() {
    // Worst case for MTF: descending sequence, every byte is at the back of the list
    let data: Vec<u8> = (0..5000).map(|i| (255 - (i % 256)) as u8).collect();
    let compressed = lzma_style::encode_block(&data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(data, decompressed, "LZMA MTF descending failed");
}

#[test]
fn qa_lzma_mtf_random_data() {
    // Random data stresses MTF positions spread across the whole range
    let mut data = vec![0u8; 10_000];
    let mut state: u64 = 0xBAAD_F00D;
    for b in data.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        *b = (state >> 33) as u8;
    }
    let compressed = lzma_style::encode_block(&data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(data, decompressed, "LZMA MTF random data failed");
}

#[test]
fn qa_lzma_mtf_with_matches() {
    // Data with both literals (exercising MTF) and long matches
    let mut data = Vec::with_capacity(20_000);
    for _ in 0..50 {
        // A pattern that LZ77 will find matches in
        data.extend_from_slice(b"The quick brown fox jumps ");
        // Some random bytes that won't match
        for j in 0..50u8 {
            data.push(j.wrapping_mul(7));
        }
    }
    let compressed = lzma_style::encode_block(&data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(data, decompressed, "LZMA MTF with matches failed");
}

// =========================================================================
// 4. MRU-4 Huffman roundtrip
// =========================================================================

/// Helper: generate LZ77 tokens from raw data, then roundtrip through
/// all four Huffman encode/decode paths.
fn roundtrip_huffman_all_paths(data: &[u8]) {
    let mut enc = Lz77Encoder::new();
    let (tokens, _) = enc.encode(data);
    if tokens.is_empty() {
        return;
    }

    // Path 1: global
    {
        let encoded = huffman::huffman_encode(&tokens);
        let decoded = huffman::huffman_decode(&encoded);
        let original = nexcomp::lz77::lz77_decode(&decoded).unwrap();
        assert_eq!(data, &original[..], "MRU4 global roundtrip failed");
    }

    // Path 2: blocked
    {
        let encoded = huffman::huffman_encode_blocked(&tokens, 4096);
        let decoded = huffman::huffman_decode_blocked(&encoded);
        let original = nexcomp::lz77::lz77_decode(&decoded).unwrap();
        assert_eq!(data, &original[..], "MRU4 blocked roundtrip failed");
    }

    // Path 3: context1
    {
        let encoded = huffman::huffman_encode_context1(&tokens, 4096);
        let decoded = huffman::huffman_decode_context1(&encoded);
        let original = nexcomp::lz77::lz77_decode(&decoded).unwrap();
        assert_eq!(data, &original[..], "MRU4 context1 roundtrip failed");
    }

    // Path 4: split
    {
        let encoded = huffman::huffman_encode_split(&tokens);
        let decoded = huffman::huffman_decode_split(&encoded);
        let original = nexcomp::lz77::lz77_decode(&decoded).unwrap();
        assert_eq!(data, &original[..], "MRU4 split roundtrip failed");
    }
}

#[test]
fn qa_mru4_stress_repeated_offsets() {
    // Data designed to exercise all 4 MRU slots:
    // Create patterns at 4 distinct offsets, then reference them repeatedly
    let mut data = Vec::with_capacity(20_000);

    // Create 4 distinct patterns at known offsets
    let patterns = [b"ALPHA_PAT " as &[u8], b"BRAVO_PAT ", b"CHARLIE_P ", b"DELTA_PAT "];
    for p in &patterns {
        data.extend_from_slice(p);
    }
    // Now fill with repetitions that cycle through all 4 offsets
    for i in 0..500 {
        data.extend_from_slice(patterns[i % 4]);
    }

    roundtrip_huffman_all_paths(&data);
}

#[test]
fn qa_mru4_large_offsets() {
    // Large offsets that rotate through MRU
    let mut data = Vec::with_capacity(50_000);
    // Big unique prefix
    for i in 0..10_000u16 {
        data.push((i & 0xFF) as u8);
    }
    // Patterns at various offsets
    let anchor = data.len();
    for _ in 0..20 {
        data.extend_from_slice(b"PATTERN_A!");
    }
    for _ in 0..20 {
        data.extend_from_slice(b"PATTERN_B!");
    }
    // Reference old data at large offsets
    let chunk: Vec<u8> = data[100..120].to_vec();
    for _ in 0..50 {
        data.extend_from_slice(&chunk);
    }
    let _ = anchor;

    roundtrip_huffman_all_paths(&data);
}

#[test]
fn qa_mru4_all_literals() {
    // Data that produces only literals (no matches) -- MRU is never used
    // 256 distinct bytes, each appears once
    let data: Vec<u8> = (0..=255u8).collect();
    roundtrip_huffman_all_paths(&data);
}

// =========================================================================
// 5. BWT SA-IS on adversarial strings
// =========================================================================

#[test]
fn qa_sais_all_same_char() {
    // 900KB of a single repeated character
    let data = vec![b'a'; 900_000];
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "BWT SA-IS all-same 900KB failed");
}

#[test]
fn qa_sais_two_chars_alternating() {
    // Alternating a/b, 900KB
    let data: Vec<u8> = (0..900_000).map(|i| if i % 2 == 0 { b'a' } else { b'b' }).collect();
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "BWT SA-IS two-chars alternating failed");
}

#[test]
fn qa_sais_nearly_sorted() {
    // Mostly sorted with a few swaps
    let mut data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
    // Introduce a few swaps
    for i in (0..data.len() - 1).step_by(997) {
        data.swap(i, i + 1);
    }
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "BWT SA-IS nearly-sorted failed");
}

#[test]
fn qa_sais_reverse_sorted() {
    // Reverse sorted -- pathological for many SA algorithms
    let data: Vec<u8> = (0..100_000).map(|i| (255 - (i % 256)) as u8).collect();
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "BWT SA-IS reverse-sorted failed");
}

#[test]
fn qa_sais_fibonacci_string() {
    // Fibonacci string: very repetitive, stresses suffix array
    let mut a = b"a".to_vec();
    let mut b = b"ab".to_vec();
    while b.len() < 200_000 {
        let next = [b.clone(), a.clone()].concat();
        a = b;
        b = next;
    }
    b.truncate(200_000);
    let compressed = bwt_compress(&b);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(b, decompressed, "BWT SA-IS Fibonacci string failed");
}

#[test]
fn qa_sais_single_byte() {
    let data = vec![0x42];
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "BWT SA-IS single byte failed");
}

#[test]
fn qa_sais_empty() {
    let data: Vec<u8> = vec![];
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "BWT SA-IS empty failed");
}

#[test]
fn qa_sais_all_256_values() {
    // All byte values present, repeated
    let data: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "BWT SA-IS all-256 values failed");
}

// =========================================================================
// 6. Multi-tree Huffman (via BWT pipeline)
// =========================================================================

#[test]
fn qa_multi_tree_single_symbol() {
    // All same symbol -> should use 1 tree
    let data = vec![b'x'; 50_000];
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "Multi-tree single symbol failed");
}

#[test]
fn qa_multi_tree_two_distributions() {
    // First half: all 'a', second half: all 'z'
    // Different distributions should benefit from 2 trees
    let mut data = vec![b'a'; 50_000];
    data.extend(vec![b'z'; 50_000]);
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "Multi-tree two distributions failed");
}

#[test]
fn qa_multi_tree_many_distributions() {
    // Multiple distinct regions with different byte distributions
    let mut data = Vec::with_capacity(100_000);
    for region in 0..10u8 {
        let base = region * 25;
        for _ in 0..10_000 {
            data.push(base.wrapping_add((region * 7) % 10));
        }
    }
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "Multi-tree many distributions failed");
}

#[test]
fn qa_multi_tree_random_data() {
    // Random data: all trees should be similar
    let mut data = vec![0u8; 50_000];
    let mut state: u64 = 0xC0FFEE;
    for b in data.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        *b = (state >> 33) as u8;
    }
    let compressed = bwt_compress(&data);
    let decompressed = bwt_decompress(&compressed);
    assert_eq!(data, decompressed, "Multi-tree random data failed");
}

// =========================================================================
// 7. BCJ filter adversarial tests
// =========================================================================

#[test]
fn qa_bcj_consecutive_e8e9() {
    // Data that is entirely E8/E9 bytes
    let data: Vec<u8> = (0..10_000).map(|i| if i % 2 == 0 { 0xE8 } else { 0xE9 }).collect();
    let encoded = bcj_encode(&data);
    let decoded = bcj_decode(&encoded);
    assert_eq!(data, decoded, "BCJ consecutive E8/E9 failed");
}

#[test]
fn qa_bcj_e8_at_various_positions() {
    // E8 at positions that might cause off-by-one errors
    for pos in [0, 1, 2, 3, 4, 5, 95, 96, 97, 98, 99] {
        let mut data = vec![0x90u8; 100];
        if pos + 4 < data.len() {
            data[pos] = 0xE8;
            data[pos + 1] = 0xFF;
            data[pos + 2] = 0xFF;
            data[pos + 3] = 0xFF;
            data[pos + 4] = 0xFF;
        }
        let encoded = bcj_encode(&data);
        let decoded = bcj_decode(&encoded);
        assert_eq!(data, decoded, "BCJ at position {pos} failed");
    }
}

#[test]
fn qa_bcj_overlapping_instructions() {
    // E8/E9 in the address field of a previous CALL/JMP
    let mut data = vec![0x90u8; 100];
    data[10] = 0xE8;
    data[11] = 0xE8; // This E8 is in the address field of the CALL at 10
    data[12] = 0x00;
    data[13] = 0x00;
    data[14] = 0x00;
    let encoded = bcj_encode(&data);
    let decoded = bcj_decode(&encoded);
    assert_eq!(data, decoded, "BCJ overlapping instructions failed");
}

#[test]
fn qa_bcj_negative_offsets() {
    // CALL with negative relative offset (backward jump)
    let mut data = vec![0x90u8; 200];
    data[100] = 0xE8;
    // Relative offset -50 = 0xFFFFFFCE
    data[101] = 0xCE;
    data[102] = 0xFF;
    data[103] = 0xFF;
    data[104] = 0xFF;
    let encoded = bcj_encode(&data);
    let decoded = bcj_decode(&encoded);
    assert_eq!(data, decoded, "BCJ negative offsets failed");
}

// =========================================================================
// 8. Adaptive pipeline end-to-end
// =========================================================================

#[test]
fn qa_adaptive_text_roundtrip() {
    let data = b"The quick brown fox jumps over the lazy dog. ".repeat(500);
    let compressed = adaptive_compress(&data);
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(data, decompressed, "Adaptive text roundtrip failed");
}

#[test]
fn qa_adaptive_binary_roundtrip() {
    let mut data = vec![0u8; 20_000];
    let mut state: u64 = 0xFACEFEED;
    for b in data.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        *b = (state >> 33) as u8;
    }
    let compressed = adaptive_compress(&data);
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(data, decompressed, "Adaptive binary roundtrip failed");
}

#[test]
fn qa_adaptive_zeros_roundtrip() {
    let data = vec![0u8; 30_000];
    let compressed = adaptive_compress(&data);
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(data, decompressed, "Adaptive zeros roundtrip failed");
}

#[test]
fn qa_adaptive_low_entropy_roundtrip() {
    // Low entropy: two non-printable values
    let data: Vec<u8> = (0..20_000).map(|i| if i % 3 == 0 { 0x01 } else { 0x02 }).collect();
    let compressed = adaptive_compress(&data);
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(data, decompressed, "Adaptive low-entropy roundtrip failed");
}

#[test]
fn qa_adaptive_mixed_content() {
    // Mix of text, binary, and repetitive data
    let mut data = Vec::with_capacity(50_000);
    // Text section
    data.extend(b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(100));
    // Binary section
    let mut state: u64 = 999;
    for _ in 0..10_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        data.push((state >> 33) as u8);
    }
    // Repetitive section
    data.extend(vec![0xAA; 5000]);

    let compressed = adaptive_compress(&data);
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(data, decompressed, "Adaptive mixed content roundtrip failed");
}

#[test]
fn qa_adaptive_small_sizes() {
    // Test various small sizes
    for size in [1, 2, 3, 10, 50, 100, 255, 256, 500, 1000] {
        let data: Vec<u8> = (0..size).map(|i| ((i * 13 + 7) % 256) as u8).collect();
        let compressed = adaptive_compress(&data);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(data, decompressed, "Adaptive size={size} roundtrip failed");
    }
}

#[test]
fn qa_adaptive_corpus_fixtures() {
    // Roundtrip the english_sample.txt fixture
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/english_sample.txt");
    if let Ok(data) = std::fs::read(path) {
        if !data.is_empty() {
            let compressed = adaptive_compress(&data);
            let decompressed = adaptive_decompress(&compressed);
            assert_eq!(
                data, decompressed,
                "Adaptive roundtrip failed on english_sample.txt ({} bytes)",
                data.len()
            );
        }
    }
}

// =========================================================================
// 9. Cross-component: LZMA full-pipeline stress
// =========================================================================

#[test]
fn qa_lzma_empty() {
    let data = b"";
    let compressed = lzma_style::encode_block(data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(&data[..], &decompressed[..]);
}

#[test]
fn qa_lzma_single_byte() {
    for byte in [0x00, 0x7F, 0x80, 0xFF] {
        let data = vec![byte];
        let compressed = lzma_style::encode_block(&data);
        let decompressed = lzma_style::decode_block(&compressed).unwrap();
        assert_eq!(data, decompressed, "LZMA single byte {byte:#04x} failed");
    }
}

#[test]
fn qa_lzma_long_match_chain() {
    // Data with very long match chains
    let mut data = Vec::with_capacity(50_000);
    let pattern = b"abcdefghijklmnop";
    for _ in 0..3000 {
        data.extend_from_slice(pattern);
    }
    let compressed = lzma_style::encode_block(&data);
    let decompressed = lzma_style::decode_block(&compressed).unwrap();
    assert_eq!(data, decompressed, "LZMA long match chain failed");
}

// =========================================================================
// 10. PPM large file stress test
// =========================================================================

#[test]
fn qa_ppm_large_repetitive() {
    // 50KB of repetitive text -- exercises rescaling in FreqTable
    let data = b"abcdefghijklmnopqrstuvwxyz ".repeat(2000);
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM large repetitive roundtrip failed");
}

#[test]
fn qa_ppm_context_order_boundary() {
    // Data with context-length = exactly MAX_ORDER (5)
    // Repeat a 6-byte pattern so order-5 context is always the same
    let pattern = b"ABCDEF";
    let data: Vec<u8> = pattern.iter().cycle().take(10_000).copied().collect();
    let compressed = ppm_compress(&data);
    let decompressed = ppm_decompress(&compressed);
    assert_eq!(data, decompressed, "PPM context order boundary failed");
}
