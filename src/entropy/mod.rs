// NEXCOMP — rANS Entropy Coder (Stage 5)
// Based on: Duda, J. "Asymmetric Numeral Systems" arXiv:0902.0271 §3–4
// Implementation: streaming rANS with byte-level output, scale_bits=12
//
// Key property: |encoded| ≤ H(X) + ε, with ε → 0 as message length → ∞
// Decode throughput target: >2 GB/s single-thread (Ryzen 7 5800X)

use std::cmp::Reverse;
use thiserror::Error;

/// Scale bits for frequency table. M = 2^SCALE_BITS = 4096.
const SCALE_BITS: u32 = 12;
const SCALE_MASK: u32 = (1 << SCALE_BITS) - 1; // 4095
const TOTAL: u32 = 1 << SCALE_BITS;             // 4096

/// rANS state bounds: state ∈ [RANS_L, RANS_L * 256)
/// With b = 256 (byte output), L = M * 256 = 1_048_576
const RANS_L: u64 = (TOTAL as u64) << 8; // 1_048_576

#[derive(Error, Debug)]
pub enum RansError {
    #[error("Frequency table does not sum to 4096: got {0}")]
    InvalidFrequencySum(u32),
    #[error("Symbol {0} out of range (alphabet size {1})")]
    SymbolOutOfRange(usize, usize),
    #[error("Empty input")]
    EmptyInput,
    #[error("Decode buffer underflow")]
    DecodeUnderflow,
    #[error("Frequency for symbol {0} is zero — cannot encode")]
    ZeroFrequency(usize),
}

/// Precomputed encoding/decoding table for a single symbol.
/// Derived from frequency histogram via `build_table()`.
///
/// For symbol s:
///   freq[s] = number of quanta assigned (out of M = 4096)
///   cumul[s] = cumulative frequency of symbols 0..s-1
#[derive(Clone, Debug)]
pub struct RansTable {
    pub freq: Vec<u32>,
    pub cumul: Vec<u32>,
    pub alphabet_size: usize,
}

/// Decode lookup table for O(1) symbol identification.
/// spread[i] = symbol whose cumulative range contains i, for i in 0..M
#[derive(Clone, Debug)]
pub struct RansDecodeTable {
    pub spread: Vec<u8>,   // M entries: spread[x & SCALE_MASK] → symbol
    pub table: RansTable,
}

/// Build frequency/cumulative table from raw frequency counts.
///
/// Input: `freqs` — frequency count per symbol. Must sum to M = 4096.
/// If counts don't sum to M, caller must normalize first.
///
/// Complexity: O(alphabet_size) time, O(alphabet_size) space.
///
/// Reference: Duda 2009 §3, equation (3): C(s,x) = ⌊x/freq[s]⌋·M + cumul[s] + (x mod freq[s])
pub fn build_table(freqs: &[u32]) -> Result<RansTable, RansError> {
    let sum: u32 = freqs.iter().sum();
    if sum != TOTAL {
        return Err(RansError::InvalidFrequencySum(sum));
    }

    let alphabet_size = freqs.len();
    let mut cumul = Vec::with_capacity(alphabet_size + 1);
    cumul.push(0);
    for &f in freqs {
        let last = *cumul.last().expect("cumul is non-empty after push(0)");
        cumul.push(last + f);
    }

    Ok(RansTable {
        freq: freqs.to_vec(),
        cumul,
        alphabet_size,
    })
}

/// Build decode lookup table for O(1) symbol resolution.
///
/// spread[i] = s  where cumul[s] <= i < cumul[s+1]
///
/// Complexity: O(M) = O(4096) time and space — constant.
pub fn build_decode_table(table: &RansTable) -> RansDecodeTable {
    let mut spread = vec![0u8; TOTAL as usize];
    for s in 0..table.alphabet_size {
        let start = table.cumul[s] as usize;
        let end = table.cumul[s + 1] as usize;
        for i in start..end {
            spread[i] = s as u8;
        }
    }
    RansDecodeTable {
        spread,
        table: table.clone(),
    }
}

/// Normalize raw byte counts to sum to M = 4096.
///
/// Algorithm: proportional scaling with floor, then distribute remainder
/// to largest-frequency symbols to minimize distortion.
///
/// Both orderings break ties by symbol index, so the table is a function of
/// the counts alone. Sorting only by the fractional part would leave tied
/// symbols in whatever order the standard library's unstable sort happens to
/// produce, which differs between compiler versions — and a decoder that
/// rebuilds this table from the stored counts would then disagree with the
/// encoder that wrote them. See [`normalize_freqs_unpinned`] for the old
/// derivation, which stays only to read what earlier releases wrote.
///
/// Guarantees: every symbol with count > 0 gets freq >= 1.
/// Complexity: O(n log n) due to sorting for remainder distribution.
pub fn normalize_freqs(counts: &[u64], alphabet_size: usize) -> Vec<u32> {
    normalize_with(counts, alphabet_size, true)
}

/// The derivation before the tie order was pinned: only for payloads an
/// earlier release wrote. Its result depends on the standard library's sort.
pub fn normalize_freqs_unpinned(counts: &[u64], alphabet_size: usize) -> Vec<u32> {
    normalize_with(counts, alphabet_size, false)
}

fn normalize_with(counts: &[u64], alphabet_size: usize, pinned: bool) -> Vec<u32> {
    let total_count: u64 = counts.iter().sum();
    if total_count == 0 {
        // Uniform distribution fallback
        let base = TOTAL / alphabet_size as u32;
        let rem = TOTAL - base * alphabet_size as u32;
        let mut freqs = vec![base; alphabet_size];
        for i in 0..rem as usize {
            freqs[i] += 1;
        }
        return freqs;
    }

    let mut freqs = vec![0u32; alphabet_size];
    let mut assigned: u32 = 0;

    // Phase 1: proportional floor, minimum 1 for nonzero counts
    for i in 0..alphabet_size {
        if counts[i] > 0 {
            let f = ((counts[i] as u128 * TOTAL as u128) / total_count as u128) as u32;
            freqs[i] = f.max(1);
            assigned += freqs[i];
        }
    }

    // Phase 2: distribute or reclaim remainder
    let target = TOTAL;
    if assigned < target {
        let mut remainder = target - assigned;
        // Give remainder to symbols with largest fractional parts
        let mut fractional: Vec<(u64, usize)> = (0..alphabet_size)
            .filter(|&i| counts[i] > 0)
            .map(|i| {
                let exact = (counts[i] as u128 * TOTAL as u128) % total_count as u128;
                (exact as u64, i)
            })
            .collect();
        if pinned {
            fractional.sort_unstable_by_key(|&(frac, idx)| (Reverse(frac), idx));
        } else {
            fractional.sort_unstable_by_key(|&(frac, _)| Reverse(frac));
        }
        for &(_, idx) in &fractional {
            if remainder == 0 {
                break;
            }
            freqs[idx] += 1;
            remainder -= 1;
        }
    } else if assigned > target {
        let mut excess = assigned - target;
        // Reclaim from symbols with smallest fractional parts (and freq > 1)
        let mut fractional: Vec<(u64, usize)> = (0..alphabet_size)
            .filter(|&i| freqs[i] > 1)
            .map(|i| {
                let exact = (counts[i] as u128 * TOTAL as u128) % total_count as u128;
                (exact as u64, i)
            })
            .collect();
        if pinned {
            fractional.sort_unstable_by_key(|&(frac, idx)| (frac, idx));
        } else {
            fractional.sort_unstable_by_key(|a| a.0);
        }
        // May need multiple passes when single-pass can't reclaim enough
        while excess > 0 {
            let mut made_progress = false;
            for &(_, idx) in &fractional {
                if excess == 0 {
                    break;
                }
                if freqs[idx] > 1 {
                    freqs[idx] -= 1;
                    excess -= 1;
                    made_progress = true;
                }
            }
            if !made_progress {
                break; // all remaining symbols are at freq=1, cannot reclaim further
            }
        }
    }

    freqs
}

/// rANS encoder state.
///
/// Encodes symbols in reverse order (last symbol first).
/// Output bytes are emitted MSB-first into a buffer that is reversed at the end.
pub struct RansEncoder {
    state: u64,
    output: Vec<u8>,
}

impl Default for RansEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RansEncoder {
    pub fn new() -> Self {
        RansEncoder {
            state: RANS_L,
            output: Vec::new(),
        }
    }

    /// Encode a single symbol using rANS.
    ///
    /// Core formula (Duda 2009, eq. 3):
    ///   C(s, x) = ⌊x / freq[s]⌋ · M + cumul[s] + (x mod freq[s])
    ///
    /// Before encoding, we renormalize: while x >= freq[s] * (RANS_L / M) * 256,
    /// output a byte and shift state down.
    ///
    /// Invariant maintained: state ∈ [RANS_L, RANS_L * 256) after renormalization.
    pub fn encode_symbol(&mut self, table: &RansTable, symbol: usize) -> Result<(), RansError> {
        if symbol >= table.alphabet_size {
            return Err(RansError::SymbolOutOfRange(symbol, table.alphabet_size));
        }
        let freq = table.freq[symbol];
        if freq == 0 {
            return Err(RansError::ZeroFrequency(symbol));
        }
        let cumul = table.cumul[symbol];

        // Renormalize: ensure state < freq * (1 << (32 - SCALE_BITS))
        // This guarantees the encoded state stays in [RANS_L, RANS_L*256)
        let upper_bound = ((RANS_L >> SCALE_BITS) << 8) * freq as u64;
        while self.state >= upper_bound {
            self.output.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }

        // Core rANS encode step: C(s, x) = ⌊x/freq⌋·M + cumul + (x mod freq)
        self.state = (self.state / freq as u64) * TOTAL as u64
            + cumul as u64
            + (self.state % freq as u64);

        Ok(())
    }

    /// Finalize encoding: flush state bytes and reverse output.
    /// Returns the compressed byte stream.
    pub fn finish(mut self) -> Vec<u8> {
        // Flush final state (8 bytes for u64)
        for _ in 0..8 {
            self.output.push((self.state & 0xFF) as u8);
            self.state >>= 8;
        }
        self.output.reverse();
        self.output
    }
}

/// rANS decoder state.
pub struct RansDecoder<'a> {
    state: u64,
    input: &'a [u8],
    pos: usize,
}

impl<'a> RansDecoder<'a> {
    /// Initialize decoder from compressed byte stream.
    /// Reads initial 8-byte state from the front of the buffer.
    pub fn new(data: &'a [u8]) -> Result<Self, RansError> {
        if data.len() < 8 {
            return Err(RansError::DecodeUnderflow);
        }
        let mut state: u64 = 0;
        for i in 0..8 {
            state = (state << 8) | data[i] as u64;
        }
        Ok(RansDecoder {
            state,
            input: data,
            pos: 8,
        })
    }

    /// Decode a single symbol.
    ///
    /// 1. Extract slot = state & SCALE_MASK → O(1) symbol lookup via spread table
    /// 2. Advance state: x = freq[s] * (x >> SCALE_BITS) + slot - cumul[s]
    /// 3. Renormalize: while x < RANS_L, read byte and shift in
    ///
    /// Throughput: ~2 GB/s with SIMD-friendly memory access patterns.
    pub fn decode_symbol(&mut self, dtable: &RansDecodeTable) -> Result<u8, RansError> {
        let slot = (self.state & SCALE_MASK as u64) as u32;
        let symbol = dtable.spread[slot as usize];
        let s = symbol as usize;

        let freq = dtable.table.freq[s];
        let cumul = dtable.table.cumul[s];

        // Advance: x' = freq * (x >> scale_bits) + (x & scale_mask) - cumul
        self.state = freq as u64 * (self.state >> SCALE_BITS)
            + (self.state & SCALE_MASK as u64)
            - cumul as u64;

        // Renormalize: read bytes until state >= RANS_L
        while self.state < RANS_L {
            if self.pos >= self.input.len() {
                return Err(RansError::DecodeUnderflow);
            }
            self.state = (self.state << 8) | self.input[self.pos] as u64;
            self.pos += 1;
        }

        Ok(symbol)
    }
}

/// Convenience: encode a full message (slice of symbols) using the given table.
/// Symbols are encoded in reverse order as required by rANS.
pub fn rans_encode(symbols: &[u8], table: &RansTable) -> Result<Vec<u8>, RansError> {
    if symbols.is_empty() {
        return Err(RansError::EmptyInput);
    }
    let mut encoder = RansEncoder::new();
    // Encode in reverse order — rANS is a stack-based coder
    for &sym in symbols.iter().rev() {
        encoder.encode_symbol(table, sym as usize)?;
    }
    Ok(encoder.finish())
}

/// Convenience: decode `n_symbols` from compressed data.
pub fn rans_decode(
    data: &[u8],
    dtable: &RansDecodeTable,
    n_symbols: usize,
) -> Result<Vec<u8>, RansError> {
    let mut decoder = RansDecoder::new(data)?;
    let mut output = Vec::with_capacity(n_symbols);
    for _ in 0..n_symbols {
        output.push(decoder.decode_symbol(dtable)?);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_uniform() {
        // 4 symbols with equal probability: freq = [1024, 1024, 1024, 1024]
        let freqs = vec![1024u32; 4];
        let table = build_table(&freqs).expect("valid freq table");
        let dtable = build_decode_table(&table);

        let original: Vec<u8> = (0..10000).map(|i| (i % 4) as u8).collect();
        let encoded = rans_encode(&original, &table).expect("encode ok");
        let decoded = rans_decode(&encoded, &dtable, original.len()).expect("decode ok");

        assert_eq!(original, decoded, "Round-trip must be bit-perfect");
        // Theoretical: 2.0 bpb for uniform 4-symbol. Check within 5%.
        let bpb = (encoded.len() as f64 * 8.0) / original.len() as f64;
        assert!(bpb < 2.1, "bpb={bpb} should be near 2.0 for uniform 4-symbol");
    }

    #[test]
    fn test_roundtrip_skewed() {
        // Skewed distribution: symbol 0 dominates
        // freq[0]=3072, freq[1]=512, freq[2]=256, freq[3]=256  → sum=4096
        let freqs = vec![3072, 512, 256, 256];
        let table = build_table(&freqs).expect("valid freq table");
        let dtable = build_decode_table(&table);

        let original: Vec<u8> = (0..50000)
            .map(|i| {
                let r = i * 7 + 3; // deterministic pseudo-random
                if r % 8 < 6 { 0 }
                else if r % 8 < 7 { 1 }
                else if r % 16 < 9 { 2 }
                else { 3 }
            })
            .collect();

        let encoded = rans_encode(&original, &table).expect("encode ok");
        let decoded = rans_decode(&encoded, &dtable, original.len()).expect("decode ok");
        assert_eq!(original, decoded, "Skewed round-trip must be bit-perfect");
    }

    #[test]
    fn test_roundtrip_text() {
        // Realistic test: ASCII text with normalized byte frequencies
        let text = b"the quick brown fox jumps over the lazy dog. \
                     compression is the dual of prediction. \
                     rANS achieves optimal coding with table-based lookups.";
        let mut counts = vec![0u64; 256];
        for &b in text.iter() {
            counts[b as usize] += 1;
        }
        let freqs = normalize_freqs(&counts, 256);
        let table = build_table(&freqs).expect("normalized freqs must be valid");
        let dtable = build_decode_table(&table);

        let encoded = rans_encode(text, &table).expect("encode ok");
        let decoded = rans_decode(&encoded, &dtable, text.len()).expect("decode ok");
        assert_eq!(text.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_normalize_freqs() {
        let mut counts = vec![0u64; 256];
        counts[0] = 1000;
        counts[1] = 500;
        counts[2] = 250;
        counts[3] = 50;
        let freqs = normalize_freqs(&counts, 256);
        let sum: u32 = freqs.iter().sum();
        assert_eq!(sum, TOTAL, "Normalized frequencies must sum to M={TOTAL}");
        // All nonzero counts must have freq >= 1
        for i in 0..4 {
            assert!(freqs[i] >= 1, "Symbol {i} with count>0 must have freq>=1");
        }
    }

    /// The other side of the canary: payloads written before the order was
    /// pinned are read back through whatever the standard library's sort does,
    /// so a change there is a compatibility break for those files. This test
    /// is what says so out loud instead of leaving a fixture to fail.
    #[test]
    fn the_unpinned_derivation_still_matches_what_it_did() {
        let mut counts = vec![1u64; 256];
        for count in counts.iter_mut().skip(250) {
            *count = 2;
        }
        assert_eq!(
            normalize_freqs_unpinned(&counts, 256),
            normalize_freqs(&counts, 256),
            "the standard library's unstable sort changed its tie order: delta payloads written \
             before the flag existed (tests/formats/nx13/codec-delta.nxc) no longer decode the \
             way they were written, and normalize_freqs_unpinned has to reproduce the old order \
             explicitly instead of deferring to sort_unstable_by_key"
        );
    }

    /// The canary for the pinned tie order: 250 symbols share a fractional
    /// part and only 160 of them can take a unit of the remainder, so the
    /// table says which order the remainder was handed out in. A standard
    /// library whose unstable sort orders ties differently would show up here
    /// rather than in a file that no longer decodes.
    #[test]
    fn normalize_freqs_breaks_ties_by_symbol() {
        let mut counts = vec![1u64; 256];
        for count in counts.iter_mut().skip(250) {
            *count = 2;
        }
        let freqs = normalize_freqs(&counts, 256);
        assert_eq!(freqs.iter().sum::<u32>(), TOTAL);
        for (i, &f) in freqs.iter().enumerate() {
            let expected = match i {
                0..=159 => 16,
                160..=249 => 15,
                _ => 31,
            };
            assert_eq!(f, expected, "symbol {i}");
        }
    }
}
