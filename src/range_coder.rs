//! Binary range coder (LZMA-compatible).
//!
//! Implements adaptive-probability bit coding with carry propagation,
//! fixed-probability direct-bit coding, and tree-based byte coding.

pub const PROB_BITS: u32 = 11;
pub const PROB_INIT: u16 = 1 << (PROB_BITS - 1); // 1024
const TOP_VALUE: u32 = 1 << 24;

pub type Prob = u16;

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

pub struct RangeEncoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    output: Vec<u8>,
}

impl Default for RangeEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RangeEncoder {
    pub fn new() -> Self {
        Self {
            low: 0,
            range: 0xFFFF_FFFF,
            cache: 0,
            cache_size: 1,
            output: Vec::new(),
        }
    }

    /// Encode one bit with adaptive probability.
    pub fn encode_bit(&mut self, prob: &mut Prob, bit: u32) {
        let bound = (self.range >> PROB_BITS) * (*prob as u32);
        if bit == 0 {
            self.range = bound;
            *prob += ((1u16 << PROB_BITS) - *prob) >> 5;
        } else {
            self.low += bound as u64;
            self.range -= bound;
            *prob -= *prob >> 5;
        }
        if self.range < TOP_VALUE {
            self.shift_low();
            self.range <<= 8;
        }
    }

    /// Encode bits with fixed 50 % probability (no model update).
    pub fn encode_direct_bits(&mut self, value: u32, count: u32) {
        for i in (0..count).rev() {
            self.range >>= 1;
            if ((value >> i) & 1) == 1 {
                self.low += self.range as u64;
            }
            if self.range < TOP_VALUE {
                self.shift_low();
                self.range <<= 8;
            }
        }
    }

    /// Encode a full byte MSB-first using a tree of 255 probabilities.
    pub fn encode_byte(&mut self, probs: &mut [Prob; 255], byte: u8) {
        let mut sym = 1u32;
        for bit_idx in (0..8).rev() {
            let bit = ((byte as u32) >> bit_idx) & 1;
            // tree index: sym ranges 1..255 -> probs[sym - 1] would also work,
            // but LZMA convention uses probs[sym] with probs[0] unused (256 entries).
            // We pack into 255 entries: index = sym - 1.
            self.encode_bit(&mut probs[sym as usize - 1], bit);
            sym = (sym << 1) | bit;
        }
    }

    /// Encode a symbol with cumulative frequency distribution.
    /// cum_low: cumulative frequency below this symbol
    /// freq: frequency of this symbol
    /// total: total of all frequencies
    pub fn encode_freq(&mut self, cum_low: u32, freq: u32, total: u32) {
        debug_assert!(freq > 0, "freq must be > 0");
        debug_assert!(total > 0, "total must be > 0");
        debug_assert!(cum_low + freq <= total, "cum_low + freq must be <= total");
        let r = self.range / total;
        self.low += cum_low as u64 * r as u64;
        self.range = if cum_low + freq == total {
            self.range - r * cum_low // last symbol gets remaining range
        } else {
            r * freq
        };
        while self.range < TOP_VALUE {
            self.shift_low();
            self.range <<= 8;
        }
    }

    /// Flush the encoder and return the compressed byte stream.
    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.output
    }

    /// LZMA carry-propagation shift.
    fn shift_low(&mut self) {
        let low_hi = (self.low >> 32) as u8; // 0 or 1 (carry)
        if self.low < 0xFF00_0000 || low_hi != 0 {
            let mut cache = self.cache;
            loop {
                self.output.push(cache.wrapping_add(low_hi));
                cache = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
            self.cache_size = 1;
        } else {
            self.cache_size += 1;
        }
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

pub struct RangeDecoder<'a> {
    range: u32,
    code: u32,
    input: &'a [u8],
    pos: usize,
}

impl<'a> RangeDecoder<'a> {
    /// Create a decoder, consuming the first 5 bytes to initialise `code`.
    pub fn new(data: &'a [u8]) -> Self {
        assert!(data.len() >= 5, "range coder stream too short");
        // First byte is discarded (LZMA convention), next 4 form `code`.
        let code = ((data[1] as u32) << 24)
            | ((data[2] as u32) << 16)
            | ((data[3] as u32) << 8)
            | (data[4] as u32);
        Self {
            range: 0xFFFF_FFFF,
            code,
            input: data,
            pos: 5,
        }
    }

    fn next_byte(&mut self) -> u8 {
        if self.pos < self.input.len() {
            let b = self.input[self.pos];
            self.pos += 1;
            b
        } else {
            0
        }
    }

    /// Decode: returns the cumulative frequency that falls within current code.
    pub fn get_freq(&self, total: u32) -> u32 {
        let r = self.range / total;
        
        (self.code / r).min(total - 1)
    }

    /// Update decoder state after decoding symbol.
    pub fn decode_freq(&mut self, cum_low: u32, freq: u32, total: u32) {
        let r = self.range / total;
        self.code -= cum_low * r;
        self.range = if cum_low + freq == total {
            self.range - r * cum_low
        } else {
            r * freq
        };
        while self.range < TOP_VALUE {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next_byte() as u32;
        }
    }

    /// Decode one bit with adaptive probability.
    pub fn decode_bit(&mut self, prob: &mut Prob) -> u32 {
        let bound = (self.range >> PROB_BITS) * (*prob as u32);
        let bit;
        if self.code < bound {
            self.range = bound;
            *prob += ((1u16 << PROB_BITS) - *prob) >> 5;
            bit = 0;
        } else {
            self.code -= bound;
            self.range -= bound;
            *prob -= *prob >> 5;
            bit = 1;
        }
        if self.range < TOP_VALUE {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next_byte() as u32;
        }
        bit
    }

    /// Decode bits with fixed 50 % probability.
    pub fn decode_direct_bits(&mut self, count: u32) -> u32 {
        let mut value = 0u32;
        for _ in 0..count {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            let t = (self.code as i32 >> 31) as u32; // 0xFFFFFFFF if code was >= range (underflow)
            self.code = self.code.wrapping_add(self.range & t);
            // bit = 1 - t  (t=0 means code didn't underflow -> bit=1; t=0xFFFFFFFF -> bit=0)
            value = (value << 1) | (1u32.wrapping_sub(t & 1));
            if self.range < TOP_VALUE {
                self.range <<= 8;
                self.code = (self.code << 8) | self.next_byte() as u32;
            }
        }
        value
    }

    /// Decode a full byte MSB-first using a tree of 255 probabilities.
    pub fn decode_byte(&mut self, probs: &mut [Prob; 255]) -> u8 {
        let mut sym = 1u32;
        for _ in 0..8 {
            let bit = self.decode_bit(&mut probs[sym as usize - 1]);
            sym = (sym << 1) | bit;
        }
        (sym - 256) as u8
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
    mod tests {
    use super::*;
    use rand::Rng;

    #[test]
    fn roundtrip_random_bits() {
        let mut rng = rand::rng();
        let n = 10_000;
        let bits: Vec<u32> = (0..n).map(|_| rng.random_range(0..2)).collect();

        // Encode
        let mut enc = RangeEncoder::new();
        let mut prob = PROB_INIT;
        for &b in &bits {
            enc.encode_bit(&mut prob, b);
        }
        let compressed = enc.finish();

        // Decode
        let mut dec = RangeDecoder::new(&compressed);
        let mut prob2 = PROB_INIT;
        for (i, &expected) in bits.iter().enumerate() {
            let got = dec.decode_bit(&mut prob2);
            assert_eq!(got, expected, "mismatch at bit {i}");
        }
    }

    #[test]
    fn roundtrip_skewed_bits() {
        // Test with highly skewed probabilities
        let mut rng = rand::rng();
        let n = 12_000;
        // 90 % zeros
        let bits: Vec<u32> = (0..n)
            .map(|_| if rng.random_range(0..10) < 9 { 0 } else { 1 })
            .collect();

        let mut enc = RangeEncoder::new();
        let mut prob = PROB_INIT;
        for &b in &bits {
            enc.encode_bit(&mut prob, b);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        let mut prob2 = PROB_INIT;
        for (i, &expected) in bits.iter().enumerate() {
            let got = dec.decode_bit(&mut prob2);
            assert_eq!(got, expected, "skewed mismatch at bit {i}");
        }
    }

    #[test]
    fn roundtrip_variable_probs_10k_bits() {
        let mut rng = rand::rng();
        let initial_probs: Vec<Prob> = (0..64)
            .map(|_| rng.random_range(1..(1 << PROB_BITS) as u16))
            .collect();
        let mut probs = initial_probs.clone();
        let bits: Vec<u32> = (0..10_000).map(|_| rng.random_range(0..2)).collect();

        let mut enc = RangeEncoder::new();
        for (i, &bit) in bits.iter().enumerate() {
            let idx = i % probs.len();
            enc.encode_bit(&mut probs[idx], bit);
        }
        let compressed = enc.finish();

        let mut probs2 = initial_probs;
        let mut dec = RangeDecoder::new(&compressed);
        for (i, &expected) in bits.iter().enumerate() {
            let idx = i % probs2.len();
            let got = dec.decode_bit(&mut probs2[idx]);
            assert_eq!(got, expected, "mismatch at bit {i}");
        }
    }

    #[test]
    fn roundtrip_bytes() {
        let mut rng = rand::rng();
        let n = 500;
        let bytes: Vec<u8> = (0..n).map(|_| rng.random()).collect();

        let mut enc = RangeEncoder::new();
        let mut probs = [PROB_INIT; 255];
        for &b in &bytes {
            enc.encode_byte(&mut probs, b);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        let mut probs2 = [PROB_INIT; 255];
        for (i, &expected) in bytes.iter().enumerate() {
            let got = dec.decode_byte(&mut probs2);
            assert_eq!(got, expected, "byte mismatch at index {i}");
        }
    }

    #[test]
    fn roundtrip_direct_bits() {
        let mut rng = rand::rng();
        let values: Vec<(u32, u32)> = (0..200)
            .map(|_| {
                let bits = rng.random_range(1..=26);
                let val = rng.random::<u32>() & ((1u32 << bits) - 1);
                (val, bits)
            })
            .collect();

        let mut enc = RangeEncoder::new();
        for &(val, cnt) in &values {
            enc.encode_direct_bits(val, cnt);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        for (i, &(expected_val, cnt)) in values.iter().enumerate() {
            let got = dec.decode_direct_bits(cnt);
            assert_eq!(got, expected_val, "direct bits mismatch at {i}");
        }
    }

    #[test]
    fn roundtrip_mixed() {
        // Mix adaptive bits, direct bits, and bytes in one stream.
        let mut rng = rand::rng();

        let mut enc = RangeEncoder::new();
        let mut prob = PROB_INIT;
        let mut byte_probs = [PROB_INIT; 255];

        let adaptive_bits: Vec<u32> = (0..500).map(|_| rng.random_range(0..2)).collect();
        let direct_val: u32 = rng.random::<u32>() & 0xFFFF;
        let plain_bytes: Vec<u8> = (0..100).map(|_| rng.random()).collect();

        for &b in &adaptive_bits {
            enc.encode_bit(&mut prob, b);
        }
        enc.encode_direct_bits(direct_val, 16);
        for &b in &plain_bytes {
            enc.encode_byte(&mut byte_probs, b);
        }
        let compressed = enc.finish();

        let mut dec = RangeDecoder::new(&compressed);
        let mut prob2 = PROB_INIT;
        let mut byte_probs2 = [PROB_INIT; 255];

        for (i, &expected) in adaptive_bits.iter().enumerate() {
            assert_eq!(dec.decode_bit(&mut prob2), expected, "adaptive bit {i}");
        }
        assert_eq!(dec.decode_direct_bits(16), direct_val);
        for (i, &expected) in plain_bytes.iter().enumerate() {
            assert_eq!(dec.decode_byte(&mut byte_probs2), expected, "byte {i}");
        }
    }

    #[test]
    fn roundtrip_freq_encoding() {
        // Test encode_freq / get_freq / decode_freq roundtrip
        // Simulate a simple frequency distribution: symbols 0..4 with weights [3, 1, 2, 5, 1]
        let weights = [3u32, 1, 2, 5, 1];
        let total: u32 = weights.iter().sum();
        let mut cum = vec![0u32; weights.len()];
        for i in 1..weights.len() {
            cum[i] = cum[i - 1] + weights[i - 1];
        }

        // Encode a sequence of symbols
        let symbols = vec![0u32, 3, 4, 1, 2, 0, 3, 3, 2, 0];
        let mut enc = RangeEncoder::new();
        for &sym in &symbols {
            enc.encode_freq(cum[sym as usize], weights[sym as usize], total);
        }
        let compressed = enc.finish();

        // Decode
        let mut dec = RangeDecoder::new(&compressed);
        for (i, &expected) in symbols.iter().enumerate() {
            let target = dec.get_freq(total);
            // Find which symbol this falls in
            let mut sym = 0u32;
            let mut accum = 0u32;
            for (j, &w) in weights.iter().enumerate() {
                if accum + w > target {
                    sym = j as u32;
                    break;
                }
                accum += w;
            }
            assert_eq!(sym, expected, "freq symbol mismatch at {i}");
            dec.decode_freq(cum[sym as usize], weights[sym as usize], total);
        }
    }
}
