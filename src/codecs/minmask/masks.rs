//! Mask (predictor) families. A mask renders n bits from state the decoder
//! already has (earlier bits of the stream, or its own parameters) and has an
//! exact description cost in bits.

use super::bits::{gamma_len, BitReader, BitVec, BitWriter};

/// How the input bytes are laid out as one bitstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Repr {
    /// byte0 bit7..bit0, byte1 bit7..bit0, ...
    Linear = 0,
    /// bit0 of every byte, then bit1 of every byte, ..., then bit7.
    Planes = 1,
}

impl Repr {
    /// Stream bits between the same bit of consecutive bytes.
    pub fn byte_step(self) -> usize {
        match self {
            Repr::Linear => 8,
            Repr::Planes => 1,
        }
    }
}

pub const FAMILY_BITS: u32 = 4;
const INDEX_BITS: u32 = 3;
pub const PATTERN_LENS: [usize; 6] = [2, 4, 8, 16, 32, 64];
/// Byte strides for `Stride` and `AddDelta`.
pub const STRIDES: [usize; 7] = [1, 2, 4, 8, 16, 32, 64];
/// Maximal-length PRBS polynomials x^a + x^b + 1 as (a, b).
pub const LFSR_POLYS: [(usize, usize); 4] = [(7, 6), (15, 14), (23, 18), (31, 28)];
/// Berlekamp-Massey fitting is quadratic; only small blocks get it.
pub const BM_MAX_BITS: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mask {
    /// All zeros (all ones with the complement flag).
    Zero,
    /// A repeating pattern of `PATTERN_LENS[len]` bits, phase-aligned to the block.
    Pattern { len: u8, bits: u64 },
    /// The stream `STRIDES[idx]` bytes back (xor delta).
    Stride { idx: u8 },
    /// The previous block (distance = nominal block size).
    PrevBlock,
    /// The stream `dist` bits back (any earlier position, incl. overlapping).
    Match { dist: u64 },
    /// Linear-extrapolated bytes 2 B[i-s] - B[i-2s] (linear layout only).
    AddDelta { idx: u8 },
    /// A PRBS sequence seeded with its first `a` bits.
    Lfsr { poly: u8, seed: u64 },
    /// The shortest LFSR generating the block (Berlekamp-Massey); `conn[j]` is c_{j+1}.
    BmLfsr { conn: BitVec, seed: BitVec },
    /// A match xored with a repeating pattern.
    TwoStage { dist: u64, len: u8, bits: u64 },
}

impl Mask {
    fn family_id(&self) -> u8 {
        match self {
            Mask::Zero => 0,
            Mask::Pattern { .. } => 1,
            Mask::Stride { .. } => 2,
            Mask::PrevBlock => 3,
            Mask::Match { .. } => 4,
            Mask::AddDelta { .. } => 5,
            Mask::Lfsr { .. } => 6,
            Mask::BmLfsr { .. } => 7,
            Mask::TwoStage { .. } => 8,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Mask::Zero => "zero",
            Mask::Pattern { .. } => "pattern",
            Mask::Stride { .. } => "stride",
            Mask::PrevBlock => "prev-block",
            Mask::Match { .. } => "match",
            Mask::AddDelta { .. } => "add-delta",
            Mask::Lfsr { .. } => "lfsr",
            Mask::BmLfsr { .. } => "bm-lfsr",
            Mask::TwoStage { .. } => "two-stage",
        }
    }

    /// Exact description size in bits, family id included.
    pub fn cost(&self) -> usize {
        FAMILY_BITS as usize
            + match self {
                Mask::Zero | Mask::PrevBlock => 0,
                Mask::Pattern { len, .. } => INDEX_BITS as usize + PATTERN_LENS[*len as usize],
                Mask::Stride { .. } | Mask::AddDelta { .. } => INDEX_BITS as usize,
                Mask::Match { dist } => gamma_len(*dist),
                Mask::Lfsr { poly, .. } => 2 + LFSR_POLYS[*poly as usize].0,
                Mask::BmLfsr { conn, .. } => gamma_len(conn.len as u64 + 1) + 2 * conn.len,
                Mask::TwoStage { dist, len, .. } => {
                    gamma_len(*dist) + INDEX_BITS as usize + PATTERN_LENS[*len as usize]
                }
            }
    }

    pub fn write(&self, w: &mut BitWriter) {
        w.put(u64::from(self.family_id()), FAMILY_BITS);
        match self {
            Mask::Zero | Mask::PrevBlock => {}
            Mask::Pattern { len, bits } => {
                w.put(u64::from(*len), INDEX_BITS);
                w.put(*bits, PATTERN_LENS[*len as usize] as u32);
            }
            Mask::Stride { idx } | Mask::AddDelta { idx } => w.put(u64::from(*idx), INDEX_BITS),
            Mask::Match { dist } => w.put_gamma(*dist),
            Mask::Lfsr { poly, seed } => {
                w.put(u64::from(*poly), 2);
                w.put(*seed, LFSR_POLYS[*poly as usize].0 as u32);
            }
            Mask::BmLfsr { conn, seed } => {
                w.put_gamma(conn.len as u64 + 1);
                w.put_bits(conn);
                w.put_bits(seed);
            }
            Mask::TwoStage { dist, len, bits } => {
                w.put_gamma(*dist);
                w.put(u64::from(*len), INDEX_BITS);
                w.put(*bits, PATTERN_LENS[*len as usize] as u32);
            }
        }
    }

    pub fn read(r: &mut BitReader, repr: Repr) -> Option<Mask> {
        let len_idx = |r: &mut BitReader| -> Option<u8> {
            let i = r.get(INDEX_BITS)? as usize;
            (i < PATTERN_LENS.len()).then_some(i as u8)
        };
        let stride_idx = |r: &mut BitReader| -> Option<u8> {
            let i = r.get(INDEX_BITS)? as usize;
            (i < STRIDES.len()).then_some(i as u8)
        };
        Some(match r.get(FAMILY_BITS)? {
            0 => Mask::Zero,
            1 => {
                let len = len_idx(r)?;
                Mask::Pattern { len, bits: r.get(PATTERN_LENS[len as usize] as u32)? }
            }
            2 => Mask::Stride { idx: stride_idx(r)? },
            3 => Mask::PrevBlock,
            4 => Mask::Match { dist: r.get_gamma()? },
            5 if repr == Repr::Linear => Mask::AddDelta { idx: stride_idx(r)? },
            6 => {
                let poly = r.get(2)? as u8;
                Mask::Lfsr { poly, seed: r.get(LFSR_POLYS[poly as usize].0 as u32)? }
            }
            7 => {
                let l = (r.get_gamma()? - 1) as usize;
                if l > BM_MAX_BITS {
                    return None;
                }
                Mask::BmLfsr { conn: r.get_bits(l)?, seed: r.get_bits(l)? }
            }
            8 => {
                let dist = r.get_gamma()?;
                let len = len_idx(r)?;
                Mask::TwoStage { dist, len, bits: r.get(PATTERN_LENS[len as usize] as u32)? }
            }
            _ => return None,
        })
    }

    /// Stream distance for copy-type masks.
    fn distance(&self, repr: Repr, block_bits: usize) -> Option<u64> {
        match self {
            Mask::Stride { idx } => Some((STRIDES[*idx as usize] * repr.byte_step()) as u64),
            Mask::PrevBlock => Some(block_bits as u64),
            Mask::Match { dist } | Mask::TwoStage { dist, .. } => Some(*dist),
            _ => None,
        }
    }

    /// The mask for the block at `off` of length `n`, given the whole stream
    /// `s` (encoder side) and the input bytes (for `AddDelta`).
    pub fn render(&self, s: &BitVec, data: &[u8], off: usize, n: usize, repr: Repr, block_bits: usize) -> BitVec {
        match self {
            Mask::Zero => BitVec::zeros(n),
            Mask::Pattern { len, bits } => pattern(n, PATTERN_LENS[*len as usize], *bits),
            Mask::Lfsr { poly, seed } => lfsr(n, LFSR_POLYS[*poly as usize], *seed),
            Mask::BmLfsr { conn, seed } => bm_generate(n, conn, seed),
            Mask::AddDelta { idx } => {
                let stride = STRIDES[*idx as usize];
                let mut m = BitVec::zeros(n);
                for i in 0..n {
                    let k = off + i;
                    let predicted = add_delta_byte(|b| data[b], k / 8, stride);
                    m.set(i, (predicted >> (7 - k % 8)) & 1 == 1);
                }
                m
            }
            _ => {
                let d = self.distance(repr, block_bits).unwrap();
                let mut m = s.slice_from(off as i64 - d as i64, n);
                if let Mask::TwoStage { len, bits, .. } = self {
                    m.xor_assign(&pattern(n, PATTERN_LENS[*len as usize], *bits));
                }
                m
            }
        }
    }

    /// Decoder side: write X = E xor M (xor complement) into `s` at `off`,
    /// computing M causally from bits already reconstructed.
    pub fn reconstruct(
        &self,
        s: &mut BitVec,
        off: usize,
        e: &BitVec,
        complement: bool,
        repr: Repr,
        block_bits: usize,
    ) -> Option<()> {
        let n = e.len;
        match self {
            Mask::Zero | Mask::Pattern { .. } | Mask::Lfsr { .. } | Mask::BmLfsr { .. } => {
                let mut x = self.render(s, &[], off, n, repr, block_bits);
                x.xor_assign(e);
                if complement {
                    x.not_assign();
                }
                s.write_at(off, &x);
            }
            Mask::AddDelta { idx } => {
                let stride = STRIDES[*idx as usize];
                let mut cached = (usize::MAX, 0u8);
                for i in 0..n {
                    let k = off + i;
                    if cached.0 != k / 8 {
                        cached = (k / 8, add_delta_byte(|b| stream_byte(s, b), k / 8, stride));
                    }
                    let m = (cached.1 >> (7 - k % 8)) & 1 == 1;
                    s.set(k, e.get(i) ^ m ^ complement);
                }
            }
            _ => {
                let d = self.distance(repr, block_bits)?;
                if d == 0 {
                    return None;
                }
                let pat = match self {
                    Mask::TwoStage { len, bits, .. } => Some(pattern(n, PATTERN_LENS[*len as usize], *bits)),
                    _ => None,
                };
                if d as usize >= n {
                    // Every source bit precedes the block: copy word-wise.
                    let mut x = s.slice_from(off as i64 - d as i64, n);
                    x.xor_assign(e);
                    if let Some(p) = &pat {
                        x.xor_assign(p);
                    }
                    if complement {
                        x.not_assign();
                    }
                    s.write_at(off, &x);
                } else {
                    for i in 0..n {
                        let src = (off + i) as i64 - d as i64;
                        let mut bit = src >= 0 && s.get(src as usize);
                        if let Some(p) = &pat {
                            bit ^= p.get(i);
                        }
                        s.set(off + i, e.get(i) ^ bit ^ complement);
                    }
                }
            }
        }
        Some(())
    }
}

fn pattern(n: usize, len: usize, bits: u64) -> BitVec {
    // len divides 64, so each word is the pattern tiled 64 / len times.
    let mut word = 0u64;
    for j in 0..64 {
        word |= ((bits >> (j % len)) & 1) << j;
    }
    let mut m = BitVec::zeros(n);
    m.words.fill(word);
    m.clear_tail();
    m
}

/// Pattern minimizing the residual weight: majority bit at each phase.
pub fn majority_pattern(x: &BitVec, len: usize) -> u64 {
    let mut ones = vec![0usize; len];
    for p in x.ones() {
        ones[p as usize % len] += 1;
    }
    let mut bits = 0u64;
    for (j, &c) in ones.iter().enumerate() {
        let total = x.len / len + usize::from(j < x.len % len);
        if 2 * c > total {
            bits |= 1 << j;
        }
    }
    bits
}

fn lfsr(n: usize, (a, b): (usize, usize), seed: u64) -> BitVec {
    let mut m = BitVec::zeros(n);
    for k in 0..n {
        let bit = if k < a { (seed >> k) & 1 == 1 } else { m.get(k - a) ^ m.get(k - a + b) };
        m.set(k, bit);
    }
    m
}

pub fn lfsr_seed(x: &BitVec, a: usize) -> u64 {
    (0..a.min(x.len)).fold(0u64, |acc, k| acc | (u64::from(x.get(k)) << k))
}

fn bm_generate(n: usize, conn: &BitVec, seed: &BitVec) -> BitVec {
    let l = conn.len;
    let taps: Vec<usize> = conn.ones().into_iter().map(|j| j as usize + 1).collect();
    let mut m = BitVec::zeros(n);
    for k in 0..n {
        let bit = if k < l {
            seed.get(k)
        } else {
            taps.iter().fold(false, |acc, &j| acc ^ m.get(k - j))
        };
        m.set(k, bit);
    }
    m
}

/// Shortest LFSR generating `x` (binary Berlekamp-Massey), as (conn, seed).
pub fn bm_fit(x: &BitVec) -> (BitVec, BitVec) {
    let n = x.len;
    // rev[i] = x[n - 1 - i], so x[k-1], x[k-2], ... is rev starting at n - k.
    let mut rev = BitVec::zeros(n);
    for i in x.ones() {
        rev.set(n - 1 - i as usize, true);
    }
    let mut c = BitVec::zeros(n + 1);
    c.set(0, true);
    let mut b = c.clone();
    let mut l = 0usize;
    let mut shift = 1usize;
    for k in 0..n {
        // d = x_k + sum_{j=1..l} c_j x_{k-j}
        let taps = c.slice_from(1, l);
        let window = rev.slice_from((n - k) as i64, l);
        let dot = taps.words.iter().zip(&window.words).map(|(a, w)| (a & w).count_ones()).sum::<u32>();
        if (u32::from(x.get(k)) + dot) % 2 == 0 {
            shift += 1;
            continue;
        }
        let prev = c.clone();
        let shifted = b.slice_from(-(shift as i64), n + 1);
        c.xor_assign(&shifted);
        if 2 * l <= k {
            l = k + 1 - l;
            b = prev;
            shift = 1;
        } else {
            shift += 1;
        }
    }
    (c.slice_from(1, l), x.slice_from(0, l))
}

fn add_delta_byte(byte: impl Fn(usize) -> u8, i: usize, stride: usize) -> u8 {
    let at = |back: usize| if i >= back { byte(i - back) } else { 0 };
    at(stride).wrapping_mul(2).wrapping_sub(at(2 * stride))
}

/// Byte `b` of a linear-layout stream.
fn stream_byte(s: &BitVec, b: usize) -> u8 {
    (0..8).fold(0u8, |acc, j| (acc << 1) | u8::from(s.get(8 * b + j)))
}

/// Candidate match distances: earlier positions whose 32-bit window equals
/// the window at an anchor, found in a sorted (key, position) index over
/// byte-aligned stream positions.
pub struct MatchIndex {
    entries: Vec<(u32, u32)>,
}

impl MatchIndex {
    pub fn build(s: &BitVec) -> Self {
        let positions = s.len.saturating_sub(32) / 8 + usize::from(s.len >= 32);
        let mut entries: Vec<(u32, u32)> = (0..positions).map(|q| (window(s, 8 * q), (8 * q) as u32)).collect();
        entries.sort_unstable();
        Self { entries }
    }

    /// Up to `k` distances to the most recent earlier windows equal to the one at `anchor`.
    pub fn distances(&self, s: &BitVec, anchor: usize, k: usize) -> Vec<u64> {
        if anchor + 32 > s.len {
            return Vec::new();
        }
        let key = window(s, anchor);
        let lo = self.entries.partition_point(|&(h, _)| h < key);
        let hi = self.entries.partition_point(|&(h, p)| h < key || (h == key && (p as usize) < anchor));
        self.entries[lo..hi].iter().rev().take(k).map(|&(_, q)| (anchor - q as usize) as u64).collect()
    }
}

fn window(s: &BitVec, pos: usize) -> u32 {
    s.slice_from(pos as i64, 32).words[0] as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_of(bytes: &[u8]) -> BitVec {
        let mut s = BitVec::zeros(8 * bytes.len());
        for (i, &b) in bytes.iter().enumerate() {
            for j in 0..8 {
                s.set(8 * i + j, (b >> (7 - j)) & 1 == 1);
            }
        }
        s
    }

    #[test]
    fn bm_fit_finds_short_lfsr_of_prbs() {
        let seq = lfsr(1000, LFSR_POLYS[1], 0x4A5B);
        let (conn, seed) = bm_fit(&seq);
        assert_eq!(conn.len, 15);
        assert_eq!(bm_generate(1000, &conn, &seed), seq);
    }

    #[test]
    fn decoder_reconstruction_matches_encoder_render() {
        let data: Vec<u8> = (0..600u32).map(|i| (i * 7 + i / 13) as u8).collect();
        let s = bits_of(&data);
        let block = 1023;
        let masks = [
            Mask::Zero,
            Mask::Pattern { len: 2, bits: 0b1011 },
            Mask::Stride { idx: 0 },
            Mask::Stride { idx: 3 },
            Mask::PrevBlock,
            Mask::Match { dist: 77 },
            Mask::Match { dist: 2000 },
            Mask::AddDelta { idx: 0 },
            Mask::Lfsr { poly: 0, seed: 0x55 },
            Mask::TwoStage { dist: 5, len: 1, bits: 0b1001 },
        ];
        for mask in masks {
            for comp in [false, true] {
                let off = 1500;
                let n = 1023;
                let mut e = s.slice_from(off as i64, n);
                e.xor_assign(&mask.render(&s, &data, off, n, Repr::Linear, block));
                if comp {
                    e.not_assign();
                }
                let mut out = s.clone();
                for i in off..off + n {
                    out.set(i, false);
                }
                mask.reconstruct(&mut out, off, &e, comp, Repr::Linear, block).unwrap();
                assert_eq!(out, s, "{} comp={comp}", mask.name());

                let mut w = BitWriter::new();
                mask.write(&mut w);
                assert_eq!(w.bits(), mask.cost());
                let bytes = w.finish();
                assert_eq!(Mask::read(&mut BitReader::new(&bytes), Repr::Linear), Some(mask.clone()));
            }
        }
    }

    #[test]
    fn match_index_returns_most_recent_equal_windows() {
        let mut data = b"0123456789abcdef".repeat(8);
        data.extend_from_slice(b"XYZW0123456789ab");
        let s = bits_of(&data);
        let idx = MatchIndex::build(&s);
        let anchor = 8 * (data.len() - 12);
        let d = idx.distances(&s, anchor, 3);
        assert_eq!(d.first(), Some(&(8 * 16 + 8 * 4)));
    }
}
