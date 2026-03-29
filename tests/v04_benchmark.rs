// NEXCOMP v0.4 — Benchmark all ramas against corpus
//
// Tests each compression strategy independently on the 3.2MB tar corpus
// and reports exact bpb, speed, and sizes.

use nexcomp::codecs::{classify_block_type, rle_encode, rle_decode, BlockType};
use nexcomp::context_model::ContextModel;
use nexcomp::entropy::{build_table, build_decode_table, normalize_freqs, rans_encode, rans_decode};
use nexcomp::grammar::{repair_decode, repair_deserialize, repair_encode, repair_serialize};
use nexcomp::lz77;
use nexcomp::repair_fast;
use nexcomp::transform::{apply_transform, inverse_transform};
use nexcomp::classifier::{classify_block, DomainType};

use std::time::Instant;

const BLOCK_SIZE: usize = 64 * 1024;

/// Pipeline v0.3: LZ77 → BWT+MTF → Re-Pair → rANS(order-0)
fn compress_v3(data: &[u8]) -> Vec<u8> {
    // LZ77
    let lz_data = lz77::compress(data);

    // Per-block BWT+MTF
    let mut transformed = Vec::new();
    let mut block_info: Vec<(u8, u32)> = Vec::new(); // (domain, transformed_size)
    for chunk in lz_data.chunks(BLOCK_SIZE) {
        let domain = classify_block(chunk).unwrap_or(DomainType::BinaryGeneric);
        let t = apply_transform(chunk, domain);
        block_info.push((domain as u8, t.len() as u32));
        transformed.extend_from_slice(&t);
    }

    // Re-Pair
    if transformed.is_empty() {
        return Vec::new();
    }
    let grammar = repair_encode(&transformed).unwrap();
    let grammar_bytes = repair_serialize(&grammar);

    // rANS order-0
    let mut counts = vec![0u64; 256];
    for &b in &grammar_bytes { counts[b as usize] += 1; }
    let freqs = normalize_freqs(&counts, 256);
    let table = build_table(&freqs).unwrap();
    let encoded = rans_encode(&grammar_bytes, &table).unwrap();

    // Pack: block_info + freq_table + grammar_len + encoded
    let mut out = Vec::new();
    out.extend_from_slice(&(block_info.len() as u32).to_le_bytes());
    for (d, sz) in &block_info {
        out.push(*d);
        out.extend_from_slice(&sz.to_le_bytes());
    }
    for &f in &freqs { out.extend_from_slice(&(f as u16).to_le_bytes()); }
    out.extend_from_slice(&(grammar_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&encoded);
    out
}

fn decompress_v3(payload: &[u8]) -> Vec<u8> {
    let mut off = 0;
    let nblocks = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]) as usize;
    off += 4;
    let mut block_info = Vec::new();
    for _ in 0..nblocks {
        let d = payload[off]; off += 1;
        let sz = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]) as usize;
        off += 4;
        block_info.push((d, sz));
    }
    let mut freqs = vec![0u32; 256];
    for i in 0..256 {
        freqs[i] = payload[off + i*2] as u32 | ((payload[off + i*2 + 1] as u32) << 8);
    }
    off += 512;
    let grammar_len = u64::from_le_bytes([
        payload[off], payload[off+1], payload[off+2], payload[off+3],
        payload[off+4], payload[off+5], payload[off+6], payload[off+7],
    ]) as usize;
    off += 8;
    let rans_data = &payload[off..];

    let table = build_table(&freqs).unwrap();
    let dtable = build_decode_table(&table);
    let grammar_bytes = rans_decode(rans_data, &dtable, grammar_len).unwrap();
    let grammar = repair_deserialize(&grammar_bytes).unwrap();
    let transformed = repair_decode(&grammar);

    let mut lz_data = Vec::new();
    let mut pos = 0;
    for (d, sz) in &block_info {
        let block = &transformed[pos..pos + sz];
        let domain = DomainType::from(*d);
        let original = inverse_transform(block, domain);
        lz_data.extend_from_slice(&original);
        pos += *sz;
    }

    lz77::decompress(&lz_data).unwrap()
}

/// Rama A: LZ77 → rANS(order-1 context model), no BWT/Re-Pair
fn compress_rama_a(data: &[u8]) -> Vec<u8> {
    let lz_data = lz77::compress(data);

    // Train order-1 model on the LZ77 token stream
    let model = ContextModel::train(&lz_data);

    // Encode with context model
    let encoded = model.encode(&lz_data).expect("rama A encode failed");

    // Serialize model + encoded data
    let model_bytes = model.serialize_model();

    let mut out = Vec::new();
    out.extend_from_slice(&(lz_data.len() as u64).to_le_bytes()); // original LZ stream length
    out.extend_from_slice(&(model_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&model_bytes);
    out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
    out.extend_from_slice(&encoded);
    out
}

fn decompress_rama_a(payload: &[u8]) -> Vec<u8> {
    let mut off = 0;
    let lz_len = u64::from_le_bytes([
        payload[off], payload[off+1], payload[off+2], payload[off+3],
        payload[off+4], payload[off+5], payload[off+6], payload[off+7],
    ]) as usize;
    off += 8;
    let model_len = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]) as usize;
    off += 4;
    let model = ContextModel::deserialize_model(&payload[off..off + model_len])
        .expect("rama A model deserialize failed");
    off += model_len;
    let enc_len = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]) as usize;
    off += 4;
    let encoded = &payload[off..off + enc_len];

    let lz_data = model.decode(encoded, lz_len).expect("rama A decode failed");
    lz77::decompress(&lz_data).unwrap()
}

/// Rama B: LZ77 → BWT+MTF → fast Re-Pair → rANS(order-0)
fn compress_rama_b(data: &[u8]) -> Vec<u8> {
    let lz_data = lz77::compress(data);

    // Per-block BWT+MTF
    let mut transformed = Vec::new();
    let mut block_info: Vec<(u8, u32)> = Vec::new();
    for chunk in lz_data.chunks(BLOCK_SIZE) {
        let domain = classify_block(chunk).unwrap_or(DomainType::BinaryGeneric);
        let t = apply_transform(chunk, domain);
        block_info.push((domain as u8, t.len() as u32));
        transformed.extend_from_slice(&t);
    }

    if transformed.is_empty() { return Vec::new(); }

    // Fast Re-Pair (optimized)
    let grammar = repair_fast::repair_encode_fast(&transformed).unwrap();
    let grammar_bytes = repair_serialize(&grammar);

    // rANS order-0
    let mut counts = vec![0u64; 256];
    for &b in &grammar_bytes { counts[b as usize] += 1; }
    let freqs = normalize_freqs(&counts, 256);
    let table = build_table(&freqs).unwrap();
    let encoded = rans_encode(&grammar_bytes, &table).unwrap();

    let mut out = Vec::new();
    out.extend_from_slice(&(block_info.len() as u32).to_le_bytes());
    for (d, sz) in &block_info {
        out.push(*d);
        out.extend_from_slice(&sz.to_le_bytes());
    }
    for &f in &freqs { out.extend_from_slice(&(f as u16).to_le_bytes()); }
    out.extend_from_slice(&(grammar_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&encoded);
    out
}

// Rama B decompress = same as v3 decompress (same format, different encoder)
fn decompress_rama_b(payload: &[u8]) -> Vec<u8> {
    decompress_v3(payload) // Same decode path — Re-Pair decode is universal
}

/// Rama C: per-block classification with RLE for zeros
fn compress_rama_c(data: &[u8]) -> Vec<u8> {
    let lz_data = lz77::compress(data);

    let mut out = Vec::new();
    let blocks: Vec<&[u8]> = lz_data.chunks(BLOCK_SIZE).collect();
    out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());

    for chunk in &blocks {
        let btype = classify_block_type(chunk);

        match btype {
            BlockType::Zeros | BlockType::LowEntropy => {
                let rle = rle_encode(chunk);
                out.push(btype as u8);
                out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                out.extend_from_slice(&(rle.len() as u32).to_le_bytes());
                out.extend_from_slice(&rle);
            }
            _ => {
                // Use standard pipeline for text/binary blocks
                let domain = classify_block(chunk).unwrap_or(DomainType::BinaryGeneric);
                let t = apply_transform(chunk, domain);

                if t.is_empty() {
                    out.push(0xFF); // stored
                    out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                    out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                    out.extend_from_slice(chunk);
                    continue;
                }

                let grammar = repair_fast::repair_encode_fast(&t).unwrap();
                let gb = repair_serialize(&grammar);
                let mut counts = vec![0u64; 256];
                for &b in &gb { counts[b as usize] += 1; }
                let freqs = normalize_freqs(&counts, 256);
                let table = build_table(&freqs).unwrap();
                let encoded = rans_encode(&gb, &table).unwrap();

                // Pack block
                let mut block_data = Vec::new();
                block_data.push(domain as u8);
                block_data.extend_from_slice(&(t.len() as u32).to_le_bytes());
                for &f in &freqs { block_data.extend_from_slice(&(f as u16).to_le_bytes()); }
                block_data.extend_from_slice(&(gb.len() as u64).to_le_bytes());
                block_data.extend_from_slice(&encoded);

                out.push(btype as u8 | 0x10); // mark as compressed block
                out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
                out.extend_from_slice(&(block_data.len() as u32).to_le_bytes());
                out.extend_from_slice(&block_data);
            }
        }
    }
    out
}

fn decompress_rama_c(payload: &[u8]) -> Vec<u8> {
    let mut off = 0;
    let nblocks = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]) as usize;
    off += 4;

    let mut lz_data = Vec::new();

    for _ in 0..nblocks {
        let tag = payload[off]; off += 1;
        let orig_len = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]) as usize;
        off += 4;
        let comp_len = u32::from_le_bytes([payload[off], payload[off+1], payload[off+2], payload[off+3]]) as usize;
        off += 4;
        let block_payload = &payload[off..off + comp_len];
        off += comp_len;

        if tag == BlockType::Zeros as u8 || tag == BlockType::LowEntropy as u8 {
            let decoded = rle_decode(block_payload).unwrap();
            lz_data.extend_from_slice(&decoded);
        } else if tag == 0xFF {
            lz_data.extend_from_slice(block_payload);
        } else {
            // Compressed block
            let mut boff = 0;
            let domain = DomainType::from(block_payload[boff]); boff += 1;
            let t_len = u32::from_le_bytes([
                block_payload[boff], block_payload[boff+1], block_payload[boff+2], block_payload[boff+3]
            ]) as usize;
            boff += 4;
            let mut freqs = vec![0u32; 256];
            for i in 0..256 {
                freqs[i] = block_payload[boff + i*2] as u32 | ((block_payload[boff + i*2 + 1] as u32) << 8);
            }
            boff += 512;
            let grammar_len = u64::from_le_bytes([
                block_payload[boff], block_payload[boff+1], block_payload[boff+2], block_payload[boff+3],
                block_payload[boff+4], block_payload[boff+5], block_payload[boff+6], block_payload[boff+7],
            ]) as usize;
            boff += 8;
            let rans_data = &block_payload[boff..];

            let table = build_table(&freqs).unwrap();
            let dtable = build_decode_table(&table);
            let gb = rans_decode(rans_data, &dtable, grammar_len).unwrap();
            let grammar = repair_deserialize(&gb).unwrap();
            let transformed = repair_decode(&grammar);

            let original = inverse_transform(&transformed[..t_len], domain);
            lz_data.extend_from_slice(&original);
        }
    }

    lz77::decompress(&lz_data).unwrap()
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[test]
fn test_rama_a_roundtrip() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_rama_a(&data);
    let decompressed = decompress_rama_a(&compressed);
    assert_eq!(data, decompressed, "Rama A round-trip failed");
}

#[test]
fn test_rama_b_roundtrip() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_rama_b(&data);
    let decompressed = decompress_rama_b(&compressed);
    assert_eq!(data, decompressed, "Rama B round-trip failed");
}

#[test]
fn test_rama_c_roundtrip() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_rama_c(&data);
    let decompressed = decompress_rama_c(&compressed);
    assert_eq!(data, decompressed, "Rama C round-trip failed");
}

#[test]
fn test_v3_roundtrip_benchmark() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let compressed = compress_v3(&data);
    let decompressed = decompress_v3(&compressed);
    assert_eq!(data, decompressed, "V3 standalone round-trip failed");
}

#[test]
fn test_zeros_rle_roundtrip() {
    let data = vec![0u8; 65536];
    let rle = rle_encode(&data);
    let decoded = rle_decode(&rle).unwrap();
    assert_eq!(data, decoded);
    assert!(rle.len() < 20, "RLE of 64KB zeros should be tiny, got {} bytes", rle.len());
}

#[test]
fn test_classifier_zeros() {
    let data = vec![0u8; 65536];
    assert_eq!(classify_block_type(&data), BlockType::Zeros);
}

#[test]
fn test_classifier_text() {
    let data = b"The quick brown fox jumps over the lazy dog. ".repeat(1400);
    assert_eq!(classify_block_type(&data), BlockType::Text);
}

#[test]
fn test_classifier_binary() {
    let data: Vec<u8> = (0..65536u32).map(|i| ((i.wrapping_mul(2654435761)) >> 16) as u8).collect();
    let bt = classify_block_type(&data);
    assert!(bt == BlockType::Binary || bt == BlockType::Text,
        "Random-ish data should be Binary or Text, got {:?}", bt);
}

#[test]
fn test_full_benchmark() {
    let data = std::fs::read("tests/fixtures/corpus.tar").unwrap();
    let input_size = data.len();

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  NEXCOMP v0.4 — Full Benchmark (corpus 3.2MB tar)");
    eprintln!("{}\n", "=".repeat(70));

    // V3 pipeline
    let t0 = Instant::now();
    let v3 = compress_v3(&data);
    let v3_comp_time = t0.elapsed().as_secs_f64();
    let t0 = Instant::now();
    let v3_dec = decompress_v3(&v3);
    let v3_dec_time = t0.elapsed().as_secs_f64();
    assert_eq!(data, v3_dec, "V3 round-trip FAILED");
    let v3_bpb = v3.len() as f64 * 8.0 / input_size as f64;

    // Rama A
    let t0 = Instant::now();
    let a = compress_rama_a(&data);
    let a_comp_time = t0.elapsed().as_secs_f64();
    let t0 = Instant::now();
    let a_dec = decompress_rama_a(&a);
    let a_dec_time = t0.elapsed().as_secs_f64();
    assert_eq!(data, a_dec, "Rama A round-trip FAILED");
    let a_bpb = a.len() as f64 * 8.0 / input_size as f64;

    // Rama B
    let t0 = Instant::now();
    let b = compress_rama_b(&data);
    let b_comp_time = t0.elapsed().as_secs_f64();
    let t0 = Instant::now();
    let b_dec = decompress_rama_b(&b);
    let b_dec_time = t0.elapsed().as_secs_f64();
    assert_eq!(data, b_dec, "Rama B round-trip FAILED");
    let b_bpb = b.len() as f64 * 8.0 / input_size as f64;

    // Rama C
    let t0 = Instant::now();
    let c = compress_rama_c(&data);
    let c_comp_time = t0.elapsed().as_secs_f64();
    let t0 = Instant::now();
    let c_dec = decompress_rama_c(&c);
    let c_dec_time = t0.elapsed().as_secs_f64();
    assert_eq!(data, c_dec, "Rama C round-trip FAILED");
    let c_bpb = c.len() as f64 * 8.0 / input_size as f64;

    // Classify blocks for Rama C stats
    let lz_data = lz77::compress(&data);
    let mut zero_blocks = 0;
    let mut low_ent_blocks = 0;
    let mut text_blocks = 0;
    let mut binary_blocks = 0;
    for chunk in lz_data.chunks(BLOCK_SIZE) {
        match classify_block_type(chunk) {
            BlockType::Zeros => zero_blocks += 1,
            BlockType::LowEntropy => low_ent_blocks += 1,
            BlockType::Text => text_blocks += 1,
            BlockType::Binary => binary_blocks += 1,
        }
    }

    let mb = input_size as f64 / 1_048_576.0;

    eprintln!("Compresor/Config        Bytes       bpb    Comp MB/s  Decomp MB/s  Verify");
    eprintln!("{}", "-".repeat(75));
    eprintln!("xz -6               {:>10}     0.779       —           —         —", 327_820);
    eprintln!("bzip2 -9            {:>10}     1.206       —           —         —", 507_569);
    eprintln!("gzip -9             {:>10}     1.524       —           —         —", 641_307);
    eprintln!("{}", "-".repeat(75));
    eprintln!("NEXCOMP v0.3 (ref)  {:>10}     2.199     0.85        39.8        OK", 925_397);
    eprintln!("{}", "-".repeat(75));
    eprintln!("v0.4-A (ctx ord1)   {:>10}     {:.3}     {:.2}        {:.1}        OK",
        a.len(), a_bpb, mb / a_comp_time, mb / a_dec_time);
    eprintln!("v0.4-B (fast RePair){:>10}     {:.3}     {:.2}        {:.1}        OK",
        b.len(), b_bpb, mb / b_comp_time, mb / b_dec_time);
    eprintln!("v0.4-C (classifier) {:>10}     {:.3}     {:.2}        {:.1}        OK",
        c.len(), c_bpb, mb / c_comp_time, mb / c_dec_time);
    eprintln!("{}", "-".repeat(75));

    // Best of all
    let best_name;
    let best_size;
    let best_bpb;
    if a.len() <= b.len() && a.len() <= c.len() {
        best_name = "v0.4-A"; best_size = a.len(); best_bpb = a_bpb;
    } else if b.len() <= c.len() {
        best_name = "v0.4-B"; best_size = b.len(); best_bpb = b_bpb;
    } else {
        best_name = "v0.4-C"; best_size = c.len(); best_bpb = c_bpb;
    }
    eprintln!("BEST: {} {:>10} bytes  {:.3} bpb", best_name, best_size, best_bpb);
    let beat_gzip = best_bpb < 1.524;
    eprintln!("Beat gzip -9 (1.524 bpb)? {}", if beat_gzip { "YES" } else { "NO" });

    eprintln!("\nBlock classification (post-LZ77):");
    eprintln!("  Zeros: {}  LowEntropy: {}  Text: {}  Binary: {}",
        zero_blocks, low_ent_blocks, text_blocks, binary_blocks);
}
