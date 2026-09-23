// NEXCOMP integration tests — full pipeline round-trip verification

use nexcomp::classifier::{classify_block, DomainType};
use nexcomp::entropy::{
    build_decode_table, build_table, normalize_freqs, rans_decode, rans_encode,
};
use nexcomp::grammar::{repair_decode, repair_deserialize, repair_encode, repair_serialize};
use nexcomp::selector::{compress_block_adaptive, decompress_block};
use nexcomp::transform::{apply_transform, inverse_transform};

/// Full pipeline round-trip: input → Re-Pair → rANS → rANS⁻¹ → Re-Pair⁻¹ → output
/// Verifies bit-perfect reconstruction.
#[test]
fn test_full_pipeline_roundtrip() {
    let input = include_bytes!("../Cargo.toml"); // Use our own Cargo.toml as test data

    // Stage 1: Classify
    let domain = classify_block(input).expect("classify ok");
    assert!(
        domain == DomainType::Text || domain == DomainType::CodeSrc || domain == DomainType::StructuredData,
        "Cargo.toml should classify as text-like, got {:?}",
        domain
    );

    // Stage 3: Re-Pair compress
    let grammar = repair_encode(input).expect("repair encode ok");
    let grammar_bytes = repair_serialize(&grammar);

    // Stage 5: rANS encode
    let mut counts = vec![0u64; 256];
    for &b in &grammar_bytes {
        counts[b as usize] += 1;
    }
    let freqs = normalize_freqs(&counts, 256);
    let table = build_table(&freqs).expect("build table ok");
    let encoded = rans_encode(&grammar_bytes, &table).expect("rans encode ok");

    // Decode: rANS → Re-Pair
    let dtable = build_decode_table(&table);
    let decoded_grammar_bytes =
        rans_decode(&encoded, &dtable, grammar_bytes.len()).expect("rans decode ok");
    assert_eq!(grammar_bytes, decoded_grammar_bytes, "rANS round-trip failed");

    let decoded_grammar = repair_deserialize(&decoded_grammar_bytes).expect("deserialize ok");
    let output = repair_decode(&decoded_grammar);

    assert_eq!(
        input.as_slice(),
        output.as_slice(),
        "Full pipeline round-trip failed: input ≠ output"
    );

    // Print compression stats
    let ratio = encoded.len() as f64 / input.len() as f64;
    let bpb = ratio * 8.0;
    eprintln!(
        "Pipeline test: {} bytes → {} grammar → {} rANS ({:.1}% ratio, {:.2} bpb)",
        input.len(),
        grammar_bytes.len(),
        encoded.len(),
        ratio * 100.0,
        bpb
    );
}

/// Adaptive v1.2 path: repetitive English text should strongly favor a compressed candidate
/// and remain lossless after selector fallback logic.
#[test]
fn test_pipeline_repetitive_text() {
    let base = "the quick brown fox jumps over the lazy dog and the cat sat on the mat ";
    let input: Vec<u8> = base.repeat(15000).into_bytes(); // ~1MB

    let compressed = compress_block_adaptive(&input).expect("compress ok");
    let output = decompress_block(compressed.codec, &compressed.payload).expect("decode ok");

    assert_eq!(input, output, "Round-trip failed for repetitive text");

    let ratio = compressed.payload.len() as f64 / input.len() as f64;
    eprintln!(
        "Adaptive repetitive text: codec={:?} {} → {} bytes ({:.1}% ratio)",
        compressed.codec,
        input.len(),
        compressed.payload.len(),
        ratio * 100.0
    );
    assert!(
        ratio < 0.30,
        "Highly repetitive text should compress to <30%, got {:.1}%",
        ratio * 100.0
    );
}

/// Full pipeline with domain transforms: classify → transform → Re-Pair → rANS → decode → inverse transform
#[test]
fn test_full_pipeline_with_domain_transforms() {
    let block_size = 64 * 1024;
    let base = "The Burrows-Wheeler Transform is a reversible transformation. \
                Data compression is fundamental to computer science. ";
    let input: Vec<u8> = base.repeat(200).into_bytes(); // ~24KB of English text

    // Stage 1: Classify blocks
    let mut domain_map: Vec<u8> = Vec::new();
    let mut domains: Vec<DomainType> = Vec::new();
    for chunk in input.chunks(block_size) {
        let domain = classify_block(chunk).expect("classify ok");
        domain_map.push(domain as u8);
        domains.push(domain);
    }

    // Stage 2: Apply domain transforms per block
    let mut transformed = Vec::new();
    let mut block_sizes: Vec<usize> = Vec::new();
    for (i, chunk) in input.chunks(block_size).enumerate() {
        let t = apply_transform(chunk, domains[i]);
        block_sizes.push(t.len());
        transformed.extend_from_slice(&t);
    }

    // Stage 3: Re-Pair compress
    let grammar = repair_encode(&transformed).expect("repair encode ok");
    let grammar_bytes = repair_serialize(&grammar);

    // Stage 5: rANS encode
    let mut counts = vec![0u64; 256];
    for &b in &grammar_bytes {
        counts[b as usize] += 1;
    }
    let freqs = normalize_freqs(&counts, 256);
    let table = build_table(&freqs).expect("build table ok");
    let encoded = rans_encode(&grammar_bytes, &table).expect("rans encode ok");

    // --- Decode ---

    // rANS decode
    let dtable = build_decode_table(&table);
    let decoded_grammar_bytes =
        rans_decode(&encoded, &dtable, grammar_bytes.len()).expect("rans decode ok");
    assert_eq!(grammar_bytes, decoded_grammar_bytes, "rANS round-trip failed");

    // Re-Pair decode
    let decoded_grammar = repair_deserialize(&decoded_grammar_bytes).expect("deserialize ok");
    let transformed_back = repair_decode(&decoded_grammar);
    assert_eq!(transformed, transformed_back, "Re-Pair round-trip failed");

    // Stage 2 inverse: Split blocks and apply inverse transform
    let mut output = Vec::new();
    let mut pos = 0;
    for (i, &sz) in block_sizes.iter().enumerate() {
        let block = &transformed_back[pos..pos + sz];
        let domain = DomainType::from(domain_map[i]);
        let original = inverse_transform(block, domain);
        output.extend_from_slice(&original);
        pos += sz;
    }

    assert_eq!(
        input, output,
        "Full pipeline with domain transforms: round-trip failed"
    );

    let ratio = encoded.len() as f64 / input.len() as f64;
    eprintln!(
        "Domain transform pipeline: {} bytes → {} bytes ({:.1}% ratio)",
        input.len(),
        encoded.len(),
        ratio * 100.0
    );
}

/// Crypto round-trip test
#[test]
fn test_crypto_roundtrip() {
    let data = b"NEXCOMP compressed payload for crypto test - includes special chars: {}[]<>";
    let key = b"test-master-key-for-integration!";
    let header = b"NXC\x01\x01\x00";

    let encrypted = nexcomp::crypto::encrypt(data, key, header).expect("encrypt ok");
    let decrypted = nexcomp::crypto::decrypt(&encrypted, key, header).expect("decrypt ok");

    assert_eq!(data.as_slice(), decrypted.as_slice());
}

/// A 4 MiB block can hold a run longer than any single RLE length class, and a
/// sparse file is exactly where that happens. The compressor used to panic
/// inside rayon and take the process with it.
#[test]
fn a_run_longer_than_the_largest_length_class_compresses() {
    let mut data = vec![0u8; 2_250_593];
    data.extend_from_slice(&[1u8; 100]);
    let encoded = nexcomp::adaptive::adaptive_compress(&data);
    assert_eq!(nexcomp::adaptive::try_adaptive_decompress(&encoded).unwrap(), data);
}
