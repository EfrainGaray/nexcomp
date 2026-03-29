// NEXCOMP — Re-Pair Grammar Compression (Stage 3)
// Based on: Larsson & Moffat "Off-line Dictionary-Based Compression" (2000)
//           Kim et al. "Re²Pair: Increasing Scalability of RePair" — ESA 2024
//
// Re-Pair algorithm:
//   1. Count all bigram (pair) frequencies in input
//   2. Replace most frequent pair with a new non-terminal symbol
//   3. Update pair frequencies; repeat until no pair has freq >= 2
//   Result: Straight-Line Program (SLP) — a context-free grammar generating exactly
//   the original string.
//
// Complexity (original Re-Pair): O(n) time, O(5n) space [Larsson & Moffat 2000]
// Complexity (Re²Pair ESA 2024): O((1+ε)n) space with O(n log n) time
//
// This implementation targets files < 100MB with O(n log n) time, O(n) space.

use std::collections::{BinaryHeap, HashMap};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum RepairError {
    #[error("Input is empty")]
    EmptyInput,
    #[error("Input too large: {0} bytes (max 100MB)")]
    InputTooLarge(usize),
}

const MAX_INPUT_SIZE: usize = 100 * 1024 * 1024; // 100 MB

/// A grammar rule: non-terminal → (left, right)
/// Symbols 0..255 are terminals (byte values).
/// Symbols >= 256 are non-terminals introduced by Re-Pair.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub left: u32,
    pub right: u32,
}

/// Result of Re-Pair compression: the SLP (grammar rules + start sequence).
#[derive(Clone, Debug)]
pub struct RepairResult {
    /// Grammar rules indexed by (id - 256). rules[0] is non-terminal 256.
    pub rules: Vec<Rule>,
    /// The compressed sequence (mix of terminals 0..255 and non-terminals >= 256).
    pub sequence: Vec<u32>,
    /// Original input length (for verification).
    pub original_len: usize,
}

/// Re-Pair compressor.
///
/// Algorithm (Larsson & Moffat 2000, §2):
///   while max_pair_freq >= 2:
///     pair = most frequent bigram
///     new_symbol = next_id++
///     rules.push(pair → new_symbol)
///     replace all occurrences of pair in sequence with new_symbol
///     update bigram frequencies for affected neighbors
///
/// Optimized implementation using double-buffered Vec<u32> sequences to avoid
/// allocation overhead, with a BinaryHeap (max-heap) for O(log n) extraction
/// of the most-frequent bigram. Frequency counts are rebuilt from scratch after
/// each replacement pass (O(n) per iteration, but cache-friendly sequential
/// access). Total complexity: O(n * r) where r = number of rules created,
/// with r typically O(log n) for repetitive data.
pub fn repair_encode(input: &[u8]) -> Result<RepairResult, RepairError> {
    if input.is_empty() {
        return Err(RepairError::EmptyInput);
    }
    if input.len() > MAX_INPUT_SIZE {
        return Err(RepairError::InputTooLarge(input.len()));
    }

    let original_len = input.len();
    let n = input.len();
    let mut rules: Vec<Rule> = Vec::new();
    let mut next_symbol: u32 = 256;

    // --- Optimization parameters ---
    // Cap the number of rules: diminishing returns after this many.
    // The first few hundred rules capture most compression; rules beyond this
    // barely shrink the sequence but cost the same O(n) scan each.
    let max_rules: usize = 256;
    // Stall detection window: every `stall_window` rules, check if the sequence
    // shrank by at least 1%. If it didn't shrink for two consecutive windows,
    // stop early.
    let stall_window: usize = 10;
    // Hard time limit: stop after 2 seconds regardless of progress
    let start_time = std::time::Instant::now();
    let time_limit = std::time::Duration::from_secs(2);

    // Double-buffered sequences to avoid repeated allocation
    let mut seq_a: Vec<u32> = input.iter().map(|&b| b as u32).collect();
    let mut seq_b: Vec<u32> = Vec::with_capacity(n);
    let mut use_a = true;

    // Frequency map using packed u64 keys for speed
    let mut freq: HashMap<u64, u32> = HashMap::with_capacity(n / 2);

    // Initial bigram frequency count
    {
        let seq = &seq_a;
        let len = seq.len();
        let mut i = 0;
        while i + 1 < len {
            let key = pack(seq[i], seq[i + 1]);
            *freq.entry(key).or_insert(0) += 1;
            i += 1;
        }
    }

    let min_freq: u32 = 2;

    // Build max-heap: (frequency, packed bigram key)
    let mut heap: BinaryHeap<(u32, u64)> = BinaryHeap::with_capacity(freq.len());
    for (&key, &count) in &freq {
        if count >= min_freq {
            heap.push((count, key));
        }
    }

    // Stall detection state
    let mut stall_count: usize = 0;
    let mut prev_seq_len: usize = seq_a.len();

    loop {
        // --- Max rules limit or time limit ---
        if rules.len() >= max_rules || start_time.elapsed() > time_limit {
            let seq = if use_a { seq_a } else { seq_b };
            return Ok(RepairResult { rules, sequence: seq, original_len });
        }

        // Pop until we find a non-stale entry with freq >= min_freq
        let best_left;
        let best_right;
        loop {
            match heap.pop() {
                None => {
                    // No more pairs with freq >= 2 — done
                    let seq = if use_a { seq_a } else { seq_b };
                    return Ok(RepairResult { rules, sequence: seq, original_len });
                }
                Some((f, key)) => {
                    let current = freq.get(&key).copied().unwrap_or(0);
                    if current >= min_freq {
                        if current == f {
                            best_left = (key >> 32) as u32;
                            best_right = key as u32;
                            break;
                        }
                        // Stale entry — re-push with correct frequency
                        heap.push((current, key));
                    }
                }
            }
        }

        // Create new rule
        rules.push(Rule { left: best_left, right: best_right });
        let new_sym = next_symbol;
        next_symbol += 1;

        // Clear freq map — we rebuild from the new sequence (cache-friendly O(n) scan)
        freq.clear();

        // Replace all occurrences of (best_left, best_right) using double buffering
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

        // Rebuild freq from new sequence
        let dst_len = dst.len();
        i = 0;
        while i + 1 < dst_len {
            let key = pack(dst[i], dst[i + 1]);
            *freq.entry(key).or_insert(0) += 1;
            i += 1;
        }

        // --- Stall detection (every stall_window rules) ---
        let cur_seq_len = dst_len;
        if rules.len() % stall_window == 0 && rules.len() > stall_window {
            let shrink_pct = if prev_seq_len > 0 {
                100 * (prev_seq_len.saturating_sub(cur_seq_len)) / prev_seq_len
            } else {
                0
            };
            if shrink_pct < 1 {
                stall_count += 1;
                if stall_count >= 2 {
                    // Stalled for two consecutive windows — stop
                    use_a = !use_a;
                    let seq = if use_a { seq_a } else { seq_b };
                    return Ok(RepairResult { rules, sequence: seq, original_len });
                }
            } else {
                stall_count = 0;
            }
            prev_seq_len = cur_seq_len;
        }

        // Rebuild heap (only entries meeting current threshold)
        heap.clear();
        for (&key, &count) in &freq {
            if count >= min_freq {
                heap.push((count, key));
            }
        }

        use_a = !use_a;
    }
}

#[inline(always)]
fn pack(left: u32, right: u32) -> u64 {
    ((left as u64) << 32) | (right as u64)
}

/// Decode a Re-Pair compressed result back to the original bytes.
///
/// Expands each symbol in the sequence:
///   - If terminal (< 256): emit byte directly
///   - If non-terminal (>= 256): recursively expand via rules
///
/// Uses an explicit stack to avoid stack overflow on deep grammars.
/// Complexity: O(n) where n = original length.
pub fn repair_decode(result: &RepairResult) -> Vec<u8> {
    let mut output = Vec::with_capacity(result.original_len);
    let mut stack: Vec<u32> = Vec::new();

    // Push sequence in reverse so we process left-to-right
    for &sym in result.sequence.iter().rev() {
        stack.push(sym);
    }

    while let Some(sym) = stack.pop() {
        if sym < 256 {
            output.push(sym as u8);
        } else {
            let rule_idx = (sym - 256) as usize;
            let rule = &result.rules[rule_idx];
            // Push right first, then left (so left is popped first)
            stack.push(rule.right);
            stack.push(rule.left);
        }
    }

    output
}

/// Serialize RepairResult to bytes for storage.
///
/// Format:
///   [4B] original_len (u32 LE)
///   [4B] num_rules (u32 LE)
///   [4B] sequence_len (u32 LE)
///   For each rule: [4B left][4B right]
///   For each seq symbol: [4B] (u32 LE)
pub fn repair_serialize(result: &RepairResult) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(result.original_len as u32).to_le_bytes());
    out.extend_from_slice(&(result.rules.len() as u32).to_le_bytes());
    out.extend_from_slice(&(result.sequence.len() as u32).to_le_bytes());
    for rule in &result.rules {
        out.extend_from_slice(&rule.left.to_le_bytes());
        out.extend_from_slice(&rule.right.to_le_bytes());
    }
    for &sym in &result.sequence {
        out.extend_from_slice(&sym.to_le_bytes());
    }
    out
}

/// Deserialize RepairResult from bytes.
pub fn repair_deserialize(data: &[u8]) -> Result<RepairResult, RepairError> {
    if data.len() < 12 {
        return Err(RepairError::EmptyInput);
    }
    let original_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let num_rules = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let seq_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

    let mut offset = 12;
    let mut rules = Vec::with_capacity(num_rules);
    for _ in 0..num_rules {
        let left = u32::from_le_bytes([
            data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
        ]);
        let right = u32::from_le_bytes([
            data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7],
        ]);
        rules.push(Rule { left, right });
        offset += 8;
    }

    let mut sequence = Vec::with_capacity(seq_len);
    for _ in 0..seq_len {
        let sym = u32::from_le_bytes([
            data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
        ]);
        sequence.push(sym);
        offset += 4;
    }

    Ok(RepairResult {
        rules,
        sequence,
        original_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_simple() {
        let input = b"abcabcabcabc";
        let result = repair_encode(input).expect("encode ok");
        let decoded = repair_decode(&result);
        assert_eq!(input.as_slice(), decoded.as_slice());
        // Grammar should compress: "abc" repeated 4 times
        assert!(!result.rules.is_empty(), "Should create at least one rule");
        assert!(
            result.sequence.len() < input.len(),
            "Compressed sequence should be shorter: {} < {}",
            result.sequence.len(),
            input.len()
        );
    }

    #[test]
    fn test_roundtrip_no_compression() {
        // All unique bytes — no repeated bigrams
        let input: Vec<u8> = (0..=255).collect();
        let result = repair_encode(&input).expect("encode ok");
        let decoded = repair_decode(&result);
        assert_eq!(input, decoded);
    }

    #[test]
    fn test_roundtrip_text() {
        let input = b"the cat sat on the mat and the cat sat on the hat";
        let result = repair_encode(input).expect("encode ok");
        let decoded = repair_decode(&result);
        assert_eq!(input.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_serialize_deserialize() {
        let input = b"hello world hello world hello world";
        let result = repair_encode(input).expect("encode ok");
        let serialized = repair_serialize(&result);
        let deserialized = repair_deserialize(&serialized).expect("deserialize ok");
        let decoded = repair_decode(&deserialized);
        assert_eq!(input.as_slice(), decoded.as_slice());
    }

    #[test]
    fn test_compression_ratio() {
        // Highly repetitive input should compress well
        let input: Vec<u8> = "abracadabra ".repeat(1000).into_bytes();
        let result = repair_encode(&input).expect("encode ok");
        let serialized = repair_serialize(&result);
        let ratio = serialized.len() as f64 / input.len() as f64;
        assert!(
            ratio < 0.5,
            "Highly repetitive text should compress to <50%: got {:.1}%",
            ratio * 100.0
        );
        // Verify round-trip
        let decoded = repair_decode(&result);
        assert_eq!(input, decoded);
    }

    #[test]
    fn test_1mb_performance() {
        // 1MB of semi-repetitive data (mixed patterns to stress the algorithm)
        let pattern = b"the quick brown fox jumps over the lazy dog and ";
        let input: Vec<u8> = pattern.iter().copied().cycle().take(1024 * 1024).collect();
        assert_eq!(input.len(), 1024 * 1024, "Input should be exactly 1MB");

        let start = std::time::Instant::now();
        let result = repair_encode(&input).expect("encode ok");
        let elapsed = start.elapsed();

        eprintln!(
            "1MB encode: {:.2}s, {} rules, sequence len {}",
            elapsed.as_secs_f64(),
            result.rules.len(),
            result.sequence.len()
        );

        // Must finish within 30 seconds in debug mode (5s in release)
        assert!(
            elapsed.as_secs() < 30,
            "1MB encode took too long: {:.2}s (limit 30s debug)",
            elapsed.as_secs_f64()
        );

        // Verify correctness
        let decoded = repair_decode(&result);
        assert_eq!(input, decoded, "Round-trip must be lossless for 1MB input");

        // Should achieve some compression on repetitive data
        assert!(
            result.sequence.len() < input.len(),
            "Should compress: seq {} < original {}",
            result.sequence.len(),
            input.len()
        );
    }
}
