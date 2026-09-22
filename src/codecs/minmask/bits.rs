//! Bit containers and a bit-exact writer/reader for the MinMask codec.

/// Fixed-length bit vector; bit `i` lives in word `i / 64` at bit `i % 64`.
/// Bits past `len` are always zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitVec {
    pub words: Vec<u64>,
    pub len: usize,
}

impl BitVec {
    pub fn zeros(len: usize) -> Self {
        Self { words: vec![0; len.div_ceil(64)], len }
    }

    #[inline]
    pub fn get(&self, i: usize) -> bool {
        (self.words[i >> 6] >> (i & 63)) & 1 == 1
    }

    #[inline]
    pub fn set(&mut self, i: usize, v: bool) {
        let (w, b) = (i >> 6, i & 63);
        if v {
            self.words[w] |= 1 << b;
        } else {
            self.words[w] &= !(1 << b);
        }
    }

    pub fn count_ones(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// `len` bits starting at signed bit offset `start`; positions outside
    /// `0..self.len` read as zero.
    pub fn slice_from(&self, start: i64, len: usize) -> BitVec {
        let mut out = BitVec::zeros(len);
        if len == 0 {
            return out;
        }
        if start >= 0 && start as usize % 64 == 0 {
            let w0 = start as usize / 64;
            for (k, word) in out.words.iter_mut().enumerate() {
                *word = self.words.get(w0 + k).copied().unwrap_or(0);
            }
        } else {
            for (k, word) in out.words.iter_mut().enumerate() {
                *word = self.word_at(start + 64 * k as i64);
            }
        }
        out.clear_tail();
        out
    }

    /// 64 bits starting at signed bit offset `pos` (outside bits are zero).
    fn word_at(&self, pos: i64) -> u64 {
        let w = pos.div_euclid(64);
        let b = pos.rem_euclid(64) as u32;
        let lo = self.word_or_zero(w);
        if b == 0 {
            return lo;
        }
        let hi = self.word_or_zero(w + 1);
        (lo >> b) | (hi << (64 - b))
    }

    fn word_or_zero(&self, w: i64) -> u64 {
        if w < 0 {
            0
        } else {
            self.words.get(w as usize).copied().unwrap_or(0)
        }
    }

    pub fn clear_tail(&mut self) {
        let r = self.len % 64;
        if r != 0 {
            if let Some(last) = self.words.last_mut() {
                *last &= (1u64 << r) - 1;
            }
        }
    }

    pub fn xor_assign(&mut self, other: &BitVec) {
        for (a, b) in self.words.iter_mut().zip(&other.words) {
            *a ^= b;
        }
    }

    pub fn not_assign(&mut self) {
        for w in &mut self.words {
            *w = !*w;
        }
        self.clear_tail();
    }

    /// Write `src` into `self` at bit offset `at` (word-level when aligned).
    pub fn write_at(&mut self, at: usize, src: &BitVec) {
        if at % 64 == 0 {
            let w0 = at / 64;
            let full = src.len / 64;
            self.words[w0..w0 + full].copy_from_slice(&src.words[..full]);
            for i in 64 * full..src.len {
                self.set(at + i, src.get(i));
            }
        } else {
            for i in 0..src.len {
                self.set(at + i, src.get(i));
            }
        }
    }

    /// Positions of set bits, ascending.
    pub fn ones(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.count_ones());
        for (k, &w) in self.words.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                out.push((64 * k) as u32 + w.trailing_zeros());
                w &= w - 1;
            }
        }
        out
    }

    /// Lengths of maximal runs of equal bits, in order.
    pub fn runs(&self) -> Vec<u32> {
        let mut runs = Vec::new();
        if self.len == 0 {
            return runs;
        }
        let mut start = 0usize;
        let mut value = self.get(0);
        while start < self.len {
            let end = self.next_diff(start, value);
            runs.push((end - start) as u32);
            start = end;
            value = !value;
        }
        runs
    }

    /// First index >= `from` whose bit differs from `value` (or `len`).
    fn next_diff(&self, from: usize, value: bool) -> usize {
        let mut w = from / 64;
        let flip = if value { u64::MAX } else { 0 };
        let mut word = (self.words[w] ^ flip) & (u64::MAX << (from % 64));
        loop {
            if word != 0 {
                return (64 * w + word.trailing_zeros() as usize).min(self.len);
            }
            w += 1;
            if w >= self.words.len() {
                return self.len;
            }
            word = self.words[w] ^ flip;
        }
    }
}

/// Elias-gamma length of `v >= 1`.
#[inline]
pub fn gamma_len(v: u64) -> usize {
    2 * (63 - v.leading_zeros() as usize) + 1
}

/// Bits needed to write any value in `0..n` (0 when `n <= 1`).
#[inline]
pub fn width_for(n: u64) -> u32 {
    if n <= 1 {
        0
    } else {
        64 - (n - 1).leading_zeros()
    }
}

/// MSB-first bit writer.
#[derive(Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    nacc: u32,
    bits: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bits(&self) -> usize {
        self.bits
    }

    /// Write the low `n` bits of `value`, most significant first.
    pub fn put(&mut self, value: u64, n: u32) {
        if n == 0 {
            return;
        }
        if n > 32 {
            self.put(value >> 32, n - 32);
            self.put(value & 0xFFFF_FFFF, 32);
            return;
        }
        let v = value & ((1u64 << n) - 1);
        self.acc = (self.acc << n) | v;
        self.nacc += n;
        self.bits += n as usize;
        while self.nacc >= 8 {
            self.nacc -= 8;
            self.bytes.push((self.acc >> self.nacc) as u8);
        }
        self.acc &= (1u64 << self.nacc) - 1;
    }

    pub fn put_bit(&mut self, b: bool) {
        self.put(u64::from(b), 1);
    }

    /// Elias-gamma code of `v >= 1`.
    pub fn put_gamma(&mut self, v: u64) {
        let nbits = 64 - v.leading_zeros();
        self.put(0, nbits - 1);
        self.put(v, nbits);
    }

    /// Rice code: `v >> k` ones, a zero, then the low `k` bits.
    pub fn put_rice(&mut self, v: u64, k: u32) {
        let mut q = v >> k;
        while q >= 32 {
            self.put(u64::from(u32::MAX), 32);
            q -= 32;
        }
        self.put((1u64 << q) - 1, q as u32);
        self.put_bit(false);
        self.put(v, k);
    }

    /// Append the bits of `bv` in index order.
    pub fn put_bits(&mut self, bv: &BitVec) {
        let full = bv.len / 64;
        for &w in &bv.words[..full] {
            self.put(w.reverse_bits(), 64);
        }
        for i in 64 * full..bv.len {
            self.put_bit(bv.get(i));
        }
    }

    /// Append whole bytes (bit-aligned to the current position).
    pub fn put_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.put(u64::from(b), 8);
        }
    }

    /// Append everything written to `other`, bit-exact.
    pub fn append(&mut self, other: BitWriter) {
        let bits = other.bits;
        let bytes = other.finish();
        for &b in &bytes[..bits / 8] {
            self.put(u64::from(b), 8);
        }
        if bits % 8 != 0 {
            self.put(u64::from(bytes[bits / 8] >> (8 - bits % 8)), (bits % 8) as u32);
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.nacc > 0 {
            self.bytes.push((self.acc << (8 - self.nacc)) as u8);
        }
        self.bytes
    }
}

/// MSB-first bit reader; every read returns `None` past the end.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        8 * self.data.len() - self.pos
    }

    pub fn get(&mut self, n: u32) -> Option<u64> {
        if n as usize > self.remaining() {
            return None;
        }
        let mut v = 0u64;
        let mut need = n;
        while need > 0 {
            let byte = self.data[self.pos / 8];
            let avail = 8 - (self.pos % 8) as u32;
            let take = avail.min(need);
            let bits = (u64::from(byte) >> (avail - take)) & ((1u64 << take) - 1);
            v = (v << take) | bits;
            self.pos += take as usize;
            need -= take;
        }
        Some(v)
    }

    pub fn get_bit(&mut self) -> Option<bool> {
        self.get(1).map(|b| b == 1)
    }

    pub fn get_gamma(&mut self) -> Option<u64> {
        let mut zeros = 0u32;
        while !self.get_bit()? {
            zeros += 1;
            if zeros > 63 {
                return None;
            }
        }
        Some((1u64 << zeros) | self.get(zeros)?)
    }

    pub fn get_rice(&mut self, k: u32) -> Option<u64> {
        let mut q = 0u64;
        while self.get_bit()? {
            q += 1;
        }
        Some((q << k) | self.get(k)?)
    }

    pub fn get_bits(&mut self, len: usize) -> Option<BitVec> {
        if len > self.remaining() {
            return None;
        }
        let mut bv = BitVec::zeros(len);
        let full = len / 64;
        for w in bv.words[..full].iter_mut() {
            *w = self.get(64)?.reverse_bits();
        }
        for i in 64 * full..len {
            bv.set(i, self.get_bit()?);
        }
        Some(bv)
    }

    pub fn get_bytes(&mut self, len: usize) -> Option<Vec<u8>> {
        if 8 * len > self.remaining() {
            return None;
        }
        (0..len).map(|_| self.get(8).map(|b| b as u8)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_reader_roundtrip_mixed_codes() {
        let mut w = BitWriter::new();
        w.put(0b101, 3);
        w.put_gamma(1);
        w.put_gamma(37);
        w.put_rice(100, 3);
        w.put(u64::MAX, 64);
        w.put(0x1234_5678_9ABC, 48);
        let bits = w.bits();
        assert_eq!(bits, 3 + 1 + gamma_len(37) + (100 >> 3) + 1 + 3 + 64 + 48);
        let bytes = w.finish();
        assert_eq!(bytes.len(), bits.div_ceil(8));

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.get(3), Some(0b101));
        assert_eq!(r.get_gamma(), Some(1));
        assert_eq!(r.get_gamma(), Some(37));
        assert_eq!(r.get_rice(3), Some(100));
        assert_eq!(r.get(64), Some(u64::MAX));
        assert_eq!(r.get(48), Some(0x1234_5678_9ABC));
    }

    #[test]
    fn bitvec_slice_runs_and_bits_roundtrip() {
        let mut bv = BitVec::zeros(200);
        for i in [0, 1, 2, 63, 64, 65, 130, 199] {
            bv.set(i, true);
        }
        assert_eq!(bv.ones(), vec![0, 1, 2, 63, 64, 65, 130, 199]);
        assert_eq!(bv.runs().iter().sum::<u32>(), 200);
        assert_eq!(bv.runs()[..3], [3, 60, 3]);

        let s = bv.slice_from(-2, 10);
        assert_eq!((0..10).map(|i| s.get(i)).collect::<Vec<_>>(), [false, false, true, true, true, false, false, false, false, false]);
        let s = bv.slice_from(190, 20);
        assert!(s.get(9) && !s.get(10));

        let mut w = BitWriter::new();
        w.put_bit(true);
        w.put_bits(&bv);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.get_bit(), Some(true));
        assert_eq!(r.get_bits(200), Some(bv));
    }
}
