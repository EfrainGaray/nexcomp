//! QA Audit Tests for NEXCOMP lossless compressor.
//!
//! Covers adversarial inputs, edge cases, wire format validation,
//! and codec-level roundtrip correctness.

use nexcomp::adaptive::{
    adaptive_compress, adaptive_decompress, decompress_block_adaptive,
    CodecId,
};
use nexcomp::codecs::bcj_filter;
use nexcomp::codecs::bwt_codec;
use nexcomp::codecs::delta_ans;
use nexcomp::codecs::lzma_style;
use nexcomp::codecs::ppm;
use nexcomp::codecs::rle_huffman;
use nexcomp::lzma_state::LzmaState;
use nexcomp::range_coder::{Prob, RangeDecoder, RangeEncoder, PROB_BITS, PROB_INIT};

// ============================================================================
// Helper: simple deterministic PRNG (no external crate dependency)
// ============================================================================

fn lcg_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

// ============================================================================
// 1. LOSSLESS CORRECTNESS — Individual codec roundtrips
// ============================================================================

/// Helper: verify compress -> decompress == original for adaptive pipeline
fn assert_adaptive_roundtrip(data: &[u8], label: &str) {
    let compressed = adaptive_compress(data);
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(
        data, &decompressed[..],
        "Adaptive roundtrip failed for: {}",
        label
    );
}

/// Helper: verify LZMA codec roundtrip
fn assert_lzma_roundtrip(data: &[u8], label: &str) {
    let encoded = lzma_style::encode_block(data);
    let decoded = lzma_style::decode_block(&encoded).expect(&format!(
        "LZMA decode failed for: {}",
        label
    ));
    assert_eq!(data, &decoded[..], "LZMA roundtrip failed for: {}", label);
}

/// Helper: verify BWT codec roundtrip
fn assert_bwt_roundtrip(data: &[u8], label: &str) {
    let compressed = bwt_codec::bwt_compress(data);
    let decompressed = bwt_codec::bwt_decompress(&compressed);
    assert_eq!(data, &decompressed[..], "BWT roundtrip failed for: {}", label);
}

/// Helper: verify DeltaANS codec roundtrip
fn assert_delta_ans_roundtrip(data: &[u8], label: &str) {
    let encoded = delta_ans::delta_ans_encode(data).expect(&format!(
        "DeltaANS encode failed for: {}",
        label
    ));
    let decoded = delta_ans::delta_ans_decode(&encoded).expect(&format!(
        "DeltaANS decode failed for: {}",
        label
    ));
    assert_eq!(
        data, &decoded[..],
        "DeltaANS roundtrip failed for: {}",
        label
    );
}

/// Helper: verify RleHuffman codec roundtrip
fn assert_rle_huffman_roundtrip(data: &[u8], label: &str) {
    let encoded = rle_huffman::rle_huffman_encode(data);
    let decoded = rle_huffman::rle_huffman_decode(&encoded);
    assert_eq!(
        data, &decoded[..],
        "RleHuffman roundtrip failed for: {}",
        label
    );
}

// ============================================================================
// QA: Empty data (0 bytes)
// ============================================================================

#[test]
fn qa_lossless_empty() {
    let data: &[u8] = &[];

    // Adaptive
    assert_adaptive_roundtrip(data, "empty adaptive");

    // LZMA
    assert_lzma_roundtrip(data, "empty lzma");

    // BWT
    assert_bwt_roundtrip(data, "empty bwt");

    // DeltaANS
    assert_delta_ans_roundtrip(data, "empty delta_ans");

    // RleHuffman
    assert_rle_huffman_roundtrip(data, "empty rle_huffman");

    // PPM
    let ppm_enc = ppm::ppm_compress(data);
    let ppm_dec = ppm::ppm_decompress(&ppm_enc);
    assert_eq!(data, &ppm_dec[..], "PPM roundtrip failed for empty");
}

// ============================================================================
// QA: Single byte
// ============================================================================

#[test]
fn qa_lossless_single_byte() {
    for b in [0u8, 1, 127, 128, 255] {
        let data = vec![b];
        let label = format!("single byte 0x{:02X}", b);

        assert_adaptive_roundtrip(&data, &label);
        assert_lzma_roundtrip(&data, &label);
        assert_bwt_roundtrip(&data, &label);
        assert_delta_ans_roundtrip(&data, &label);
        assert_rle_huffman_roundtrip(&data, &label);

        let ppm_enc = ppm::ppm_compress(&data);
        let ppm_dec = ppm::ppm_decompress(&ppm_enc);
        assert_eq!(data, ppm_dec, "PPM roundtrip failed for {}", label);
    }
}

// ============================================================================
// QA: All-same bytes (large)
// ============================================================================

#[test]
fn qa_lossless_all_same_1mb() {
    // 1MB of 0x00
    let data_zeros = vec![0u8; 1024 * 1024];
    assert_adaptive_roundtrip(&data_zeros, "1MB zeros");
    assert_lzma_roundtrip(&data_zeros, "1MB zeros lzma");
    assert_rle_huffman_roundtrip(&data_zeros, "1MB zeros rle_huffman");
    // BWT on 1MB is large but we test it at block-size boundary later
    // DeltaANS
    assert_delta_ans_roundtrip(&data_zeros, "1MB zeros delta_ans");

    // 1MB of 0xFF
    let data_ff = vec![0xFFu8; 1024 * 1024];
    assert_adaptive_roundtrip(&data_ff, "1MB 0xFF");
    assert_lzma_roundtrip(&data_ff, "1MB 0xFF lzma");
    assert_rle_huffman_roundtrip(&data_ff, "1MB 0xFF rle_huffman");
    assert_delta_ans_roundtrip(&data_ff, "1MB 0xFF delta_ans");
}

// ============================================================================
// QA: All unique bytes (0-255 repeated)
// ============================================================================

#[test]
fn qa_lossless_all_unique_bytes() {
    let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();

    assert_adaptive_roundtrip(&data, "all unique bytes");
    assert_lzma_roundtrip(&data, "all unique bytes lzma");
    assert_bwt_roundtrip(&data, "all unique bytes bwt");
    assert_delta_ans_roundtrip(&data, "all unique bytes delta_ans");
    assert_rle_huffman_roundtrip(&data, "all unique bytes rle_huffman");
}

// ============================================================================
// QA: Random data (high entropy)
// ============================================================================

#[test]
fn qa_lossless_random_high_entropy() {
    let data = lcg_bytes(0xDEADBEEF, 65536);

    assert_adaptive_roundtrip(&data, "random 64KB");
    assert_lzma_roundtrip(&data, "random 64KB lzma");
    assert_bwt_roundtrip(&data, "random 64KB bwt");
    assert_delta_ans_roundtrip(&data, "random 64KB delta_ans");
    assert_rle_huffman_roundtrip(&data, "random 64KB rle_huffman");

    // Adaptive should not expand too much
    let compressed = adaptive_compress(&data);
    let overhead = compressed.len() as f64 / data.len() as f64;
    assert!(
        overhead < 1.15,
        "Random data overhead too high: {:.3}",
        overhead
    );
}

// ============================================================================
// QA: Near-boundary sizes
// ============================================================================

#[test]
fn qa_lossless_boundary_sizes() {
    let boundaries = [
        4095, 4096, 4097, 8191, 8192, 8193, 65535, 65536, 65537,
    ];

    for &size in &boundaries {
        let data = lcg_bytes(size as u64, size);
        let label = format!("boundary size {}", size);
        assert_adaptive_roundtrip(&data, &label);
        assert_lzma_roundtrip(&data, &label);
        assert_delta_ans_roundtrip(&data, &label);
        assert_rle_huffman_roundtrip(&data, &label);
    }
}

// ============================================================================
// QA: BWT block boundary sizes (900KB)
// ============================================================================

#[test]
fn qa_bwt_block_boundary() {
    let bwt_block = bwt_codec::BWT_BLOCK_SIZE; // 900 * 1024

    for &size in &[bwt_block - 1, bwt_block, bwt_block + 1] {
        // Use repetitive text-like data (BWT is for text)
        let pattern = b"The quick brown fox jumps over the lazy dog. ";
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            data.extend_from_slice(pattern);
        }
        data.truncate(size);

        let label = format!("bwt boundary size {}", size);
        assert_bwt_roundtrip(&data, &label);
    }
}

// ============================================================================
// QA: Pathological inputs — all 0xE8 (triggers BCJ filter)
// ============================================================================

#[test]
fn qa_lossless_pathological_e8() {
    // All E8 bytes — a worst case for BCJ filter
    let data = vec![0xE8u8; 10000];
    assert_adaptive_roundtrip(&data, "all 0xE8 10KB");
    assert_lzma_roundtrip(&data, "all 0xE8 lzma");
    assert_rle_huffman_roundtrip(&data, "all 0xE8 rle_huffman");
    assert_delta_ans_roundtrip(&data, "all 0xE8 delta_ans");

    // BCJ filter roundtrip on all-E8
    let encoded = bcj_filter::bcj_encode(&data);
    let decoded = bcj_filter::bcj_decode(&encoded);
    assert_eq!(data, decoded, "BCJ roundtrip failed on all-0xE8");
}

// ============================================================================
// QA: Pathological inputs — alternating 0x00/0xFF
// ============================================================================

#[test]
fn qa_lossless_alternating() {
    let data: Vec<u8> = (0..50000).map(|i| if i % 2 == 0 { 0x00 } else { 0xFF }).collect();
    assert_adaptive_roundtrip(&data, "alternating 00/FF");
    assert_lzma_roundtrip(&data, "alternating 00/FF lzma");
    assert_bwt_roundtrip(&data, "alternating 00/FF bwt");
    assert_delta_ans_roundtrip(&data, "alternating 00/FF delta_ans");
    assert_rle_huffman_roundtrip(&data, "alternating 00/FF rle_huffman");
}

// ============================================================================
// QA: Pathological — long runs followed by random
// ============================================================================

#[test]
fn qa_lossless_runs_then_random() {
    let mut data = vec![0xAAu8; 50000];
    data.extend(lcg_bytes(42, 50000));
    assert_adaptive_roundtrip(&data, "runs then random");
    assert_lzma_roundtrip(&data, "runs then random lzma");
    assert_delta_ans_roundtrip(&data, "runs then random delta_ans");
    assert_rle_huffman_roundtrip(&data, "runs then random rle_huffman");
}

// ============================================================================
// 2. BCJ FILTER CORRECTNESS
// ============================================================================

#[test]
fn qa_bcj_roundtrip_adversarial() {
    // Empty
    assert_eq!(bcj_filter::bcj_decode(&bcj_filter::bcj_encode(&[])), &[] as &[u8]);

    // 1 byte
    assert_eq!(bcj_filter::bcj_decode(&bcj_filter::bcj_encode(&[0xE8])), vec![0xE8]);

    // 4 bytes (just under the threshold for transformation)
    let short = vec![0xE8, 0x01, 0x02, 0x03];
    assert_eq!(bcj_filter::bcj_decode(&bcj_filter::bcj_encode(&short)), short);

    // 5 bytes — exactly the boundary
    let exact5 = vec![0xE8, 0x10, 0x00, 0x00, 0x00];
    assert_eq!(bcj_filter::bcj_decode(&bcj_filter::bcj_encode(&exact5)), exact5);

    // E8/E9 in last 4 bytes — should not be transformed
    let mut trailing = vec![0x90u8; 100];
    trailing[96] = 0xE8;
    trailing[97] = 0xFF;
    trailing[98] = 0xFF;
    trailing[99] = 0xFF;
    assert_eq!(
        bcj_filter::bcj_decode(&bcj_filter::bcj_encode(&trailing)),
        trailing,
        "BCJ failed with E8 in last 4 bytes"
    );

    // E9 at position (len - 5) — last valid position
    let mut last_valid = vec![0x90u8; 20];
    last_valid[15] = 0xE9;
    last_valid[16] = 0x01;
    last_valid[17] = 0x02;
    last_valid[18] = 0x03;
    last_valid[19] = 0x04;
    assert_eq!(
        bcj_filter::bcj_decode(&bcj_filter::bcj_encode(&last_valid)),
        last_valid,
        "BCJ failed at last valid E9 position"
    );

    // Random data with many E8/E9 bytes
    let mut heavy_e8 = lcg_bytes(0x1234, 8192);
    for i in (0..heavy_e8.len()).step_by(7) {
        heavy_e8[i] = if i % 2 == 0 { 0xE8 } else { 0xE9 };
    }
    assert_eq!(
        bcj_filter::bcj_decode(&bcj_filter::bcj_encode(&heavy_e8)),
        heavy_e8,
        "BCJ failed on heavy E8/E9 data"
    );
}

// ============================================================================
// 3. BWT / SA-IS ADVERSARIAL TESTS
// ============================================================================

#[test]
fn qa_bwt_sais_adversarial() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("all same 'a'", vec![b'a'; 1000]),
        ("two chars alternating", {
            let mut v = Vec::with_capacity(2000);
            for _ in 0..1000 { v.push(b'a'); v.push(b'b'); }
            v
        }),
        ("nearly sorted", b"abcdefghijklmnopqrstuvwxyz".to_vec()),
        ("reverse sorted", b"zyxwvutsrqponmlkjihgfedcba".to_vec()),
        ("long repeated pattern", b"abcabc".repeat(10000)),
        ("single char different", {
            let mut v = vec![b'a'; 1000];
            v.push(b'b');
            v
        }),
        ("binary all same 0x00", vec![0x00; 500]),
        ("binary all same 0xFF", vec![0xFF; 500]),
        ("two-byte pattern", vec![0x00, 0xFF].repeat(5000)),
    ];

    for (label, data) in &cases {
        assert_bwt_roundtrip(data, label);
    }
}

// ============================================================================
// 4. RANGE CODER EXTREME PROBABILITIES
// ============================================================================

#[test]
fn qa_range_coder_extreme_probs() {
    // Near-zero probability (1/2048)
    {
        let mut enc = RangeEncoder::new();
        let mut prob: Prob = 1; // near zero probability for bit=0
        let bits: Vec<u32> = (0..5000).map(|i| if i % 100 == 0 { 0 } else { 1 }).collect();
        for &b in &bits {
            enc.encode_bit(&mut prob, b);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        let mut prob2: Prob = 1;
        for (i, &expected) in bits.iter().enumerate() {
            let got = dec.decode_bit(&mut prob2);
            assert_eq!(got, expected, "extreme low prob mismatch at bit {}", i);
        }
    }

    // Near-one probability (2047/2048)
    {
        let mut enc = RangeEncoder::new();
        let mut prob: Prob = (1 << PROB_BITS) - 1; // near 1.0
        let bits: Vec<u32> = (0..5000).map(|i| if i % 100 == 0 { 1 } else { 0 }).collect();
        for &b in &bits {
            enc.encode_bit(&mut prob, b);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        let mut prob2: Prob = (1 << PROB_BITS) - 1;
        for (i, &expected) in bits.iter().enumerate() {
            let got = dec.decode_bit(&mut prob2);
            assert_eq!(got, expected, "extreme high prob mismatch at bit {}", i);
        }
    }

    // Many consecutive identical bits (1000 zeros, then 1000 ones)
    {
        let mut bits = vec![0u32; 1000];
        bits.extend(vec![1u32; 1000]);

        let mut enc = RangeEncoder::new();
        let mut prob: Prob = PROB_INIT;
        for &b in &bits {
            enc.encode_bit(&mut prob, b);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        let mut prob2: Prob = PROB_INIT;
        for (i, &expected) in bits.iter().enumerate() {
            let got = dec.decode_bit(&mut prob2);
            assert_eq!(got, expected, "consecutive bits mismatch at {}", i);
        }
    }

    // Alternating bits
    {
        let bits: Vec<u32> = (0..2000).map(|i| (i % 2) as u32).collect();

        let mut enc = RangeEncoder::new();
        let mut prob: Prob = PROB_INIT;
        for &b in &bits {
            enc.encode_bit(&mut prob, b);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        let mut prob2: Prob = PROB_INIT;
        for (i, &expected) in bits.iter().enumerate() {
            let got = dec.decode_bit(&mut prob2);
            assert_eq!(got, expected, "alternating bits mismatch at {}", i);
        }
    }
}

// ============================================================================
// 5. LZMA STATE MACHINE TRANSITIONS
// ============================================================================

#[test]
fn qa_lzma_state_transitions() {
    // Verify LZMA spec transition tables (from LZMA SDK documentation)
    //
    // LITERAL_NEXT:    [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5]
    // MATCH_NEXT:      [7, 7, 7, 7, 7, 7, 7, 10, 10, 10, 10, 10]
    // REP_NEXT:        [8, 8, 8, 8, 8, 8, 8, 11, 11, 11, 11, 11]
    // SHORTREP_NEXT:   [9, 9, 9, 9, 9, 9, 9, 11, 11, 11, 11, 11]

    let expected_literal = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 4, 5];
    let expected_match = [7, 7, 7, 7, 7, 7, 7, 10, 10, 10, 10, 10];
    let expected_rep = [8, 8, 8, 8, 8, 8, 8, 11, 11, 11, 11, 11];
    let expected_shortrep = [9, 9, 9, 9, 9, 9, 9, 11, 11, 11, 11, 11];

    for start in 0..12 {
        // Literal transition
        let mut s = LzmaState { state: start };
        s.update_literal();
        assert_eq!(
            s.state, expected_literal[start],
            "literal transition from state {} incorrect",
            start
        );

        // Match transition
        let mut s = LzmaState { state: start };
        s.update_match();
        assert_eq!(
            s.state, expected_match[start],
            "match transition from state {} incorrect",
            start
        );
        assert!(s.state >= 7, "match should produce non-literal state");

        // Rep transition
        let mut s = LzmaState { state: start };
        s.update_rep();
        assert_eq!(
            s.state, expected_rep[start],
            "rep transition from state {} incorrect",
            start
        );

        // Shortrep transition
        let mut s = LzmaState { state: start };
        s.update_shortrep();
        assert_eq!(
            s.state, expected_shortrep[start],
            "shortrep transition from state {} incorrect",
            start
        );
    }

    // Random walk: state should always remain in [0, 11]
    let mut state = LzmaState::new();
    let mut rng_state: u64 = 0x12345678;
    for _ in 0..100_000 {
        rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        match (rng_state >> 33) % 4 {
            0 => state.update_literal(),
            1 => state.update_match(),
            2 => state.update_rep(),
            _ => state.update_shortrep(),
        }
        assert!(
            state.state < 12,
            "state {} out of range after random transitions",
            state.state
        );
    }
}

// ============================================================================
// 6. WIRE FORMAT — Truncated payload
// ============================================================================

#[test]
fn qa_wire_format_truncated() {
    let data = b"Hello, World! This is test data for wire format.";
    let compressed = adaptive_compress(data);

    // Verify the full roundtrip works
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(&data[..], &decompressed[..]);

    // Truncated to 0 bytes — should panic (assert in adaptive_decompress)
    let result = std::panic::catch_unwind(|| {
        adaptive_decompress(&[]);
    });
    assert!(result.is_err(), "Empty payload should panic");

    // Truncated to 5 bytes — too short for header
    let result = std::panic::catch_unwind(|| {
        adaptive_decompress(&compressed[..5]);
    });
    assert!(result.is_err(), "5-byte payload should panic");

    // Truncated to 9 bytes — missing bcj_flag
    let result = std::panic::catch_unwind(|| {
        adaptive_decompress(&compressed[..9]);
    });
    assert!(result.is_err(), "9-byte payload should panic");

    // Header only (10 bytes) — no compressed data
    // This may or may not panic depending on the codec
    // We just verify it doesn't silently return wrong data
    let header_only = &compressed[..10];
    let result = std::panic::catch_unwind(|| {
        let dec = adaptive_decompress(header_only);
        // If it doesn't panic, it must NOT equal original
        // (unless original was empty, which it isn't here)
        dec
    });
    // Either panics or returns something != original
    if let Ok(dec) = result {
        assert_ne!(&data[..], &dec[..], "Truncated payload should not decompress to original");
    }
}

// ============================================================================
// 7. WIRE FORMAT — Invalid codec ID
// ============================================================================

#[test]
fn qa_wire_format_invalid_codec() {
    // Build a valid wire-format packet and corrupt the codec_id byte
    let data = b"Test data for codec ID validation.";
    let mut compressed = adaptive_compress(data);

    // codec_id of the first block is at offset 16
    let original_codec_id = compressed[16];

    // Test with codec_id = 255 (invalid — should map to Passthrough via from_u8)
    compressed[16] = 255;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adaptive_decompress(&compressed)
    }));
    // CodecId::from_u8(255) maps to Passthrough, so it will return the raw
    // compressed payload as if it were the original data — this is a BUG
    // (it should error, not silently return corrupt data)
    if let Ok(dec) = &result {
        // The decompressed data will NOT match original (it's the compressed payload)
        assert_ne!(
            &data[..], &dec[..],
            "Invalid codec ID 255 should not produce correct output"
        );
    }

    // Restore and verify original still works
    compressed[16] = original_codec_id;
    let decompressed = adaptive_decompress(&compressed);
    assert_eq!(&data[..], &decompressed[..]);

    // Test: codec_id = 6 (Ppm) — from_u8(6) maps to Passthrough (BUG)
    compressed[16] = 6;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adaptive_decompress(&compressed)
    }));
    if let Ok(dec) = &result {
        // Should NOT match original since PPM data is being treated as Passthrough
        assert_ne!(
            &data[..], &dec[..],
            "Codec ID 6 (Ppm) mapped to Passthrough is a bug"
        );
    }
}

// ============================================================================
// 8. WIRE FORMAT — orig_len mismatch (header says different size)
// ============================================================================

#[test]
fn qa_wire_format_orig_len_ignored() {
    // BUG: adaptive_decompress reads _orig_len but never uses it.
    // This test verifies the bug exists.
    let data = b"Test data for orig_len verification.";
    let mut compressed = adaptive_compress(data);

    // Corrupt orig_len in header (bytes 4-7) to something wrong
    let wrong_len: u32 = 999999;
    compressed[4..8].copy_from_slice(&wrong_len.to_le_bytes());

    // Decompression should still "succeed" because orig_len is ignored
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adaptive_decompress(&compressed)
    }));

    // If it succeeds with wrong header length, that confirms the bug:
    // no integrity check on orig_len
    if let Ok(dec) = result {
        // It will produce the correct data because orig_len is ignored — BUG
        // A correct implementation would verify orig_len matches
        assert_eq!(
            &data[..], &dec[..],
            "BUG CONFIRMED: orig_len in header is ignored"
        );
    }
}

// ============================================================================
// 9. Individual codec roundtrips on various data patterns
// ============================================================================

#[test]
fn qa_codec_individual_roundtrips() {
    let test_data: Vec<(&str, Vec<u8>)> = vec![
        ("two bytes", vec![0x00, 0xFF]),
        ("three bytes", vec![0x01, 0x02, 0x03]),
        ("256 unique", (0..=255u8).collect()),
        ("run of 50000", vec![0x42; 50000]),
        ("sawtooth", (0..10000).map(|i| (i % 256) as u8).collect()),
        (
            "correlated",
            {
                let mut v = vec![0u8; 8192];
                let mut val: u8 = 128;
                for i in 0..v.len() {
                    val = val.wrapping_add((i as u8 % 5).wrapping_mul(3));
                    v[i] = val;
                }
                v
            },
        ),
    ];

    for (label, data) in &test_data {
        // LZMA
        assert_lzma_roundtrip(data, &format!("{} lzma", label));

        // DeltaANS
        assert_delta_ans_roundtrip(data, &format!("{} delta_ans", label));

        // RleHuffman
        assert_rle_huffman_roundtrip(data, &format!("{} rle_huffman", label));

        // BWT (skip very small inputs < 2 bytes — BWT works better on larger data)
        if data.len() >= 2 {
            assert_bwt_roundtrip(data, &format!("{} bwt", label));
        }

        // PPM
        let ppm_enc = ppm::ppm_compress(data);
        let ppm_dec = ppm::ppm_decompress(&ppm_enc);
        assert_eq!(data, &ppm_dec, "PPM roundtrip failed for {}", label);
    }
}

// ============================================================================
// 10. CodecId::from_u8 validation — tests for known mapping bugs
// ============================================================================

#[test]
fn qa_codec_id_from_u8() {
    // Every valid codec ID round-trips through from_u8
    for codec in [
        CodecId::Lz77Huffman,
        CodecId::LzmaStyle,
        CodecId::DeltaAns,
        CodecId::RleHuffman,
        CodecId::Passthrough,
        CodecId::BwtRans,
        CodecId::Ppm,
        CodecId::StrideCm,
    ] {
        assert_eq!(CodecId::from_u8(codec as u8), Some(codec));
    }

    // Unknown IDs are rejected instead of silently mapping to Passthrough
    assert_eq!(CodecId::from_u8(8), None);
    assert_eq!(CodecId::from_u8(255), None);
}

// ============================================================================
// 11. decompress_block_adaptive fallback behavior
// ============================================================================

#[test]
fn qa_decompress_fallback_behavior() {
    // A failed LZMA or DeltaANS decode must surface as an error, never as a
    // silent fallback to the baseline decoder.
    let garbage = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
    for codec in [CodecId::LzmaStyle, CodecId::DeltaAns] {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decompress_block_adaptive(codec, &garbage, garbage.len())
        }));
        if let Ok(decoded) = result {
            assert!(decoded.is_err(), "{} decoded garbage without error", codec.name());
        }
    }
}

// ============================================================================
// 12. Adaptive roundtrip on text corpus patterns
// ============================================================================

#[test]
fn qa_adaptive_text_patterns() {
    // English-like text
    let english = b"The quick brown fox jumps over the lazy dog. \
                    Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                    Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua."
        .repeat(100);
    assert_adaptive_roundtrip(&english, "english text 100x");

    // Source code like
    let code = b"fn main() { let x = 42; println!(\"Hello {}\", x); } \
                 struct Foo { bar: i32, baz: String } impl Foo { fn new() -> Self { Self { bar: 0, baz: String::new() } } }"
        .repeat(50);
    assert_adaptive_roundtrip(&code, "source code 50x");

    // XML/HTML like
    let xml = b"<root><item id=\"1\"><name>Test</name><value>42</value></item>\
                <item id=\"2\"><name>Foo</name><value>99</value></item></root>"
        .repeat(100);
    assert_adaptive_roundtrip(&xml, "xml 100x");
}

// ============================================================================
// 13. PPM memory and roundtrip on larger data
// ============================================================================

#[test]
fn qa_ppm_roundtrip_adversarial() {
    // All zeros
    let data = vec![0u8; 5000];
    let enc = ppm::ppm_compress(&data);
    let dec = ppm::ppm_decompress(&enc);
    assert_eq!(data, dec, "PPM failed on all zeros");

    // All 0xFF
    let data = vec![0xFFu8; 5000];
    let enc = ppm::ppm_compress(&data);
    let dec = ppm::ppm_decompress(&enc);
    assert_eq!(data, dec, "PPM failed on all 0xFF");

    // High entropy random
    let data = lcg_bytes(0xBEEF, 4096);
    let enc = ppm::ppm_compress(&data);
    let dec = ppm::ppm_decompress(&enc);
    assert_eq!(data, dec, "PPM failed on random data");

    // Alternating bytes
    let data: Vec<u8> = (0..4096).map(|i| if i % 2 == 0 { 0x00 } else { 0xFF }).collect();
    let enc = ppm::ppm_compress(&data);
    let dec = ppm::ppm_decompress(&enc);
    assert_eq!(data, dec, "PPM failed on alternating bytes");
}

// ============================================================================
// 14. BWT on 100KB+ blocks (where bugs hide)
// ============================================================================

#[test]
fn qa_bwt_large_block_roundtrip() {
    // 100KB of repetitive text
    let pattern = b"Data compression is the art of reducing redundancy. ";
    let mut data = Vec::with_capacity(102400);
    while data.len() < 102400 {
        data.extend_from_slice(pattern);
    }
    data.truncate(102400);
    assert_bwt_roundtrip(&data, "100KB text");

    // 200KB of binary-ish data (low alphabet)
    let data: Vec<u8> = (0..204800).map(|i| (i % 4) as u8).collect();
    assert_bwt_roundtrip(&data, "200KB low alphabet");

    // 100KB random
    let data = lcg_bytes(0xFACE, 102400);
    assert_bwt_roundtrip(&data, "100KB random");
}

// ============================================================================
// 15. Check for no integrity verification (checksum)
// ============================================================================

#[test]
fn qa_no_checksum_verification() {
    // A corrupted payload byte must be reported, never returned as data.
    let data = b"This is important data that should be verified. ".repeat(20);
    let mut compressed = adaptive_compress(&data);
    let payload_start = 16 + 14; // file header + one block header
    compressed[payload_start + 2] ^= 0xFF;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        nexcomp::adaptive::try_adaptive_decompress(&compressed)
    }));
    assert!(!matches!(result, Ok(Ok(ref d)) if d != &data), "corrupt data returned without error");
}

// ============================================================================
// 16. Adaptive pipeline on very small sizes (2, 3, 4 bytes)
// ============================================================================

#[test]
fn qa_adaptive_very_small() {
    for size in 2..=10 {
        let data: Vec<u8> = (0..size as u8).collect();
        assert_adaptive_roundtrip(&data, &format!("{} bytes sequential", size));

        let data = vec![0xAA; size];
        assert_adaptive_roundtrip(&data, &format!("{} bytes same", size));
    }
}

// ============================================================================
// 17. Ensure probability updates are identical between encoder and decoder
// ============================================================================

#[test]
fn qa_range_coder_prob_sync() {
    // Encode a specific bit sequence and verify probabilities stay in sync
    let bits: Vec<u32> = vec![
        0, 0, 0, 1, 0, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 1,
        0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 1, 1,
    ];

    // Track probabilities during encoding
    let mut enc = RangeEncoder::new();
    let mut prob_enc: Prob = PROB_INIT;
    let mut prob_trace_enc = Vec::new();
    for &b in &bits {
        prob_trace_enc.push(prob_enc);
        enc.encode_bit(&mut prob_enc, b);
    }
    let compressed = enc.finish();

    // Track probabilities during decoding
    let mut dec = RangeDecoder::new(&compressed);
    let mut prob_dec: Prob = PROB_INIT;
    let mut prob_trace_dec = Vec::new();
    for (i, &expected) in bits.iter().enumerate() {
        prob_trace_dec.push(prob_dec);
        let got = dec.decode_bit(&mut prob_dec);
        assert_eq!(got, expected, "bit mismatch at {}", i);
    }

    // Verify probability traces match exactly
    assert_eq!(
        prob_trace_enc, prob_trace_dec,
        "Probability update traces must be identical between encoder and decoder"
    );
}

// ============================================================================
// 18. Range coder encode_freq / decode_freq roundtrip
// ============================================================================

#[test]
fn qa_range_coder_freq_roundtrip() {
    // Test the frequency-based encoding used by PPM
    let symbols: Vec<(u32, u32, u32)> = vec![
        // (cum_low, freq, total)
        (0, 10, 100),
        (10, 5, 100),
        (90, 10, 100),
        (0, 1, 256),
        (255, 1, 256),
        (0, 4000, 4096),
        (4000, 96, 4096),
    ];

    let mut enc = RangeEncoder::new();
    for &(cum, freq, total) in &symbols {
        enc.encode_freq(cum, freq, total);
    }
    let compressed = enc.finish();

    let mut dec = RangeDecoder::new(&compressed);
    for (i, &(expected_cum, freq, total)) in symbols.iter().enumerate() {
        let target = dec.get_freq(total);
        assert!(
            target >= expected_cum && target < expected_cum + freq,
            "freq decode mismatch at symbol {}: target={} expected range [{}, {})",
            i,
            target,
            expected_cum,
            expected_cum + freq
        );
        dec.decode_freq(expected_cum, freq, total);
    }
}

// ============================================================================
// 19. Global mutable state check (thread safety)
// ============================================================================

#[test]
fn qa_no_global_mutable_state() {
    // Run two compressions in parallel to verify no shared mutable state.
    // If there were static mut variables, this would likely fail or produce
    // incorrect results.
    use std::thread;

    let data1 = b"Thread safety test data one, repeated many times for testing. ".repeat(100);
    let data2 = b"Different data for thread two, also repeated many times here. ".repeat(100);

    let d1 = data1.clone();
    let d2 = data2.clone();

    let h1 = thread::spawn(move || {
        let compressed = adaptive_compress(&d1);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(&d1[..], &decompressed[..], "Thread 1 roundtrip failed");
    });

    let h2 = thread::spawn(move || {
        let compressed = adaptive_compress(&d2);
        let decompressed = adaptive_decompress(&compressed);
        assert_eq!(&d2[..], &decompressed[..], "Thread 2 roundtrip failed");
    });

    h1.join().expect("Thread 1 panicked");
    h2.join().expect("Thread 2 panicked");
}

// ============================================================================
// 20. Adaptive with Calgary/Canterbury-like file patterns (if not on disk)
// ============================================================================

#[test]
fn qa_adaptive_corpus_patterns() {
    // Simulate various file types found in standard corpora

    // "book"-like: repetitive English prose
    let book = b"It was the best of times, it was the worst of times, \
                 it was the age of wisdom, it was the age of foolishness. "
        .repeat(500);
    assert_adaptive_roundtrip(&book, "book-like text");

    // "progc"-like: C source code
    let progc = b"#include <stdio.h>\nint main() {\n    printf(\"hello\\n\");\n    return 0;\n}\n"
        .repeat(200);
    assert_adaptive_roundtrip(&progc, "C source code");

    // "geo"-like: structured 32-bit values
    let mut geo = Vec::with_capacity(102400);
    let mut val: u32 = 1000;
    for _ in 0..25600 {
        geo.extend_from_slice(&val.to_le_bytes());
        val = val.wrapping_add(7);
    }
    assert_adaptive_roundtrip(&geo, "geo-like structured");

    // "pic"-like: few unique bytes, long runs
    let mut pic = Vec::with_capacity(100000);
    for _ in 0..500 {
        pic.extend(std::iter::repeat(0x00).take(150));
        pic.extend(std::iter::repeat(0xFF).take(50));
    }
    assert_adaptive_roundtrip(&pic, "pic-like binary");

    // "obj"-like: x86 binary with CALL instructions
    let mut obj = vec![0x90u8; 50000];
    obj[0] = 0x7F; obj[1] = b'E'; obj[2] = b'L'; obj[3] = b'F';
    let mut pos = 16;
    let mut lcg: u32 = 0x1234;
    while pos + 5 < obj.len() {
        obj[pos] = 0xE8;
        let target = (lcg % 4096) as i32;
        let bytes = target.to_le_bytes();
        obj[pos + 1] = bytes[0];
        obj[pos + 2] = bytes[1];
        obj[pos + 3] = bytes[2];
        obj[pos + 4] = bytes[3];
        lcg = lcg.wrapping_mul(1103515245).wrapping_add(12345);
        pos += 5 + (lcg as usize % 8);
    }
    assert_adaptive_roundtrip(&obj, "x86 obj-like binary");
}

// ============================================================================
// 21. Calgary corpus files (if available on disk)
// ============================================================================

#[test]
fn qa_adaptive_all_calgary_lossless() {
    let base = "/tmp/nexcomp_corpora/calgary";
    let files = [
        "bib", "book1", "book2", "geo", "news", "obj1", "obj2",
        "paper1", "paper2", "paper3", "paper4", "paper5", "paper6",
        "pic", "progc", "progl", "progp", "trans",
    ];

    let mut tested = 0;
    for name in &files {
        let path = format!("{}/{}", base, name);
        if let Ok(data) = std::fs::read(&path) {
            assert_adaptive_roundtrip(&data, &format!("calgary/{}", name));
            tested += 1;
        }
    }

    if tested == 0 {
        eprintln!("SKIPPED: Calgary corpus not found at {}", base);
    } else {
        eprintln!("Tested {} Calgary files", tested);
    }
}

#[test]
fn qa_adaptive_all_canterbury_lossless() {
    let base = "/tmp/nexcomp_corpora/canterbury";
    let files = [
        "alice29.txt", "asyoulik.txt", "cp.html", "fields.c", "grammar.lsp",
        "kennedy.xls", "lcet10.txt", "plrabn12.txt", "ptt5", "sum",
        "xargs.1",
    ];

    let mut tested = 0;
    for name in &files {
        let path = format!("{}/{}", base, name);
        if let Ok(data) = std::fs::read(&path) {
            assert_adaptive_roundtrip(&data, &format!("canterbury/{}", name));
            tested += 1;
        }
    }

    if tested == 0 {
        eprintln!("SKIPPED: Canterbury corpus not found at {}", base);
    } else {
        eprintln!("Tested {} Canterbury files", tested);
    }
}

// ============================================================================
// 22. BWT multi-block seam verification
// ============================================================================

#[test]
fn qa_bwt_multi_block_seam() {
    // Verify data spanning exactly 2 BWT blocks decompresses correctly
    // especially at the seam between blocks
    let block_size = bwt_codec::BWT_BLOCK_SIZE;
    let total = block_size * 2;

    // Create data with a known pattern across the block boundary
    let mut data = Vec::with_capacity(total);
    for i in 0..total {
        data.push((i % 251) as u8); // prime modulus to avoid alignment artifacts
    }

    let compressed = bwt_codec::bwt_compress(&data);
    let decompressed = bwt_codec::bwt_decompress(&compressed);
    assert_eq!(data.len(), decompressed.len(), "BWT multi-block length mismatch");
    assert_eq!(data, decompressed, "BWT multi-block seam corruption");

    // Specifically verify the bytes around the seam
    let seam_start = block_size - 10;
    let seam_end = block_size + 10;
    assert_eq!(
        &data[seam_start..seam_end],
        &decompressed[seam_start..seam_end],
        "BWT block seam data mismatch"
    );
}

// ============================================================================
// 23. LZMA stress test — data with many different match distances
// ============================================================================

#[test]
fn qa_lzma_varied_distances() {
    // Create data with matches at various distances
    let mut data = Vec::with_capacity(65536);

    // Short-distance matches
    for _ in 0..100 {
        data.extend_from_slice(b"ABCD");
    }
    // Medium-distance matches
    let chunk: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
    data.extend_from_slice(&chunk);
    data.extend_from_slice(&chunk); // repeat at distance 1000

    // Long-distance matches
    let far_chunk = b"unique pattern far away ".repeat(10);
    data.extend_from_slice(&far_chunk);
    data.extend(vec![0u8; 30000]); // gap
    data.extend_from_slice(&far_chunk); // repeat at distance > 30000

    assert_lzma_roundtrip(&data, "varied distances");
}

// ============================================================================
// 24. DeltaANS stride selection
// ============================================================================

#[test]
fn qa_delta_ans_stride_modes() {
    // Verify all delta modes produce correct roundtrips

    // Mode 0 (xor stride-1) works best on slowly varying data
    let data: Vec<u8> = (0..8192).map(|i| (i & 0xFF) as u8).collect();
    assert_delta_ans_roundtrip(&data, "stride-1 candidate");

    // Mode 2 (xor stride-2) works best on 16-bit structured data
    let mut data = Vec::with_capacity(8192);
    let mut val: u16 = 100;
    for _ in 0..4096 {
        data.extend_from_slice(&val.to_le_bytes());
        val = val.wrapping_add(3);
    }
    assert_delta_ans_roundtrip(&data, "stride-2 candidate");

    // Mode 3 (xor stride-4) works best on 32-bit structured data
    let mut data = Vec::with_capacity(8192);
    let mut val: u32 = 1000;
    for _ in 0..2048 {
        data.extend_from_slice(&val.to_le_bytes());
        val = val.wrapping_add(7);
    }
    assert_delta_ans_roundtrip(&data, "stride-4 candidate");

    // Mode 4 (no delta) best on random
    let data = lcg_bytes(0x5678, 8192);
    assert_delta_ans_roundtrip(&data, "no-delta candidate");
}

// ============================================================================
// 25. RleHuffman with single unique value
// ============================================================================

#[test]
fn qa_rle_huffman_single_unique() {
    // Single byte value repeated — mode 1 should handle this
    for val in [0x00u8, 0x42, 0xFF] {
        let data = vec![val; 100000];
        assert_rle_huffman_roundtrip(&data, &format!("100K of 0x{:02X}", val));
    }
}

// ============================================================================
// 26. CodecId::name() exhaustiveness
// ============================================================================

#[test]
fn qa_codec_id_name_all_variants() {
    // Verify name() returns a non-empty string for all known IDs
    let ids = [
        CodecId::Lz77Huffman,
        CodecId::LzmaStyle,
        CodecId::DeltaAns,
        CodecId::RleHuffman,
        CodecId::Passthrough,
        CodecId::BwtRans,
        // CodecId::Ppm — if this were used, name() would need to handle it
    ];

    for id in &ids {
        let name = id.name();
        assert!(!name.is_empty(), "CodecId {:?} has empty name", id);
    }
}
