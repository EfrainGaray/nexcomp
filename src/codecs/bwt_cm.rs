//! Context-mixing entropy coder for BWT output (after Ilia Muraviev's BCM).
//!
//! Each byte is coded MSB-first as 8 binary decisions. The probability of a 1
//! mixes three adaptive counters indexed by the partial byte: order 0, order 1
//! on the previous byte, and the same order-1 table read with the byte before
//! it. An SSE stage keyed by the partial byte and a run flag refines the mix.

const COUNTER_INIT: u16 = 1 << 15;
const SSE_BUCKETS: usize = 17;

/// Move a 16-bit probability of a 1 toward the observed bit.
#[inline]
fn adapt(p: &mut u16, bit: bool, rate: u32) {
    if bit {
        *p += (u16::MAX - *p) >> rate;
    } else {
        *p -= *p >> rate;
    }
}

struct Model {
    order0: [u16; 256],
    order1: Vec<[u16; 256]>,
    sse: Vec<[u16; SSE_BUCKETS]>,
    /// Partial byte with a leading 1 bit (1..=255).
    node: usize,
    c1: usize,
    c2: usize,
    run: u32,
    sse_row: usize,
    bucket: usize,
}

impl Model {
    fn new() -> Self {
        // SSE starts as the identity map over 16 equal-width buckets.
        let mut identity = [0u16; SSE_BUCKETS];
        for (k, v) in identity.iter_mut().enumerate() {
            *v = ((k << 12) - usize::from(k == SSE_BUCKETS - 1)) as u16;
        }
        Self {
            order0: [COUNTER_INIT; 256],
            order1: vec![[COUNTER_INIT; 256]; 256],
            sse: vec![identity; 2 * 256],
            node: 1,
            c1: 0,
            c2: 0,
            run: 0,
            sse_row: 0,
            bucket: 0,
        }
    }

    /// Probability (16-bit fixed point) that the next bit is 1.
    fn predict(&mut self) -> u32 {
        let p0 = u32::from(self.order0[self.node]);
        let p1 = u32::from(self.order1[self.c1][self.node]);
        let p2 = u32::from(self.order1[self.c2][self.node]);
        let p = ((p0 + p1) * 7 + p2 * 2) >> 4;

        self.sse_row = (usize::from(self.run > 2) << 8) | self.node;
        self.bucket = (p >> 12) as usize;
        let row = &self.sse[self.sse_row];
        let lo = i64::from(row[self.bucket]);
        let hi = i64::from(row[self.bucket + 1]);
        let refined = (lo + (((hi - lo) * i64::from(p & 4095)) >> 12)) as u32;

        ((p + 3 * refined) >> 2).clamp(1, 65535)
    }

    fn update(&mut self, bit: bool) {
        adapt(&mut self.order0[self.node], bit, 2);
        adapt(&mut self.order1[self.c1][self.node], bit, 4);
        let row = &mut self.sse[self.sse_row];
        adapt(&mut row[self.bucket], bit, 6);
        adapt(&mut row[self.bucket + 1], bit, 6);

        self.node = (self.node << 1) | usize::from(bit);
        if self.node >= 256 {
            self.c2 = self.c1;
            self.c1 = self.node & 0xFF;
            self.run = if self.c1 == self.c2 { self.run + 1 } else { 0 };
            self.node = 1;
        }
    }
}

/// Carry-less binary arithmetic coder over 32-bit bounds.
struct Encoder {
    x1: u32,
    x2: u32,
    out: Vec<u8>,
}

impl Encoder {
    fn encode(&mut self, bit: bool, p1: u32) {
        let xmid = self.x1 + ((u64::from(self.x2 - self.x1) * u64::from(p1)) >> 16) as u32;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xFF00_0000 == 0 {
            self.out.push((self.x2 >> 24) as u8);
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 0xFF;
        }
    }
}

struct Decoder<'a> {
    x1: u32,
    x2: u32,
    x: u32,
    input: &'a [u8],
    pos: usize,
}

impl Decoder<'_> {
    fn next_byte(&mut self) -> u32 {
        let b = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        u32::from(b)
    }

    fn decode(&mut self, p1: u32) -> bool {
        let xmid = self.x1 + ((u64::from(self.x2 - self.x1) * u64::from(p1)) >> 16) as u32;
        let bit = self.x <= xmid;
        if bit {
            self.x2 = xmid;
        } else {
            self.x1 = xmid + 1;
        }
        while (self.x1 ^ self.x2) & 0xFF00_0000 == 0 {
            self.x1 <<= 8;
            self.x2 = (self.x2 << 8) | 0xFF;
            self.x = (self.x << 8) | self.next_byte();
        }
        bit
    }
}

/// Entropy-code a BWT output block.
pub fn encode(bwt: &[u8]) -> Vec<u8> {
    let mut model = Model::new();
    let mut enc = Encoder { x1: 0, x2: u32::MAX, out: Vec::with_capacity(bwt.len() / 3) };
    for &byte in bwt {
        for i in (0..8).rev() {
            let bit = (byte >> i) & 1 == 1;
            enc.encode(bit, model.predict());
            model.update(bit);
        }
    }
    enc.out.extend_from_slice(&enc.x1.to_be_bytes());
    enc.out
}

/// Decode `len` bytes of BWT output produced by [`encode`].
pub fn decode(data: &[u8], len: usize) -> Vec<u8> {
    let mut model = Model::new();
    let mut dec = Decoder { x1: 0, x2: u32::MAX, x: 0, input: data, pos: 0 };
    for _ in 0..4 {
        dec.x = (dec.x << 8) | dec.next_byte();
    }
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let mut byte = 0u8;
        for _ in 0..8 {
            let bit = dec.decode(model.predict());
            model.update(bit);
            byte = (byte << 1) | u8::from(bit);
        }
        out.push(byte);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_runs_and_noise() {
        let mut data = vec![b'a'; 5000];
        let mut state: u64 = 0xDEAD_BEEF;
        data.extend((0..5000).map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state >> 33) as u8
        }));
        data.extend(b"abababababbbbbbbbbbaaaaaaaa".repeat(100));
        assert_eq!(decode(&encode(&data), data.len()), data);
    }

    #[test]
    fn roundtrip_empty_and_single() {
        assert_eq!(decode(&encode(&[]), 0), Vec::<u8>::new());
        assert_eq!(decode(&encode(&[0xFF]), 1), vec![0xFF]);
    }

    #[test]
    fn long_run_codes_to_few_bytes() {
        let data = vec![0u8; 100_000];
        let encoded = encode(&data);
        assert!(encoded.len() < 200, "100 KB run coded to {} bytes", encoded.len());
        assert_eq!(decode(&encoded, data.len()), data);
    }
}
