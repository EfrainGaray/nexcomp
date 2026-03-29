// NEXCOMP — Fast Re-Pair Grammar Compression
//
// Optimized Re-Pair encoder using:
//   - Flat [u32; 65536] frequency array indexed by (a << 8 | b) for byte-level alphabets
//   - Linear scan over the flat array to find the max-frequency bigram (cache-friendly)
//   - Configurable minimum frequency threshold (default 4)
//   - Max 256 rules cap
//   - 1-second time limit
//   - Double-buffered Vec<u32> sequences
//
// This replaces repair_encode for the serialized LZ77 token stream where all
// symbols fit in a byte (alphabet size <= 256).

use crate::grammar::{RepairError, RepairResult, Rule};

const MAX_INPUT_SIZE: usize = 100 * 1024 * 1024;
const MAX_RULES: usize = 256;
const DEFAULT_MIN_FREQ: u32 = 4;

/// Optimized Re-Pair encoder for byte-level input streams.
///
/// Uses a flat frequency array of 65536 entries instead of a HashMap,
/// and a linear scan instead of a heap to find the most frequent bigram.
/// Only considers bigrams with frequency >= `min_freq` (default 4).
/// Stops after 256 rules or 1 second, whichever comes first.
pub fn repair_encode_fast(input: &[u8]) -> Result<RepairResult, RepairError> {
    repair_encode_fast_with_min_freq(input, DEFAULT_MIN_FREQ)
}

/// Same as `repair_encode_fast` but with a configurable minimum frequency threshold.
pub fn repair_encode_fast_with_min_freq(
    input: &[u8],
    min_freq: u32,
) -> Result<RepairResult, RepairError> {
    if input.is_empty() {
        return Err(RepairError::EmptyInput);
    }
    if input.len() > MAX_INPUT_SIZE {
        return Err(RepairError::InputTooLarge(input.len()));
    }

    let original_len = input.len();
    let mut rules: Vec<Rule> = Vec::new();
    let mut next_symbol: u32 = 256;

    let start_time = std::time::Instant::now();
    let time_limit = std::time::Duration::from_secs(1);

    // Double-buffered sequences
    let mut seq_a: Vec<u32> = input.iter().map(|&b| b as u32).collect();
    let mut seq_b: Vec<u32> = Vec::with_capacity(input.len());
    let mut use_a = true;

    // Flat frequency array: index = (left << 8) | right, for symbols 0..255.
    // Once we introduce non-terminals (>= 256), bigrams involving them can't be
    // tracked in this array. We handle this by only tracking byte-byte pairs
    // in the flat array and using a small HashMap overflow for pairs involving
    // non-terminals.
    //
    // However, since we cap at 256 rules, non-terminal IDs go 256..511, and
    // the total alphabet stays <= 512. We can use a larger flat array or
    // just skip non-terminal pairs (they're rarer). For simplicity and speed,
    // we use a HashMap for the general case but only when symbols exceed 255.
    //
    // Actually, the cleanest approach: since max symbols = 256 (terminals) + 256
    // (rules) = 512, we need 512*512 = 262144 entries. That's 1MB — still fine.
    const EXTENDED_SIZE: usize = 512 * 512; // 262144
    let mut freq = vec![0u32; EXTENDED_SIZE];

    // Build initial frequencies (all symbols are < 256 at this point)
    {
        let seq = &seq_a;
        for i in 0..seq.len().saturating_sub(1) {
            let idx = (seq[i] as usize) << 9 | (seq[i + 1] as usize);
            if idx < EXTENDED_SIZE {
                freq[idx] += 1;
            }
        }
    }

    // Effective min_freq: at least 2 to be useful
    let min_freq = min_freq.max(2);

    loop {
        // Check limits
        if rules.len() >= MAX_RULES || start_time.elapsed() > time_limit {
            break;
        }

        // Linear scan to find max frequency bigram
        let mut best_freq: u32 = 0;
        let mut best_idx: usize = 0;
        for i in 0..EXTENDED_SIZE {
            if freq[i] > best_freq {
                best_freq = freq[i];
                best_idx = i;
            }
        }

        if best_freq < min_freq {
            break;
        }

        let best_left = (best_idx >> 9) as u32;
        let best_right = (best_idx & 0x1FF) as u32;

        // Create new rule
        rules.push(Rule {
            left: best_left,
            right: best_right,
        });
        let new_sym = next_symbol;
        next_symbol += 1;

        // Replace all occurrences using double buffering
        let (src, dst) = if use_a {
            (&seq_a, &mut seq_b)
        } else {
            (&seq_b, &mut seq_a)
        };
        dst.clear();

        let src_len = src.len();
        let mut i = 0;
        while i < src_len {
            if i + 1 < src_len && src[i] == best_left && src[i + 1] == best_right {
                dst.push(new_sym);
                i += 2;
            } else {
                dst.push(src[i]);
                i += 1;
            }
        }

        // Rebuild frequency table from new sequence
        // Clear the table
        for v in freq.iter_mut() {
            *v = 0;
        }

        let dst_len = dst.len();
        for i in 0..dst_len.saturating_sub(1) {
            let a = dst[i] as usize;
            let b = dst[i + 1] as usize;
            // Only track if both symbols fit in 9 bits (0..511)
            if a < 512 && b < 512 {
                freq[a << 9 | b] += 1;
            }
        }

        use_a = !use_a;
    }

    let seq = if use_a { seq_a } else { seq_b };
    Ok(RepairResult {
        rules,
        sequence: seq,
        original_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::repair_decode;

    #[test]
    fn test_fast_roundtrip_random() {
        // 64KB of pseudo-random data
        let mut data = vec![0u8; 64 * 1024];
        let mut state: u64 = 0xDEAD_BEEF_CAFE_BABE;
        for byte in data.iter_mut() {
            // xorshift64
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }

        let result = repair_encode_fast(&data).expect("encode ok");
        let decoded = repair_decode(&result);
        assert_eq!(
            data, decoded,
            "64KB random data round-trip must be lossless"
        );
    }

    #[test]
    fn test_fast_roundtrip_repetitive() {
        let input: Vec<u8> = "abcdefgh".repeat(10_000).into_bytes();
        let result = repair_encode_fast(&input).expect("encode ok");
        let decoded = repair_decode(&result);
        assert_eq!(input, decoded, "Repetitive data round-trip must be lossless");
        // Must actually compress
        assert!(
            result.sequence.len() < input.len(),
            "Repetitive data must compress: seq {} < original {}",
            result.sequence.len(),
            input.len()
        );
        assert!(
            !result.rules.is_empty(),
            "Should create at least one grammar rule"
        );
    }

    #[test]
    fn test_fast_roundtrip_1mb() {
        // 1MB of semi-repetitive data
        let pattern = b"the quick brown fox jumps over the lazy dog and ";
        let input: Vec<u8> = pattern.iter().copied().cycle().take(1024 * 1024).collect();

        let start = std::time::Instant::now();
        let result = repair_encode_fast(&input).expect("encode ok");
        let elapsed = start.elapsed();

        eprintln!(
            "Fast 1MB encode: {:.3}s, {} rules, sequence len {}",
            elapsed.as_secs_f64(),
            result.rules.len(),
            result.sequence.len()
        );

        let time_limit = if cfg!(debug_assertions) { 3.0 } else { 1.0 };

        // Keep the performance guard meaningful without making debug/test runs flaky.
        assert!(
            elapsed.as_secs_f64() < time_limit,
            "1MB encode took too long: {:.3}s (limit {:.1}s)",
            elapsed.as_secs_f64(),
            time_limit,
        );

        let decoded = repair_decode(&result);
        assert_eq!(input, decoded, "1MB round-trip must be lossless");
    }

    #[test]
    fn test_fast_matches_decode() {
        // Encode with fast encoder, decode with standard decoder — must be compatible
        let input = b"abracadabra abracadabra abracadabra abracadabra ";
        let input_vec: Vec<u8> = input.repeat(100);

        let result = repair_encode_fast(&input_vec).expect("encode ok");

        // Verify the RepairResult structure is valid for standard decode
        assert_eq!(result.original_len, input_vec.len());
        for rule in &result.rules {
            assert!(
                rule.left < 256 + result.rules.len() as u32,
                "Rule left symbol out of range"
            );
            assert!(
                rule.right < 256 + result.rules.len() as u32,
                "Rule right symbol out of range"
            );
        }

        // Standard decode must produce original input
        let decoded = repair_decode(&result);
        assert_eq!(
            input_vec, decoded,
            "Fast encode -> standard decode must be lossless"
        );
    }
}
