//! Pragmatic LZMA-style codec used as an adaptive candidate.
//!
//! This codec is intentionally scoped for the selector:
//! - lossless and self-contained
//! - uses the binary range coder, LZMA state machine and literal coder
//! - LZMA-style slot-based distance coding with context-modelled extra bits
//! - three-range length coder with probability trees throughout

use crate::literal_coder::LiteralCoder;
use crate::lz77::{Lz77Encoder, Token};
use crate::lzma_state::{LzmaState, StateProbs, NUM_POS_STATES};
use crate::range_coder::{Prob, RangeDecoder, RangeEncoder, PROB_INIT};

// ---------------------------------------------------------------------------
// Distance slot helpers (LZMA specification)
// ---------------------------------------------------------------------------

const NUM_DIST_SLOTS: usize = 64;
const NUM_LEN_TO_POS_STATES: usize = 4;
const NUM_ALIGN_BITS: u32 = 4;
const ALIGN_TABLE_SIZE: usize = 1 << NUM_ALIGN_BITS; // 16
const END_POS_MODEL_INDEX: usize = 14;
const START_POS_MODEL_INDEX: usize = 4;
// Total context-coded reverse-bit-tree probs for slots 4..13
// Each slot s (4..14) has (1 << ((s>>1)-1)) probs stored contiguously.
// We index by: base[s] + tree_index.  Total entries = sum of those sizes.
// Slot 4,5 => 2 probs each; 6,7 => 4; 8,9 => 8; 10,11 => 16; 12,13 => 32
// Total = 2*2 + 2*4 + 2*8 + 2*16 + 2*32 = 4+8+16+32+64 = 124
const NUM_SPEC_PROBS: usize = 128; // rounded up for simplicity

/// Convert a 0-based distance to a distance slot.
fn dist_slot(dist: u32) -> u32 {
    if dist < 4 {
        return dist;
    }
    let bsr = 31 - dist.leading_zeros(); // floor(log2(dist))
    ((bsr as u32) << 1) + ((dist >> (bsr - 1)) & 1)
}

/// Convert a distance slot back to the base distance and number of extra bits.
fn slot_base_and_bits(slot: u32) -> (u32, u32) {
    if slot < 4 {
        return (slot, 0);
    }
    let num_extra = (slot >> 1) - 1;
    let base = (2 | (slot & 1)) << num_extra;
    (base, num_extra)
}

// ---------------------------------------------------------------------------
// Distance coder
// ---------------------------------------------------------------------------

struct DistanceCoder {
    // Slot tree probs, per len_state (4 len states x 64-entry trees)
    slot_probs: [[Prob; NUM_DIST_SLOTS]; NUM_LEN_TO_POS_STATES],
    // Context-coded reverse bit trees for slots 4..13
    spec_probs: [Prob; NUM_SPEC_PROBS],
    // Alignment reverse bit tree (4 bits) for slots >= 14
    align_probs: [Prob; ALIGN_TABLE_SIZE],
}

impl DistanceCoder {
    fn new() -> Self {
        Self {
            slot_probs: [[PROB_INIT; NUM_DIST_SLOTS]; NUM_LEN_TO_POS_STATES],
            spec_probs: [PROB_INIT; NUM_SPEC_PROBS],
            align_probs: [PROB_INIT; ALIGN_TABLE_SIZE],
        }
    }

    fn len_state(len: usize) -> usize {
        (len.saturating_sub(2)).min(NUM_LEN_TO_POS_STATES - 1)
    }

    fn encode_tree6(enc: &mut RangeEncoder, probs: &mut [Prob; NUM_DIST_SLOTS], value: u32) {
        let mut sym = 1u32;
        for bit_idx in (0..6).rev() {
            let bit = (value >> bit_idx) & 1;
            enc.encode_bit(&mut probs[sym as usize], bit);
            sym = (sym << 1) | bit;
        }
    }

    fn decode_tree6(dec: &mut RangeDecoder, probs: &mut [Prob; NUM_DIST_SLOTS]) -> u32 {
        let mut sym = 1u32;
        for _ in 0..6 {
            let bit = dec.decode_bit(&mut probs[sym as usize]);
            sym = (sym << 1) | bit;
        }
        sym - 64
    }

    /// Offset into spec_probs for a given slot (4..14).
    fn spec_offset(slot: u32) -> usize {
        // Slots 4,5 start at 0 (2 entries each)
        // Slots 6,7 start at 4 (4 entries each)
        // etc.
        let mut off = 0usize;
        let mut s = START_POS_MODEL_INDEX as u32;
        while s < slot {
            let num_bits = (s >> 1) - 1;
            off += 1 << num_bits;
            s += 1;
        }
        off
    }

    fn encode_reverse_bits(
        enc: &mut RangeEncoder,
        probs: &mut [Prob],
        base: usize,
        num_bits: u32,
        value: u32,
    ) {
        let mut sym = 1u32;
        for i in 0..num_bits {
            let bit = (value >> i) & 1;
            enc.encode_bit(&mut probs[base + sym as usize], bit);
            sym = (sym << 1) | bit;
        }
    }

    fn decode_reverse_bits(
        dec: &mut RangeDecoder,
        probs: &mut [Prob],
        base: usize,
        num_bits: u32,
    ) -> u32 {
        let mut sym = 1u32;
        let mut result = 0u32;
        for i in 0..num_bits {
            let bit = dec.decode_bit(&mut probs[base + sym as usize]);
            sym = (sym << 1) | bit;
            result |= bit << i;
        }
        result
    }

    /// Encode a 1-based distance (offset).
    fn encode(&mut self, enc: &mut RangeEncoder, dist: u32, len: usize) {
        let ls = Self::len_state(len);
        let dist0 = dist - 1; // convert to 0-based
        let slot = dist_slot(dist0);
        Self::encode_tree6(enc, &mut self.slot_probs[ls], slot);

        if slot >= START_POS_MODEL_INDEX as u32 {
            let (base, num_extra) = slot_base_and_bits(slot);
            let extra = dist0 - base;

            if slot < END_POS_MODEL_INDEX as u32 {
                // Context-coded reverse bit tree
                let off = Self::spec_offset(slot);
                Self::encode_reverse_bits(
                    enc,
                    &mut self.spec_probs,
                    off,
                    num_extra,
                    extra,
                );
            } else {
                // Direct bits (high part) + align bits (low 4 bits)
                let direct_bits = num_extra - NUM_ALIGN_BITS;
                enc.encode_direct_bits(extra >> NUM_ALIGN_BITS, direct_bits);
                Self::encode_reverse_bits(
                    enc,
                    &mut self.align_probs,
                    0,
                    NUM_ALIGN_BITS,
                    extra & (ALIGN_TABLE_SIZE as u32 - 1),
                );
            }
        }
        // slots 0-3: distance encoded entirely by slot, no extra bits needed
    }

    /// Decode and return a 1-based distance.
    fn decode(&mut self, dec: &mut RangeDecoder, len: usize) -> u32 {
        let ls = Self::len_state(len);
        let slot = Self::decode_tree6(dec, &mut self.slot_probs[ls]);

        if slot < START_POS_MODEL_INDEX as u32 {
            return slot + 1; // 0-based slot IS the distance for slots 0-3
        }

        let (base, num_extra) = slot_base_and_bits(slot);
        let extra = if slot < END_POS_MODEL_INDEX as u32 {
            let off = Self::spec_offset(slot);
            Self::decode_reverse_bits(dec, &mut self.spec_probs, off, num_extra)
        } else {
            let direct_bits = num_extra - NUM_ALIGN_BITS;
            let high = dec.decode_direct_bits(direct_bits);
            let low = Self::decode_reverse_bits(dec, &mut self.align_probs, 0, NUM_ALIGN_BITS);
            (high << NUM_ALIGN_BITS) | low
        };

        base + extra + 1 // convert back to 1-based
    }
}

// ---------------------------------------------------------------------------
// Length coder (three ranges, all probability-tree coded)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LzmaStyleError {
    InvalidPayload(&'static str),
    InvalidMatch { offset: usize, available: usize },
}

struct LengthCoder {
    choice: [Prob; NUM_POS_STATES],
    choice2: [Prob; NUM_POS_STATES],
    low: [[Prob; 8]; NUM_POS_STATES],   // 3-bit trees (lengths 2-9)
    mid: [[Prob; 8]; NUM_POS_STATES],   // 3-bit trees (lengths 10-17)
    high: [Prob; 256],                   // 8-bit tree  (lengths 18-273)
}

impl LengthCoder {
    fn new() -> Self {
        Self {
            choice: [PROB_INIT; NUM_POS_STATES],
            choice2: [PROB_INIT; NUM_POS_STATES],
            low: [[PROB_INIT; 8]; NUM_POS_STATES],
            mid: [[PROB_INIT; 8]; NUM_POS_STATES],
            high: [PROB_INIT; 256],
        }
    }

    fn encode_tree3(enc: &mut RangeEncoder, probs: &mut [Prob; 8], value: u32) {
        let mut sym = 1u32;
        for bit_idx in (0..3).rev() {
            let bit = (value >> bit_idx) & 1;
            enc.encode_bit(&mut probs[sym as usize], bit);
            sym = (sym << 1) | bit;
        }
    }

    fn decode_tree3(dec: &mut RangeDecoder, probs: &mut [Prob; 8]) -> usize {
        let mut sym = 1u32;
        for _ in 0..3 {
            let bit = dec.decode_bit(&mut probs[sym as usize]);
            sym = (sym << 1) | bit;
        }
        (sym - 8) as usize
    }

    fn encode_tree8(enc: &mut RangeEncoder, probs: &mut [Prob; 256], value: u32) {
        let mut sym = 1u32;
        for bit_idx in (0..8).rev() {
            let bit = (value >> bit_idx) & 1;
            enc.encode_bit(&mut probs[sym as usize], bit);
            sym = (sym << 1) | bit;
        }
    }

    fn decode_tree8(dec: &mut RangeDecoder, probs: &mut [Prob; 256]) -> usize {
        let mut sym = 1u32;
        for _ in 0..8 {
            let bit = dec.decode_bit(&mut probs[sym as usize]);
            sym = (sym << 1) | bit;
        }
        (sym - 256) as usize
    }

    fn encode(&mut self, enc: &mut RangeEncoder, len: usize, pos_state: usize) {
        let value = len.saturating_sub(2);
        if value < 8 {
            enc.encode_bit(&mut self.choice[pos_state], 0);
            Self::encode_tree3(enc, &mut self.low[pos_state], value as u32);
            return;
        }

        enc.encode_bit(&mut self.choice[pos_state], 1);
        let value = value - 8;
        if value < 8 {
            enc.encode_bit(&mut self.choice2[pos_state], 0);
            Self::encode_tree3(enc, &mut self.mid[pos_state], value as u32);
        } else {
            enc.encode_bit(&mut self.choice2[pos_state], 1);
            Self::encode_tree8(enc, &mut self.high, (value - 8).min(255) as u32);
        }
    }

    fn decode(&mut self, dec: &mut RangeDecoder, pos_state: usize) -> usize {
        if dec.decode_bit(&mut self.choice[pos_state]) == 0 {
            return 2 + Self::decode_tree3(dec, &mut self.low[pos_state]);
        }
        if dec.decode_bit(&mut self.choice2[pos_state]) == 0 {
            return 10 + Self::decode_tree3(dec, &mut self.mid[pos_state]);
        }
        18 + Self::decode_tree8(dec, &mut self.high)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn move_rep_to_front(reps: &mut [u32; 4], idx: usize) {
    let value = reps[idx];
    for i in (1..=idx).rev() {
        reps[i] = reps[i - 1];
    }
    reps[0] = value;
}

fn copy_match(output: &mut Vec<u8>, offset: usize, length: usize) -> Result<(), LzmaStyleError> {
    if offset == 0 || offset > output.len() {
        return Err(LzmaStyleError::InvalidMatch {
            offset,
            available: output.len(),
        });
    }
    let start = output.len() - offset;
    for i in 0..length {
        let b = output[start + i];
        output.push(b);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

pub fn encode_block(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    if data.is_empty() {
        return out;
    }

    let mut lz = Lz77Encoder::new();
    let (tokens, _) = lz.encode(data);

    let mut enc = RangeEncoder::new();
    let mut state = LzmaState::new();
    let mut probs = StateProbs::new();
    let mut lit = LiteralCoder::new();
    let mut len_coder = LengthCoder::new();
    let mut rep_len_coder = LengthCoder::new();
    let mut dist_coder = DistanceCoder::new();
    let mut reps = [1u32, 2u32, 3u32, 4u32];
    let mut pos = 0usize;

    for token in tokens {
        let pos_state = pos & 0x3;
        match token {
            Token::Literal(byte) => {
                enc.encode_bit(&mut probs.is_match[state.state][pos_state], 0);
                let prev_byte = if pos == 0 { 0 } else { data[pos - 1] };
                let ctx = LiteralCoder::context_index(pos, prev_byte);
                if state.is_literal_state() || reps[0] as usize > pos {
                    lit.encode_literal(&mut enc, byte, ctx);
                } else {
                    let match_byte = data[pos - reps[0] as usize];
                    lit.encode_matched_literal(&mut enc, byte, match_byte, ctx);
                }
                state.update_literal();
                pos += 1;
            }
            Token::Match { offset, length } => {
                let length = length as usize;
                if let Some(rep_idx) = reps.iter().position(|&rep| rep == offset) {
                    // Rep match — use rep_len_coder (separate from match len coder)
                    enc.encode_bit(&mut probs.is_match[state.state][pos_state], 1);
                    enc.encode_bit(&mut probs.is_rep[state.state], 1);

                    if rep_idx == 0 {
                        enc.encode_bit(&mut probs.is_rep0[state.state], 0);
                        if length == 1 {
                            enc.encode_bit(
                                &mut probs.is_rep0_long[state.state][pos_state],
                                0,
                            );
                        } else {
                            enc.encode_bit(
                                &mut probs.is_rep0_long[state.state][pos_state],
                                1,
                            );
                            rep_len_coder.encode(&mut enc, length, pos_state);
                        }
                    } else {
                        enc.encode_bit(&mut probs.is_rep0[state.state], 1);
                        if rep_idx == 1 {
                            enc.encode_bit(&mut probs.is_rep1[state.state], 0);
                        } else {
                            enc.encode_bit(&mut probs.is_rep1[state.state], 1);
                            enc.encode_bit(
                                &mut probs.is_rep2[state.state],
                                if rep_idx == 2 { 0 } else { 1 },
                            );
                        }
                        rep_len_coder.encode(&mut enc, length, pos_state);
                    }

                    if rep_idx == 0 && length == 1 {
                        state.update_shortrep();
                        pos += 1;
                    } else {
                        state.update_rep();
                        move_rep_to_front(&mut reps, rep_idx);
                        pos += length;
                    }
                } else {
                    // Normal match
                    enc.encode_bit(&mut probs.is_match[state.state][pos_state], 1);
                    enc.encode_bit(&mut probs.is_rep[state.state], 0);
                    len_coder.encode(&mut enc, length, pos_state);
                    dist_coder.encode(&mut enc, offset, length);
                    reps = [offset, reps[0], reps[1], reps[2]];
                    state.update_match();
                    pos += length;
                }
            }
        }
    }

    out.extend_from_slice(&enc.finish());
    out
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

pub fn decode_block(payload: &[u8]) -> Result<Vec<u8>, LzmaStyleError> {
    if payload.len() < 4 {
        return Err(LzmaStyleError::InvalidPayload("payload too short"));
    }
    let orig_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    if orig_len == 0 {
        return Ok(Vec::new());
    }
    let stream = &payload[4..];
    if stream.len() < 5 {
        return Err(LzmaStyleError::InvalidPayload("range stream too short"));
    }

    let mut dec = RangeDecoder::new(stream);
    let mut state = LzmaState::new();
    let mut probs = StateProbs::new();
    let mut lit = LiteralCoder::new();
    let mut len_coder = LengthCoder::new();
    let mut rep_len_coder = LengthCoder::new();
    let mut dist_coder = DistanceCoder::new();
    let mut reps = [1u32, 2u32, 3u32, 4u32];
    let mut output = Vec::with_capacity(orig_len);

    while output.len() < orig_len {
        let pos_state = output.len() & 0x3;
        let is_match = dec.decode_bit(&mut probs.is_match[state.state][pos_state]);
        if is_match == 0 {
            let prev_byte = output.last().copied().unwrap_or(0);
            let ctx = LiteralCoder::context_index(output.len(), prev_byte);
            let byte = if state.is_literal_state() || reps[0] as usize > output.len() {
                lit.decode_literal(&mut dec, ctx)
            } else {
                let match_byte = output[output.len() - reps[0] as usize];
                lit.decode_matched_literal(&mut dec, match_byte, ctx)
            };
            output.push(byte);
            state.update_literal();
            continue;
        }

        let is_rep = dec.decode_bit(&mut probs.is_rep[state.state]);
        if is_rep == 0 {
            let length = len_coder.decode(&mut dec, pos_state);
            let distance = dist_coder.decode(&mut dec, length) as usize;
            copy_match(&mut output, distance, length)?;
            reps = [distance as u32, reps[0], reps[1], reps[2]];
            state.update_match();
            continue;
        }

        let distance = if dec.decode_bit(&mut probs.is_rep0[state.state]) == 0 {
            if dec.decode_bit(&mut probs.is_rep0_long[state.state][pos_state]) == 0 {
                let distance = reps[0] as usize;
                copy_match(&mut output, distance, 1)?;
                state.update_shortrep();
                continue;
            }
            reps[0] as usize
        } else {
            let rep_idx = if dec.decode_bit(&mut probs.is_rep1[state.state]) == 0 {
                1
            } else if dec.decode_bit(&mut probs.is_rep2[state.state]) == 0 {
                2
            } else {
                3
            };
            let distance = reps[rep_idx] as usize;
            move_rep_to_front(&mut reps, rep_idx);
            distance
        };

        let length = rep_len_coder.decode(&mut dec, pos_state);
        copy_match(&mut output, distance, length)?;
        state.update_rep();
    }

    output.truncate(orig_len);
    Ok(output)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dist_slot_roundtrip() {
        // Verify dist_slot and slot_base_and_bits are consistent
        for d in 0..1000u32 {
            let slot = dist_slot(d);
            let (base, _bits) = slot_base_and_bits(slot);
            assert!(
                d >= base,
                "dist={d} slot={slot} base={base}"
            );
            if slot >= 4 {
                let (_, num_extra) = slot_base_and_bits(slot);
                assert!(
                    d < base + (1 << num_extra),
                    "dist={d} slot={slot} base={base} num_extra={num_extra}"
                );
            }
        }
    }

    #[test]
    fn roundtrip_text_block() {
        let data = b"The quick brown fox jumps over the lazy dog. ".repeat(200);
        let encoded = encode_block(&data);
        let decoded = decode_block(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_binary_block() {
        let data: Vec<u8> = (0..8192).map(|i| ((i * 31) & 0xFF) as u8).collect();
        let encoded = encode_block(&data);
        let decoded = decode_block(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_empty() {
        let data = b"";
        let encoded = encode_block(data);
        let decoded = decode_block(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_single_byte() {
        let data = b"x";
        let encoded = encode_block(data);
        let decoded = decode_block(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_highly_repetitive() {
        let data = b"abcabc".repeat(5000);
        let encoded = encode_block(&data);
        let decoded = decode_block(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }

    #[test]
    fn measure_bpb_english_text() {
        // Simulate book-like English text
        let phrases = [
            b"The quick brown fox jumps over the lazy dog. ".as_slice(),
            b"Data compression reduces the size of data. ",
            b"Information theory provides the foundation. ",
            b"Entropy coding achieves near-optimal compression. ",
            b"The brown fox was quick and nimble. ",
            b"Redundancy in language enables compression. ",
            b"Statistical models predict the next symbol. ",
            b"Context modeling improves prediction accuracy. ",
        ];
        let mut data = Vec::new();
        for i in 0..2000 {
            data.extend_from_slice(phrases[i % phrases.len()]);
        }

        let encoded = encode_block(&data);
        let decoded = decode_block(&encoded).expect("decode ok");
        assert_eq!(decoded, data);

        let bpb = (encoded.len() as f64 * 8.0) / data.len() as f64;
        eprintln!(
            "LZMA-style bpb on English-like text: {:.3} ({} -> {} bytes)",
            bpb,
            data.len(),
            encoded.len()
        );
        // With proper distance coding, we should beat naive 22-bit distances
        assert!(bpb < 2.0, "bpb too high: {bpb:.3}");
    }

    #[test]
    fn measure_bpb_english_sample_file() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/english_sample.txt");
        if let Ok(data) = std::fs::read(path) {
            if data.is_empty() {
                return;
            }
            let encoded = encode_block(&data);
            let decoded = decode_block(&encoded).expect("decode ok");
            assert_eq!(decoded, data);

            let bpb = (encoded.len() as f64 * 8.0) / data.len() as f64;
            eprintln!(
                "LZMA-style bpb on english_sample.txt: {:.3} ({} -> {} bytes)",
                bpb,
                data.len(),
                encoded.len()
            );
        }
    }

    #[test]
    fn roundtrip_long_distance_matches() {
        // Create data with matches at various distances
        let mut data = Vec::new();
        for _ in 0..100 {
            data.extend_from_slice(b"hello world ");
        }
        // Push some unique data to create distance
        for i in 0..10000u16 {
            data.push((i & 0xFF) as u8);
        }
        // Repeat the pattern so there are long-distance matches
        for _ in 0..100 {
            data.extend_from_slice(b"hello world ");
        }
        let encoded = encode_block(&data);
        let decoded = decode_block(&encoded).expect("decode ok");
        assert_eq!(decoded, data);
    }
}
