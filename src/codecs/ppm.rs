//! PPM (Prediction by Partial Matching) order-5 codec.
//!
//! Models P(byte | last N bytes) for N = 0..5, escaping to lower orders
//! when a context has not been seen. Uses Method D escape estimation with
//! exclusion for efficiency.

use crate::range_coder::{RangeEncoder, RangeDecoder};
use std::collections::HashMap;

const MAX_ORDER: usize = 5;

// ---------------------------------------------------------------------------
// Frequency table for a single context
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FreqTable {
    counts: [u16; 256],
    total: u32,
    num_symbols: u16, // number of distinct symbols seen
}

impl FreqTable {
    fn new() -> Self {
        Self {
            counts: [0; 256],
            total: 0,
            num_symbols: 0,
        }
    }

    fn update(&mut self, byte: u8) {
        if self.counts[byte as usize] == 0 {
            self.num_symbols += 1;
        }
        self.counts[byte as usize] += 1;
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
        self.num_symbols = 0;
        for c in self.counts.iter_mut() {
            *c = (*c + 1) / 2; // halve with rounding up
            if *c > 0 {
                self.num_symbols += 1;
            }
            self.total += *c as u32;
        }
    }
}

// ---------------------------------------------------------------------------
// PPM model using HashMap for context storage
// ---------------------------------------------------------------------------

/// PPM context model. Contexts are stored as byte sequences of length 0..=MAX_ORDER
/// mapping to frequency tables.
struct PpmModel {
    /// Map from context bytes -> frequency table.
    /// Empty slice = order 0, slice of length k = order k context.
    contexts: HashMap<Vec<u8>, FreqTable>,
}

impl PpmModel {
    fn new() -> Self {
        Self {
            contexts: HashMap::new(),
        }
    }

    /// Get or create the frequency table for a given context.
    fn get_or_create(&mut self, ctx: &[u8]) -> &mut FreqTable {
        self.contexts.entry(ctx.to_vec()).or_insert_with(FreqTable::new)
    }

    /// Get the frequency table for a given context (read-only).
    fn get(&self, ctx: &[u8]) -> Option<&FreqTable> {
        self.contexts.get(ctx)
    }

    /// Update model with observed byte at given context.
    /// Updates all orders from 0 to min(MAX_ORDER, ctx.len()).
    fn update(&mut self, full_ctx: &[u8], byte: u8) {
        let max_ord = full_ctx.len().min(MAX_ORDER);
        for order in 0..=max_ord {
            let start = full_ctx.len() - order;
            let ctx = &full_ctx[start..];
            self.get_or_create(ctx).update(byte);
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
    full_ctx: &[u8],
    byte: u8,
) {
    let mut excluded = [false; 256];
    let max_ord = full_ctx.len().min(MAX_ORDER);

    // Try from highest order down to 0
    for order in (0..=max_ord).rev() {
        let start = full_ctx.len() - order;
        let ctx = &full_ctx[start..];

        if let Some(freq) = model.get(ctx) {
            // Compute total excluding already-excluded symbols, and the number
            // of distinct non-excluded symbols (used as the escape weight).
            let mut total_excl = 0u32;
            let mut num_distinct = 0u32;

            for b in 0..256usize {
                if excluded[b] {
                    continue;
                }
                if freq.counts[b] > 0 {
                    total_excl += freq.counts[b] as u32;
                    num_distinct += 1;
                }
            }

            // If no non-excluded symbols in this context, skip to lower order
            if total_excl == 0 {
                continue;
            }

            let byte_count = if excluded[byte as usize] {
                0
            } else {
                freq.counts[byte as usize] as u32
            };

            // Escape weight = num_distinct (Method D)
            let esc_weight = num_distinct;
            let denom = total_excl + esc_weight;

            if byte_count > 0 {
                // Encode the byte (not an escape)
                let mut cum_low = 0u32;
                for b in 0..byte as usize {
                    if excluded[b] {
                        continue;
                    }
                    if freq.counts[b] > 0 {
                        cum_low += freq.counts[b] as u32;
                    }
                }
                enc.encode_freq(cum_low, byte_count, denom);
                return;
            }

            // Byte not found in this context -- encode escape
            enc.encode_freq(total_excl, esc_weight, denom);

            // Mark all symbols seen in this context as excluded
            for b in 0..256 {
                if freq.counts[b] > 0 {
                    excluded[b] = true;
                }
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
    full_ctx: &[u8],
) -> u8 {
    let mut excluded = [false; 256];
    let max_ord = full_ctx.len().min(MAX_ORDER);

    // Try from highest order down to 0
    for order in (0..=max_ord).rev() {
        let start = full_ctx.len() - order;
        let ctx = &full_ctx[start..];

        if let Some(freq) = model.get(ctx) {
            let mut total_excl = 0u32;
            let mut num_distinct = 0u32;

            for b in 0..256usize {
                if excluded[b] {
                    continue;
                }
                if freq.counts[b] > 0 {
                    total_excl += freq.counts[b] as u32;
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
                for b in 0..256usize {
                    if excluded[b] {
                        continue;
                    }
                    let c = freq.counts[b] as u32;
                    if c > 0 {
                        if cum + c > target {
                            // Found the symbol
                            dec.decode_freq(cum, c, denom);
                            return b as u8;
                        }
                        cum += c;
                    }
                }
                // Should not reach here
                unreachable!("decode_byte_ppm: symbol not found in CDF");
            } else {
                // Escape
                dec.decode_freq(total_excl, esc_weight, denom);

                // Mark all symbols seen in this context as excluded
                for b in 0..256 {
                    if freq.counts[b] > 0 {
                        excluded[b] = true;
                    }
                }
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
    let mut ctx: Vec<u8> = Vec::with_capacity(MAX_ORDER + 1);

    // Write original length as header
    let mut header = (data.len() as u32).to_le_bytes().to_vec();

    for &byte in data {
        encode_byte_ppm(&mut enc, &model, &ctx, byte);
        model.update(&ctx, byte);
        ctx.push(byte);
        if ctx.len() > MAX_ORDER {
            ctx.remove(0);
        }
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
    let mut ctx: Vec<u8> = Vec::with_capacity(MAX_ORDER + 1);
    let mut output = Vec::with_capacity(orig_len);

    for _ in 0..orig_len {
        let byte = decode_byte_ppm(&mut dec, &model, &ctx);
        output.push(byte);
        model.update(&ctx, byte);
        ctx.push(byte);
        if ctx.len() > MAX_ORDER {
            ctx.remove(0);
        }
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
