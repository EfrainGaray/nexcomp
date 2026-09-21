//! PPM (Prediction by Partial Matching) order-5 codec.
//!
//! Models P(byte | last N bytes) for N = 0..5, escaping to lower orders
//! when a context has not been seen. Uses Method D escape estimation with
//! exclusion for efficiency.

use crate::range_coder::{RangeEncoder, RangeDecoder};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

const MAX_ORDER: usize = 5;

// ---------------------------------------------------------------------------
// Frequency table for a single context
// ---------------------------------------------------------------------------

/// Sparse symbol counts in first-seen order (the order defines the coding CDF).
struct FreqTable {
    syms: Vec<(u8, u16)>,
    total: u32,
}

impl FreqTable {
    fn new() -> Self {
        Self {
            syms: Vec::new(),
            total: 0,
        }
    }

    fn update(&mut self, byte: u8) {
        match self.syms.iter_mut().find(|(s, _)| *s == byte) {
            Some((_, c)) => *c += 1,
            None => self.syms.push((byte, 1)),
        }
        self.total += 1;

        // Rescale if total gets too large.
        // The range coder computes r = range / total; with range >= 2^24 and
        // total <= 4096, r >= 4096 which keeps precision loss per symbol small.
        // At 16000 the per-symbol error accumulates over long files (769KB+).
        if self.total > 8192 {
            self.rescale();
        }
    }

    fn rescale(&mut self) {
        self.total = 0;
        for (_, c) in self.syms.iter_mut() {
            *c = (*c + 1) / 2; // halve with rounding up
            self.total += *c as u32;
        }
    }
}

// ---------------------------------------------------------------------------
// PPM model: contexts keyed by (order, last `order` bytes) packed into a u64
// ---------------------------------------------------------------------------

/// Rolling history of the last MAX_ORDER bytes plus how many are valid.
#[derive(Clone, Copy, Default)]
struct History {
    bytes: u64,
    len: usize,
}

impl History {
    fn push(&mut self, byte: u8) {
        self.bytes = ((self.bytes << 8) | byte as u64) & ((1 << (8 * MAX_ORDER)) - 1);
        self.len = (self.len + 1).min(MAX_ORDER);
    }

    /// Key of the order-`order` context (order <= self.len).
    fn key(&self, order: usize) -> u64 {
        ((order as u64) << (8 * MAX_ORDER)) | (self.bytes & ((1u64 << (8 * order)) - 1))
    }
}

/// Multiplicative hasher for the packed u64 context keys.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("context keys are hashed with write_u64");
    }
    fn write_u64(&mut self, key: u64) {
        let h = key.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = h ^ (h >> 29);
    }
}

struct PpmModel {
    contexts: HashMap<u64, FreqTable, BuildHasherDefault<KeyHasher>>,
}

impl PpmModel {
    fn new() -> Self {
        Self {
            contexts: HashMap::default(),
        }
    }

    fn get(&self, key: u64) -> Option<&FreqTable> {
        self.contexts.get(&key)
    }

    /// Update model with observed byte in every context order 0..=history.len.
    fn update(&mut self, history: &History, byte: u8) {
        for order in 0..=history.len {
            self.contexts
                .entry(history.key(order))
                .or_insert_with(FreqTable::new)
                .update(byte);
        }
    }
}

// ---------------------------------------------------------------------------
// PPM encode/decode helpers
// ---------------------------------------------------------------------------

/// Encode a single byte using PPM with exclusion (Method D escape).
///
/// Method D: escape weight = number of distinct symbols seen in this context
/// (excluding already-excluded symbols). This gives a tighter escape estimate
/// than Method C (which uses the number of unseen symbols as escape weight).
fn encode_byte_ppm(
    enc: &mut RangeEncoder,
    model: &PpmModel,
    history: &History,
    byte: u8,
) {
    let mut excluded = [false; 256];

    // Try from highest order down to 0
    for order in (0..=history.len).rev() {
        if let Some(freq) = model.get(history.key(order)) {
            // Total and distinct count over non-excluded symbols (distinct = escape
            // weight), plus the cumulative frequency below `byte` if present.
            let mut total_excl = 0u32;
            let mut num_distinct = 0u32;
            let mut found: Option<(u32, u32)> = None;
            for &(sym, count) in &freq.syms {
                if excluded[sym as usize] {
                    continue;
                }
                if sym == byte {
                    found = Some((total_excl, count as u32));
                }
                total_excl += count as u32;
                num_distinct += 1;
            }

            // If no non-excluded symbols in this context, skip to lower order
            if total_excl == 0 {
                continue;
            }

            let esc_weight = num_distinct;
            let denom = total_excl + esc_weight;

            if let Some((cum_low, byte_count)) = found {
                enc.encode_freq(cum_low, byte_count, denom);
                return;
            }

            // Byte not found in this context -- encode escape
            enc.encode_freq(total_excl, esc_weight, denom);

            // Mark all symbols seen in this context as excluded
            for &(sym, _) in &freq.syms {
                excluded[sym as usize] = true;
            }
        }
        // Context not found -- implicit escape, continue to lower order
    }

    // Order -1: uniform distribution over non-excluded symbols
    let mut non_excluded_count = 0u32;
    let mut cum_low = 0u32;
    let mut found = false;
    for b in 0..256usize {
        if excluded[b] {
            continue;
        }
        if b == byte as usize {
            found = true;
        }
        if !found {
            cum_low += 1;
        }
        non_excluded_count += 1;
    }
    debug_assert!(
        non_excluded_count > 0,
        "all symbols excluded, cannot encode"
    );
    enc.encode_freq(cum_low, 1, non_excluded_count);
}

/// Decode a single byte using PPM with exclusion (Method D escape).
fn decode_byte_ppm(
    dec: &mut RangeDecoder,
    model: &PpmModel,
    history: &History,
) -> u8 {
    let mut excluded = [false; 256];

    // Try from highest order down to 0
    for order in (0..=history.len).rev() {
        if let Some(freq) = model.get(history.key(order)) {
            let mut total_excl = 0u32;
            let mut num_distinct = 0u32;
            for &(sym, count) in &freq.syms {
                if !excluded[sym as usize] {
                    total_excl += count as u32;
                    num_distinct += 1;
                }
            }

            if total_excl == 0 {
                continue;
            }

            let esc_weight = num_distinct;
            let denom = total_excl + esc_weight;

            // Decode: get the cumulative frequency
            let target = dec.get_freq(denom);

            if target < total_excl {
                // It's a real symbol (not escape)
                let mut cum = 0u32;
                for &(sym, count) in &freq.syms {
                    if excluded[sym as usize] {
                        continue;
                    }
                    let c = count as u32;
                    if cum + c > target {
                        dec.decode_freq(cum, c, denom);
                        return sym;
                    }
                    cum += c;
                }
                unreachable!("decode_byte_ppm: symbol not found in CDF");
            }

            // Escape
            dec.decode_freq(total_excl, esc_weight, denom);

            // Mark all symbols seen in this context as excluded
            for &(sym, _) in &freq.syms {
                excluded[sym as usize] = true;
            }
        }
    }

    // Order -1: uniform distribution over non-excluded symbols
    let non_excluded: Vec<u8> = (0..=255u8).filter(|&b| !excluded[b as usize]).collect();
    let count = non_excluded.len() as u32;
    debug_assert!(count > 0, "all symbols excluded, cannot decode");
    let target = dec.get_freq(count);
    let byte = non_excluded[target as usize];
    dec.decode_freq(target, 1, count);
    byte
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compress data using PPM order-5.
///
/// Wire format: [4B original_length LE][range-coded stream]
pub fn ppm_compress(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return vec![0, 0, 0, 0];
    }

    let mut model = PpmModel::new();
    let mut enc = RangeEncoder::new();
    let mut ctx = History::default();

    // Write original length as header
    let mut header = (data.len() as u32).to_le_bytes().to_vec();

    for &byte in data {
        encode_byte_ppm(&mut enc, &model, &ctx, byte);
        model.update(&ctx, byte);
        ctx.push(byte);
    }

    let compressed = enc.finish();
    header.extend_from_slice(&compressed);
    header
}

/// Decompress PPM-compressed data.
pub fn ppm_decompress(payload: &[u8]) -> Vec<u8> {
    if payload.len() < 4 {
        return Vec::new();
    }
    let orig_len =
        u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    if orig_len == 0 {
        return Vec::new();
    }

    let mut model = PpmModel::new();
    let mut dec = RangeDecoder::new(&payload[4..]);
    let mut ctx = History::default();
    let mut output = Vec::with_capacity(orig_len);

    for _ in 0..orig_len {
        let byte = decode_byte_ppm(&mut dec, &model, &ctx);
        output.push(byte);
        model.update(&ctx, byte);
        ctx.push(byte);
    }

    output
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simple() {
        let data = b"Hello World Hello World Hello";
        let compressed = ppm_compress(data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(&data[..], &decompressed[..]);
    }

    #[test]
    fn roundtrip_empty() {
        let data = b"";
        let compressed = ppm_compress(data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(&data[..], &decompressed[..]);
    }

    #[test]
    fn roundtrip_single_byte() {
        let data = b"A";
        let compressed = ppm_compress(data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(&data[..], &decompressed[..]);
    }

    #[test]
    fn roundtrip_repeated() {
        let data = vec![b'a'; 1000];
        let compressed = ppm_compress(&data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(data, decompressed);
        // Repeated single byte should compress well (includes 4B header + range coder overhead)
        assert!(
            compressed.len() < 150,
            "1000 repeated bytes should compress to < 150 bytes, got {}",
            compressed.len()
        );
    }

    #[test]
    fn roundtrip_all_bytes() {
        // All 256 byte values
        let data: Vec<u8> = (0..=255u8).collect();
        let compressed = ppm_compress(&data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(data, decompressed);
    }

    #[test]
    fn roundtrip_text_paragraph() {
        let data = b"The quick brown fox jumps over the lazy dog. \
                     Pack my box with five dozen liquor jugs. \
                     How vexingly quick daft zebras jump. \
                     The five boxing wizards jump quickly.";
        let compressed = ppm_compress(data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(&data[..], &decompressed[..]);
    }

    #[test]
    fn roundtrip_binary_data() {
        // Pseudo-random binary data
        let mut data = vec![0u8; 2048];
        let mut state: u64 = 0xDEAD_BEEF;
        for b in data.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *b = (state >> 33) as u8;
        }
        let compressed = ppm_compress(&data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(data, decompressed);
    }

    #[test]
    fn roundtrip_english_sample() {
        // Load the english sample fixture if available
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/english_sample.txt"
        );
        if let Ok(data) = std::fs::read(path) {
            if !data.is_empty() {
                let compressed = ppm_compress(&data);
                let decompressed = ppm_decompress(&compressed);
                assert_eq!(data, decompressed);
                let bpb = (compressed.len() as f64 * 8.0) / data.len() as f64;
                eprintln!(
                    "PPM english_sample.txt: {} -> {} bytes ({:.3} bpb)",
                    data.len(),
                    compressed.len(),
                    bpb
                );
            }
        }
    }

    #[test]
    fn roundtrip_large_english_text() {
        // Generate ~800KB of English-like text using a deterministic PRNG
        // to simulate book1-scale input and verify no decoder desync.
        let words = [
            "the ", "of ", "and ", "to ", "a ", "in ", "that ", "is ",
            "was ", "he ", "for ", "it ", "with ", "as ", "his ", "on ",
            "be ", "at ", "by ", "I ", "this ", "had ", "not ", "are ",
            "but ", "from ", "or ", "have ", "an ", "they ", "which ",
            "one ", "you ", "were ", "her ", "all ", "she ", "there ",
            "would ", "their ", "we ", "him ", "been ", "has ", "when ",
            "who ", "will ", "more ", "no ", "if ", "out ", "so ", "said ",
        ];
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        let mut data = Vec::with_capacity(800_000);
        while data.len() < 769_000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let idx = ((state >> 33) as usize) % words.len();
            data.extend_from_slice(words[idx].as_bytes());
            // Occasional newline
            if (state >> 40) & 0xF == 0 {
                data.push(b'\n');
            }
        }
        let compressed = ppm_compress(&data);
        let decompressed = ppm_decompress(&compressed);
        assert_eq!(
            data.len(),
            decompressed.len(),
            "length mismatch on 769KB text"
        );
        assert_eq!(data, decompressed, "content mismatch on 769KB text");
        let bpb = (compressed.len() as f64 * 8.0) / data.len() as f64;
        eprintln!(
            "PPM 769KB synthetic text: {} -> {} bytes ({:.3} bpb)",
            data.len(),
            compressed.len(),
            bpb
        );
        assert!(
            bpb < 3.0,
            "PPM bpb on 769KB text should be < 3.0, got {:.3}",
            bpb
        );
    }

    #[test]
    fn roundtrip_calgary_corpus() {
        // Test against real Calgary corpus files if available
        let names = [
            "bib", "book1", "book2", "news", "paper1", "paper2",
        ];
        let base_dir = "/tmp/nexcomp_corpora/calgary";
        let mut any_tested = false;
        for name in &names {
            let path = format!("{}/{}", base_dir, name);
            if let Ok(data) = std::fs::read(&path) {
                if data.len() < 100_000 {
                    continue; // skip small files for this scaling test
                }
                any_tested = true;
                let compressed = ppm_compress(&data);
                let decompressed = ppm_decompress(&compressed);
                assert_eq!(
                    data.len(),
                    decompressed.len(),
                    "{}: length mismatch",
                    name
                );
                assert_eq!(data, decompressed, "{}: content mismatch", name);
                let bpb = (compressed.len() as f64 * 8.0) / data.len() as f64;
                eprintln!(
                    "PPM {}: {} -> {} bytes ({:.3} bpb)",
                    name,
                    data.len(),
                    compressed.len(),
                    bpb
                );
                assert!(
                    bpb < 3.5,
                    "PPM {} bpb should be < 3.5, got {:.3}",
                    name,
                    bpb
                );
            }
        }
        if !any_tested {
            eprintln!("SKIP: Calgary corpus not found at {}/", base_dir);
        }
    }

    #[test]
    fn compression_ratio_text() {
        // PPM should achieve decent compression on repetitive text
        let data = b"abracadabra abracadabra abracadabra abracadabra \
                     abracadabra abracadabra abracadabra abracadabra \
                     abracadabra abracadabra abracadabra abracadabra";
        let compressed = ppm_compress(data);
        let ratio = compressed.len() as f64 / data.len() as f64;
        eprintln!(
            "PPM repetitive text: {} -> {} ({:.1}%)",
            data.len(),
            compressed.len(),
            ratio * 100.0
        );
        // Should achieve some compression on repetitive data
        assert!(
            compressed.len() < data.len(),
            "PPM should compress repetitive text"
        );
    }
}
