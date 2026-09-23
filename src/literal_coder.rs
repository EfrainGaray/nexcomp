//! Context-dependent literal coder (LZMA-style).
//!
//! Supports both normal literal encoding and matched-literal encoding where
//! each bit is conditioned on the corresponding bit of a match byte.

use crate::range_coder::{Prob, RangeDecoder, RangeEncoder, PROB_INIT};

pub const LC: usize = 3;
pub const LP: usize = 0;
pub const NUM_LIT_CONTEXTS: usize = 1 << (LC + LP); // 8

/// Per-context probability table.
///
/// Layout (768 entries per context):
///   0..256   -- normal literal tree (index 0 unused, 1..255 used)
///   256..512 -- matched-literal tree, match_bit = 0
///   512..768 -- matched-literal tree, match_bit = 1
const LITERAL_PROBS_PER_CONTEXT: usize = 0x300;

pub struct LiteralCoder {
    probs: [[Prob; LITERAL_PROBS_PER_CONTEXT]; NUM_LIT_CONTEXTS],
}

impl LiteralCoder {
    pub fn new() -> Self {
        Self {
            probs: [[PROB_INIT; LITERAL_PROBS_PER_CONTEXT]; NUM_LIT_CONTEXTS],
        }
    }

    /// Compute the context index from the output position and previous byte.
    pub fn context_index(pos: usize, prev_byte: u8) -> usize {
        let lit_pos = pos & ((1 << LP) - 1); // LP = 0 => always 0
        let prev_ctx = (prev_byte >> (8 - LC)) as usize; // top 3 bits
        (lit_pos << LC) | prev_ctx
    }

    // -- normal literal --------------------------------------------------

    pub fn encode_literal(&mut self, enc: &mut RangeEncoder, byte: u8, ctx: usize) {
        let probs = &mut self.probs[ctx];
        let mut sym = 1u32;
        for bit_idx in (0..8).rev() {
            let bit = ((byte as u32) >> bit_idx) & 1;
            enc.encode_bit(&mut probs[sym as usize], bit);
            sym = (sym << 1) | bit;
        }
    }

    pub fn decode_literal(&mut self, dec: &mut RangeDecoder, ctx: usize) -> u8 {
        let probs = &mut self.probs[ctx];
        let mut sym = 1u32;
        for _ in 0..8 {
            let bit = dec.decode_bit(&mut probs[sym as usize]);
            sym = (sym << 1) | bit;
        }
        (sym - 256) as u8
    }

    // -- matched literal -------------------------------------------------

    pub fn encode_matched_literal(
        &mut self,
        enc: &mut RangeEncoder,
        byte: u8,
        match_byte: u8,
        ctx: usize,
    ) {
        let mut sym = 1u32;
        let mut match_bit_active = true;
        for bit_idx in (0..8).rev() {
            let bit = ((byte as u32) >> bit_idx) & 1;
            let match_bit = ((match_byte as u32) >> bit_idx) & 1;

            let prob_idx = if match_bit_active {
                let offset = if match_bit == 0 { 0x100 } else { 0x200 };
                offset + sym as usize
            } else {
                sym as usize
            };

            enc.encode_bit(&mut self.probs[ctx][prob_idx], bit);
            sym = (sym << 1) | bit;

            if match_bit_active && match_bit != bit {
                match_bit_active = false;
            }
        }
    }

    pub fn decode_matched_literal(
        &mut self,
        dec: &mut RangeDecoder,
        match_byte: u8,
        ctx: usize,
    ) -> u8 {
        let mut sym = 1u32;
        let mut match_bit_active = true;
        for bit_idx in (0..8).rev() {
            let match_bit = ((match_byte as u32) >> bit_idx) & 1;

            let prob_idx = if match_bit_active {
                let offset = if match_bit == 0 { 0x100 } else { 0x200 };
                offset + sym as usize
            } else {
                sym as usize
            };

            let bit = dec.decode_bit(&mut self.probs[ctx][prob_idx]);
            sym = (sym << 1) | bit;

            if match_bit_active && match_bit != bit {
                match_bit_active = false;
            }
        }
        (sym - 256) as u8
    }
}

impl Default for LiteralCoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_coder::{RangeDecoder, RangeEncoder};
    use rand::Rng;

    #[test]
    fn context_index_covers_prev_byte_top_bits() {
        // LP = 0, LC = 3 => context = top 3 bits of prev_byte
        for b in 0..=255u8 {
            let ctx = LiteralCoder::context_index(0, b);
            assert_eq!(ctx, (b >> 5) as usize);
        }
    }

    #[test]
    fn roundtrip_normal_literals() {
        let mut rng = rand::rng();
        let n = 1000;
        let data: Vec<u8> = (0..n).map(|_| rng.random()).collect();

        let mut coder = LiteralCoder::new();
        let mut enc = RangeEncoder::new();

        let mut prev_byte = 0u8;
        for (i, &b) in data.iter().enumerate() {
            let ctx = LiteralCoder::context_index(i, prev_byte);
            coder.encode_literal(&mut enc, b, ctx);
            prev_byte = b;
        }
        let compressed = enc.finish();

        let mut coder2 = LiteralCoder::new();
        let mut dec = RangeDecoder::new(&compressed);
        let mut prev_byte = 0u8;
        for (i, &expected) in data.iter().enumerate() {
            let ctx = LiteralCoder::context_index(i, prev_byte);
            let got = coder2.decode_literal(&mut dec, ctx);
            assert_eq!(got, expected, "normal literal mismatch at {i}");
            prev_byte = got;
        }
    }

    #[test]
    fn roundtrip_matched_literals() {
        let mut rng = rand::rng();
        let n = 500;
        let data: Vec<u8> = (0..n).map(|_| rng.random()).collect();
        let match_bytes: Vec<u8> = (0..n).map(|_| rng.random()).collect();

        let mut coder = LiteralCoder::new();
        let mut enc = RangeEncoder::new();

        for i in 0..n {
            let ctx = LiteralCoder::context_index(i, if i == 0 { 0 } else { data[i - 1] });
            coder.encode_matched_literal(&mut enc, data[i], match_bytes[i], ctx);
        }
        let compressed = enc.finish();

        let mut coder2 = LiteralCoder::new();
        let mut dec = RangeDecoder::new(&compressed);
        for i in 0..n {
            let ctx = LiteralCoder::context_index(i, if i == 0 { 0 } else { data[i - 1] });
            let got = coder2.decode_matched_literal(&mut dec, match_bytes[i], ctx);
            assert_eq!(got, data[i], "matched literal mismatch at {i}");
        }
    }

    #[test]
    fn roundtrip_matched_literal_same_as_match_byte() {
        // When byte == match_byte, matched encoding should still roundtrip.
        let mut coder = LiteralCoder::new();
        let mut enc = RangeEncoder::new();

        for b in 0..=255u8 {
            coder.encode_matched_literal(&mut enc, b, b, 0);
        }
        let compressed = enc.finish();

        let mut coder2 = LiteralCoder::new();
        let mut dec = RangeDecoder::new(&compressed);
        for b in 0..=255u8 {
            let got = coder2.decode_matched_literal(&mut dec, b, 0);
            assert_eq!(got, b);
        }
    }

    #[test]
    fn roundtrip_mixed_normal_and_matched() {
        let mut rng = rand::rng();
        let n = 600;

        #[derive(Clone)]
        enum Op {
            Normal(u8),
            Matched(u8, u8),
        }

        let ops: Vec<Op> = (0..n)
            .map(|_| {
                if rng.random_bool(0.5) {
                    Op::Normal(rng.random())
                } else {
                    Op::Matched(rng.random(), rng.random())
                }
            })
            .collect();

        let mut coder = LiteralCoder::new();
        let mut enc = RangeEncoder::new();
        let mut prev: u8 = 0;

        for (i, op) in ops.iter().enumerate() {
            let ctx = LiteralCoder::context_index(i, prev);
            match *op {
                Op::Normal(b) => {
                    enc.encode_direct_bits(0, 1); // flag: normal
                    coder.encode_literal(&mut enc, b, ctx);
                    prev = b;
                }
                Op::Matched(b, mb) => {
                    enc.encode_direct_bits(1, 1); // flag: matched
                    coder.encode_matched_literal(&mut enc, b, mb, ctx);
                    prev = b;
                }
            }
        }
        let compressed = enc.finish();

        let mut coder2 = LiteralCoder::new();
        let mut dec = RangeDecoder::new(&compressed);
        let mut prev: u8 = 0;

        for (i, op) in ops.iter().enumerate() {
            let ctx = LiteralCoder::context_index(i, prev);
            let flag = dec.decode_direct_bits(1);
            match *op {
                Op::Normal(expected) => {
                    assert_eq!(flag, 0, "flag mismatch at {i}");
                    let got = coder2.decode_literal(&mut dec, ctx);
                    assert_eq!(got, expected, "normal mismatch at {i}");
                    prev = got;
                }
                Op::Matched(expected, mb) => {
                    assert_eq!(flag, 1, "flag mismatch at {i}");
                    let got = coder2.decode_matched_literal(&mut dec, mb, ctx);
                    assert_eq!(got, expected, "matched mismatch at {i}");
                    prev = got;
                }
            }
        }
    }
}
