//! Residual encoders for E = X xor M. Every codec reports the exact number
//! of bits it writes, so candidates are ranked by serialized size.

use super::bch;
use super::bits::{gamma_len, width_for, BitReader, BitVec, BitWriter};
use super::enumerative;
use crate::codecs::bwt_cm;
use crate::entropy::{build_decode_table, build_table, normalize_freqs, rans_decode, rans_encode};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Residual {
    /// The n bits verbatim.
    Raw = 0,
    /// Run count, first bit, then Elias-gamma run lengths (last run implied).
    Rle = 1,
    /// Weight, then each set position in ceil(log2 n) bits.
    SparsePos = 2,
    /// Weight, Rice parameter, then Rice-coded gaps between set positions.
    Rice = 3,
    /// Weight, then the rank among C(n, t) subsets.
    Enum = 4,
    /// Weight t, then the BCH syndrome E mod g_t (n = 2^m - 1 only).
    Bch = 5,
    /// Adaptive binary arithmetic coding with an order-1 bit context.
    Arith = 6,
    /// NEXCOMP's static rANS over residual bytes, with its frequency table.
    Rans = 7,
}

pub const ALL: [Residual; 8] = [
    Residual::Raw,
    Residual::Rle,
    Residual::SparsePos,
    Residual::Rice,
    Residual::Enum,
    Residual::Bch,
    Residual::Arith,
    Residual::Rans,
];

pub const ID_BITS: u32 = 3;
const RICE_K_BITS: u32 = 4;
/// Exact ranking costs big-integer work proportional to n (or t^2); keep it to
/// sparse or co-sparse residuals. Dense residuals get within ~1% of it with
/// `Arith` at a fraction of the time.
const ENUM_MAX_T: usize = 1024;
/// rANS frequency entries: 256 symbols, 13 bits each (values 0..=4096).
const RANS_FREQ_BITS: usize = 256 * 13;

impl Residual {
    pub fn from_id(id: u8) -> Option<Self> {
        ALL.get(id as usize).copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Residual::Raw => "raw",
            Residual::Rle => "rle",
            Residual::SparsePos => "positions",
            Residual::Rice => "rice",
            Residual::Enum => "enumerative",
            Residual::Bch => "bch",
            Residual::Arith => "arith",
            Residual::Rans => "rans",
        }
    }
}

/// Facts about one residual shared by the cost functions.
pub struct Stats {
    pub n: usize,
    pub t: usize,
    pub ones: Vec<u32>,
}

impl Stats {
    pub fn of(e: &BitVec) -> Self {
        Self { n: e.len, t: e.count_ones(), ones: e.ones() }
    }
}

fn rice_cost(ones: &[u32], k: u32) -> usize {
    let mut prev = -1i64;
    let mut bits = 0usize;
    for &p in ones {
        let gap = (i64::from(p) - prev - 1) as u64;
        bits += (gap >> k) as usize + 1 + k as usize;
        prev = i64::from(p);
    }
    bits
}

fn best_rice_k(ones: &[u32]) -> (u32, usize) {
    (0..1 << RICE_K_BITS)
        .map(|k| (k, rice_cost(ones, k)))
        .min_by_key(|&(_, c)| c)
        .unwrap()
}

/// Exact serialized size in bits, or `None` when the codec cannot represent `e`.
pub fn cost(codec: Residual, e: &BitVec, st: &Stats) -> Option<usize> {
    let (n, t) = (st.n, st.t);
    let weight = gamma_len(t as u64 + 1);
    Some(match codec {
        Residual::Raw => n,
        Residual::Rle => {
            let runs = e.runs();
            gamma_len(runs.len() as u64 + 1)
                + 1
                + runs[..runs.len().saturating_sub(1)]
                    .iter()
                    .map(|&r| gamma_len(u64::from(r)))
                    .sum::<usize>()
        }
        Residual::SparsePos => weight + t * width_for(n as u64) as usize,
        Residual::Rice => {
            weight + if t == 0 { 0 } else { RICE_K_BITS as usize + best_rice_k(&st.ones).1 }
        }
        Residual::Enum => {
            if t > ENUM_MAX_T && n - t > ENUM_MAX_T {
                return None;
            }
            weight + enumerative::rank_width(n, t)
        }
        Residual::Bch => {
            let code = bch::for_len(n)?;
            if t > code.max_t() {
                return None;
            }
            weight + code.syndrome_bits(t)?
        }
        Residual::Arith => {
            let bytes = arith_encode(e).len();
            gamma_len(bytes as u64 + 1) + 8 * bytes
        }
        Residual::Rans => {
            let bytes = rans_payload(e)?.len();
            RANS_FREQ_BITS + gamma_len(bytes as u64 + 1) + 8 * bytes
        }
    })
}

/// `Rans` cost from the order-0 entropy of the residual bytes (rANS lands
/// within a few bytes of it), for ranking.
pub fn rans_cost_estimate(e: &BitVec) -> usize {
    let mut counts = [0u32; 256];
    for (k, &w) in e.words.iter().enumerate() {
        let bytes = (e.len - 64 * k).min(64).div_ceil(8);
        for j in 0..bytes {
            counts[((w >> (8 * j)) & 0xFF) as usize] += 1;
        }
    }
    let total: u32 = counts.iter().sum();
    let bits: f64 = counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| -f64::from(c) * (f64::from(c) / f64::from(total)).log2())
        .sum();
    RANS_FREQ_BITS + gamma_len(u64::from(total) / 2 + 1) + bits as usize + 32
}

/// `Arith` cost from the ideal KT code length of each order-1 bit context
/// (the coder adds its 4-byte flush and byte rounding), for ranking.
pub fn arith_cost_estimate(e: &BitVec) -> usize {
    // counts[prev][bit] over the sequence, with prev = 0 before the first bit.
    let mut counts = [[0u64; 2]; 2];
    let mut carry = 0u64;
    for (k, &w) in e.words.iter().enumerate() {
        let valid = if 64 * (k + 1) <= e.len { u64::MAX } else { (1u64 << (e.len - 64 * k)) - 1 };
        let prev = (w << 1) | carry;
        carry = w >> 63;
        counts[0][1] += u64::from((w & !prev & valid).count_ones());
        counts[1][1] += u64::from((w & prev & valid).count_ones());
        counts[0][0] += u64::from((!w & !prev & valid).count_ones());
        counts[1][0] += u64::from((!w & prev & valid).count_ones());
    }
    let kt = |c: &[u64; 2]| -> f64 {
        let n = (c[0] + c[1]) as f64;
        if n == 0.0 {
            return 0.0;
        }
        let h = |x: f64| if x == 0.0 { 0.0 } else { -x * (x / n).log2() };
        h(c[0] as f64) + h(c[1] as f64) + 0.5 * (n + 1.0).log2() + 1.0
    };
    let bits = kt(&counts[0]) + kt(&counts[1]) + 32.0;
    let bytes = (bits / 8.0).ceil() as u64;
    gamma_len(bytes + 1) + 8 * bytes as usize
}

pub fn encode(codec: Residual, e: &BitVec, st: &Stats, w: &mut BitWriter) {
    let n = st.n;
    match codec {
        Residual::Raw => w.put_bits(e),
        Residual::Rle => {
            let runs = e.runs();
            w.put_gamma(runs.len() as u64 + 1);
            w.put_bit(n > 0 && e.get(0));
            for &r in &runs[..runs.len().saturating_sub(1)] {
                w.put_gamma(u64::from(r));
            }
        }
        Residual::SparsePos => {
            w.put_gamma(st.t as u64 + 1);
            let width = width_for(n as u64);
            for &p in &st.ones {
                w.put(u64::from(p), width);
            }
        }
        Residual::Rice => {
            w.put_gamma(st.t as u64 + 1);
            if st.t > 0 {
                let (k, _) = best_rice_k(&st.ones);
                w.put(u64::from(k), RICE_K_BITS);
                let mut prev = -1i64;
                for &p in &st.ones {
                    w.put_rice((i64::from(p) - prev - 1) as u64, k);
                    prev = i64::from(p);
                }
            }
        }
        Residual::Enum => {
            w.put_gamma(st.t as u64 + 1);
            let width = enumerative::rank_width(n, st.t);
            let r = enumerative::rank(e);
            for i in (0..width).rev() {
                w.put_bit(r.bit(i));
            }
        }
        Residual::Bch => {
            w.put_gamma(st.t as u64 + 1);
            let code = bch::for_len(n).expect("bch only offered for n = 2^m - 1");
            w.put_bits(&code.syndrome(e, st.t));
        }
        Residual::Arith => {
            let bytes = arith_encode(e);
            w.put_gamma(bytes.len() as u64 + 1);
            w.put_bytes(&bytes);
        }
        Residual::Rans => {
            let payload = rans_payload(e).expect("rans cost was computed");
            for f in rans_freqs(e) {
                w.put(u64::from(f), 13);
            }
            w.put_gamma(payload.len() as u64 + 1);
            w.put_bytes(&payload);
        }
    }
}

pub fn decode(codec: Residual, n: usize, r: &mut BitReader) -> Option<BitVec> {
    match codec {
        Residual::Raw => r.get_bits(n),
        Residual::Rle => {
            let k = (r.get_gamma()? - 1) as usize;
            let mut value = r.get_bit()?;
            let mut e = BitVec::zeros(n);
            let mut pos = 0usize;
            for i in 0..k {
                let run = if i + 1 < k { r.get_gamma()? as usize } else { n.checked_sub(pos)? };
                if run == 0 || pos + run > n {
                    return None;
                }
                if value {
                    for j in pos..pos + run {
                        e.set(j, true);
                    }
                }
                pos += run;
                value = !value;
            }
            (pos == n).then_some(e)
        }
        Residual::SparsePos => {
            let t = (r.get_gamma()? - 1) as usize;
            let width = width_for(n as u64);
            let mut e = BitVec::zeros(n);
            let mut prev: Option<usize> = None;
            for _ in 0..t {
                let p = r.get(width)? as usize;
                if p >= n || prev.is_some_and(|q| p <= q) {
                    return None;
                }
                e.set(p, true);
                prev = Some(p);
            }
            Some(e)
        }
        Residual::Rice => {
            let t = (r.get_gamma()? - 1) as usize;
            let mut e = BitVec::zeros(n);
            if t == 0 {
                return Some(e);
            }
            let k = r.get(RICE_K_BITS)? as u32;
            let mut pos = -1i64;
            for _ in 0..t {
                pos += r.get_rice(k)? as i64 + 1;
                if pos as usize >= n {
                    return None;
                }
                e.set(pos as usize, true);
            }
            Some(e)
        }
        Residual::Enum => {
            let t = (r.get_gamma()? - 1) as usize;
            if t > n {
                return None;
            }
            let width = enumerative::rank_width(n, t);
            let bits: Option<Vec<bool>> = (0..width).map(|_| r.get_bit()).collect();
            let rank = enumerative::Big::from_bits_msb_first(bits?.into_iter());
            enumerative::unrank(&rank, n, t)
        }
        Residual::Bch => {
            let t = (r.get_gamma()? - 1) as usize;
            let code = bch::for_len(n)?;
            let syn = r.get_bits(code.syndrome_bits(t)?)?;
            code.decode(&syn, t)
        }
        Residual::Arith => {
            let len = (r.get_gamma()? - 1) as usize;
            Some(arith_decode(&r.get_bytes(len)?, n))
        }
        Residual::Rans => {
            let freqs: Option<Vec<u32>> = (0..256).map(|_| r.get(13).map(|f| f as u32)).collect();
            let len = (r.get_gamma()? - 1) as usize;
            let payload = r.get_bytes(len)?;
            let table = build_table(&freqs?).ok()?;
            let bytes = rans_decode(&payload, &build_decode_table(&table), n.div_ceil(8)).ok()?;
            Some(bytes_to_bits(&bytes, n))
        }
    }
}

// --- adaptive binary arithmetic coding (order-1 bit context, KT counts) ---

#[inline]
fn kt_p1(counts: &[u32; 2]) -> u32 {
    // P(1) = (n1 + 1/2) / (n0 + n1 + 1), in 16-bit fixed point.
    let num = (2 * u64::from(counts[1]) + 1) << 16;
    let den = 2 * (u64::from(counts[0]) + u64::from(counts[1])) + 2;
    ((num / den) as u32).clamp(1, 65535)
}

fn arith_encode(e: &BitVec) -> Vec<u8> {
    let mut enc = bwt_cm::Encoder::new();
    let mut counts = [[0u32; 2]; 2];
    let mut prev = 0usize;
    for i in 0..e.len {
        let bit = e.get(i);
        enc.encode(bit, kt_p1(&counts[prev]));
        counts[prev][usize::from(bit)] += 1;
        prev = usize::from(bit);
    }
    enc.finish()
}

fn arith_decode(data: &[u8], n: usize) -> BitVec {
    let mut dec = bwt_cm::Decoder::new(data);
    let mut counts = [[0u32; 2]; 2];
    let mut prev = 0usize;
    let mut e = BitVec::zeros(n);
    for i in 0..n {
        let bit = dec.decode(kt_p1(&counts[prev]));
        e.set(i, bit);
        counts[prev][usize::from(bit)] += 1;
        prev = usize::from(bit);
    }
    e
}

// --- static rANS over residual bytes (existing NEXCOMP entropy coder) ---

fn bits_to_bytes(e: &BitVec) -> Vec<u8> {
    (0..e.len.div_ceil(8))
        .map(|b| (0..8).fold(0u8, |acc, j| {
            let i = 8 * b + j;
            (acc << 1) | u8::from(i < e.len && e.get(i))
        }))
        .collect()
}

fn bytes_to_bits(bytes: &[u8], n: usize) -> BitVec {
    let mut e = BitVec::zeros(n);
    for i in 0..n {
        e.set(i, (bytes[i / 8] >> (7 - i % 8)) & 1 == 1);
    }
    e
}

fn rans_freqs(e: &BitVec) -> Vec<u32> {
    let mut counts = vec![0u64; 256];
    for b in bits_to_bytes(e) {
        counts[b as usize] += 1;
    }
    normalize_freqs(&counts, 256)
}

fn rans_payload(e: &BitVec) -> Option<Vec<u8>> {
    if e.len == 0 {
        return None;
    }
    let table = build_table(&rans_freqs(e)).ok()?;
    rans_encode(&bits_to_bytes(e), &table).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state >> 33
    }

    fn residual_with_weight(n: usize, t: usize, s: &mut u64) -> BitVec {
        let mut e = BitVec::zeros(n);
        let mut placed = 0;
        while placed < t {
            let p = (lcg(s) % n as u64) as usize;
            if !e.get(p) {
                e.set(p, true);
                placed += 1;
            }
        }
        e
    }

    #[test]
    fn every_codec_roundtrips_and_costs_what_it_writes() {
        let mut s = 3u64;
        for &(n, t) in &[(255usize, 0usize), (255, 1), (1023, 2), (1023, 8), (1023, 128), (2048, 900), (8191, 40), (4096, 4096), (777, 300)] {
            let e = residual_with_weight(n, t, &mut s);
            let st = Stats::of(&e);
            for codec in ALL {
                let Some(c) = cost(codec, &e, &st) else { continue };
                let mut w = BitWriter::new();
                w.put_bit(true); // misalign on purpose
                encode(codec, &e, &st, &mut w);
                assert_eq!(w.bits(), 1 + c, "{} n={n} t={t}", codec.name());
                let bytes = w.finish();
                let mut r = BitReader::new(&bytes);
                r.get_bit();
                assert_eq!(decode(codec, n, &mut r), Some(e.clone()), "{} n={n} t={t}", codec.name());
            }
        }
    }

    #[test]
    fn ranking_estimates_track_exact_costs() {
        let mut s = 21u64;
        for &(n, t) in &[(1023usize, 30usize), (8192, 400), (8192, 2500), (32768, 9000), (32768, 30000)] {
            let e = residual_with_weight(n, t, &mut s);
            let st = Stats::of(&e);
            for (codec, est) in [(Residual::Arith, arith_cost_estimate(&e)), (Residual::Rans, rans_cost_estimate(&e))] {
                let exact = cost(codec, &e, &st).unwrap() as f64;
                assert!((est as f64 - exact).abs() <= 0.02 * exact + 64.0, "{} n={n} t={t}: est {est} exact {exact}", codec.name());
            }
        }
    }

    #[test]
    fn bch_is_offered_up_to_its_design_limit_only() {
        let mut s = 11u64;
        let code = bch::for_len(1023).unwrap();
        let max = code.max_t();
        let at_max = residual_with_weight(1023, max, &mut s);
        assert!(cost(Residual::Bch, &at_max, &Stats::of(&at_max)).is_some());
        let beyond = residual_with_weight(1023, max + 1, &mut s);
        assert!(cost(Residual::Bch, &beyond, &Stats::of(&beyond)).is_none());
        assert!(cost(Residual::Bch, &BitVec::zeros(1024), &Stats::of(&BitVec::zeros(1024))).is_none());
    }
}
