//! MinMask (research codec): minimum-description mask + sparse XOR residual.
//!
//! The input is viewed as one bitstream (linear or bit planes) cut into
//! blocks of n bits. For each block X the encoder searches masks M it can
//! describe cheaply, forms E = X xor M (optionally complemented) and codes E
//! with the residual codec that writes the fewest bits. Every choice is
//! ranked by its complete serialized size: mask description, complement flag,
//! residual codec id and residual payload.
//!
//! Wire format:
//! ```text
//! [4B] original length (u32 LE)
//! [1B] config: bit 7 = layout (0 linear, 1 planes), bits 0..3 = block size id
//! bitstream, per block: mask | complement (1b) | residual id (3b) | residual
//! ```

pub mod bch;
pub mod bits;
pub mod enumerative;
pub mod masks;
pub mod residual;

use bits::{BitReader, BitVec, BitWriter};
use masks::{Mask, MatchIndex, Repr, BM_MAX_BITS, LFSR_POLYS, PATTERN_LENS, STRIDES};
use rayon::prelude::*;
use residual::Residual;
use thiserror::Error;

/// Block sizes in bits: BCH-compatible 2^m - 1, then byte-aligned 256 B..64 KiB.
pub const BLOCK_BITS: [usize; 12] = [255, 511, 1023, 2047, 4095, 8191, 2048, 4096, 8192, 32768, 131072, 524288];
const HEADER_BYTES: usize = 5;
/// Largest input accepted by the decoder (the adaptive container uses 4 MiB).
const MAX_INPUT: usize = 64 << 20;
/// Masks whose `arith` and `rans` costs are computed exactly.
const FINALISTS: usize = 3;

#[derive(Debug, Error)]
pub enum MinMaskError {
    #[error("truncated or malformed MinMask payload")]
    Malformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Search {
    Fast,
    Exhaustive,
}

/// Which configurations, residual codecs and mask search to try.
#[derive(Clone, Debug)]
pub struct Options {
    pub reprs: Vec<Repr>,
    pub block_ids: Vec<usize>,
    pub residuals: Vec<Residual>,
    pub search: Search,
}

impl Options {
    pub fn fast() -> Self {
        Self {
            reprs: vec![Repr::Linear],
            block_ids: vec![2, 8, 9],
            residuals: residual::ALL.to_vec(),
            search: Search::Fast,
        }
    }

    pub fn exhaustive() -> Self {
        Self {
            reprs: vec![Repr::Linear, Repr::Planes],
            block_ids: (0..BLOCK_BITS.len()).collect(),
            residuals: residual::ALL.to_vec(),
            search: Search::Exhaustive,
        }
    }
}

/// What the encoder chose for one block, with its exact bit accounting.
#[derive(Clone, Debug)]
pub struct BlockReport {
    pub n: usize,
    pub family: &'static str,
    pub mask_bits: usize,
    pub complement: bool,
    pub residual: Residual,
    pub weight: usize,
    pub residual_bits: usize,
    /// log2 C(n, weight): bits to name the flips if n and t were free.
    pub sparse_floor: f64,
    /// Candidate masks evaluated.
    pub candidates: usize,
}

impl BlockReport {
    pub fn total_bits(&self) -> usize {
        self.mask_bits + 1 + residual::ID_BITS as usize + self.residual_bits
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub repr: Repr,
    pub block_bits: usize,
    pub bytes: usize,
    pub blocks: Vec<BlockReport>,
}

/// Linear or bit-plane bitstream of `data`.
pub fn to_bits(data: &[u8], repr: Repr) -> BitVec {
    let mut s = BitVec::zeros(8 * data.len());
    match repr {
        Repr::Linear => {
            for (k, chunk) in data.chunks(8).enumerate() {
                let mut word = 0u64;
                for (j, &b) in chunk.iter().enumerate() {
                    word |= u64::from(b.reverse_bits()) << (8 * j);
                }
                s.words[k] = word;
            }
        }
        Repr::Planes => {
            let len = data.len();
            for (i, &b) in data.iter().enumerate() {
                for p in 0..8 {
                    if (b >> p) & 1 == 1 {
                        s.set(p * len + i, true);
                    }
                }
            }
        }
    }
    s
}

pub fn from_bits(s: &BitVec, repr: Repr, len: usize) -> Vec<u8> {
    match repr {
        Repr::Linear => (0..len)
            .map(|i| ((s.words[i / 8] >> (8 * (i % 8))) as u8).reverse_bits())
            .collect(),
        Repr::Planes => (0..len)
            .map(|i| (0..8).fold(0u8, |acc, p| acc | (u8::from(s.get(p * len + i)) << p)))
            .collect(),
    }
}

struct Choice {
    mask: Mask,
    complement: bool,
    residual: Residual,
    e: BitVec,
    report: BlockReport,
}

struct Scored {
    total: usize,
    mask: Mask,
    complement: bool,
    residual: Residual,
    e: BitVec,
}

/// Masks to try for one block.
fn candidates(
    s: &BitVec,
    x: &BitVec,
    off: usize,
    repr: Repr,
    search: Search,
    index: Option<&MatchIndex>,
) -> Vec<Mask> {
    let exhaustive = search == Search::Exhaustive;
    let n = x.len;
    let mut out = vec![Mask::Zero, Mask::PrevBlock];

    let pattern_lens: &[usize] = if exhaustive { &[0, 1, 2, 3, 4, 5] } else { &[0, 2, 4] };
    for &len in pattern_lens {
        out.push(Mask::Pattern { len: len as u8, bits: masks::majority_pattern(x, PATTERN_LENS[len]) });
    }
    let strides = if exhaustive { STRIDES.len() } else { 3 };
    for idx in 0..strides {
        out.push(Mask::Stride { idx: idx as u8 });
        if repr == Repr::Linear && (exhaustive || idx == 0) {
            out.push(Mask::AddDelta { idx: idx as u8 });
        }
    }
    if let Some(index) = index {
        let (anchors, k) = if exhaustive { (vec![0, n / 2 / 8 * 8], 16) } else { (vec![0], 2) };
        let mut seen = Vec::new();
        for a in anchors {
            for d in index.distances(s, off + a, k) {
                if !seen.contains(&d) {
                    seen.push(d);
                    out.push(Mask::Match { dist: d });
                }
            }
        }
    }
    if exhaustive {
        for (poly, &(a, _)) in LFSR_POLYS.iter().enumerate() {
            out.push(Mask::Lfsr { poly: poly as u8, seed: masks::lfsr_seed(x, a) });
        }
        if n <= BM_MAX_BITS {
            let (conn, seed) = masks::bm_fit(x);
            if 2 * conn.len < n {
                out.push(Mask::BmLfsr { conn, seed });
            }
        }
    }
    out
}

/// Exact cost, or in the ranking stage an entropy estimate for the codecs
/// whose exact cost needs a full encode (only used to pick finalists).
fn residual_cost(codec: Residual, e: &BitVec, st: &residual::Stats, exact: bool) -> Option<usize> {
    if exact {
        return residual::cost(codec, e, st);
    }
    match codec {
        Residual::Enum => residual::cost(codec, e, st),
        Residual::Arith => Some(residual::arith_cost_estimate(e)),
        Residual::Rans => (e.len > 0).then(|| residual::rans_cost_estimate(e)),
        _ => residual::cost(codec, e, st),
    }
}

/// Best (mask, complement, residual) for the block at `off`, by exact size.
#[allow(clippy::too_many_arguments)]
fn best_choice(
    s: &BitVec,
    data: &[u8],
    off: usize,
    n: usize,
    repr: Repr,
    block_bits: usize,
    opts: &Options,
    index: Option<&MatchIndex>,
) -> Choice {
    let x = s.slice_from(off as i64, n);
    let mut cands = candidates(s, &x, off, repr, opts.search, index);
    // Raw is always available as the lossless fallback.
    let mut codecs = opts.residuals.clone();
    if !codecs.contains(&Residual::Raw) {
        codecs.push(Residual::Raw);
    }

    let score = |mask: &Mask, exact: bool| -> Vec<Scored> {
        let mut e = x.clone();
        e.xor_assign(&mask.render(s, data, off, n, repr, block_bits));
        let mut out = Vec::new();
        for complement in [false, true] {
            let mut e = e.clone();
            if complement {
                e.not_assign();
            }
            let st = residual::Stats::of(&e);
            if let Some((c, codec)) = codecs
                .iter()
                .filter_map(|&codec| residual_cost(codec, &e, &st, exact).map(|c| (c, codec)))
                .min()
            {
                out.push(Scored { total: mask.cost() + 1 + residual::ID_BITS as usize + c, mask: mask.clone(), complement, residual: codec, e });
            }
        }
        out
    };

    // Stage 1: every mask with the cheap codecs (enum estimated).
    let mut stage1: Vec<Scored> = cands.iter().flat_map(|m| score(m, false)).collect();
    stage1.sort_by_key(|sc| sc.total);

    // Two-stage masks refine the best copy-type masks with a pattern.
    if opts.search == Search::Exhaustive {
        let mut extra = Vec::new();
        for sc in stage1.iter().take(FINALISTS) {
            let dist = match sc.mask {
                Mask::Match { dist } => dist,
                Mask::PrevBlock => block_bits as u64,
                Mask::Stride { idx } => (STRIDES[idx as usize] * repr.byte_step()) as u64,
                _ => continue,
            };
            let base = Mask::Match { dist }.render(s, data, off, n, repr, block_bits);
            let mut rest = x.clone();
            rest.xor_assign(&base);
            for len in [2u8, 4, 5] {
                let bits = masks::majority_pattern(&rest, PATTERN_LENS[len as usize]);
                extra.push(Mask::TwoStage { dist, len, bits });
            }
        }
        for m in &extra {
            stage1.extend(score(m, false));
        }
        stage1.sort_by_key(|sc| sc.total);
        cands.extend(extra);
    }

    // Stage 2: exact costs for all codecs on the finalist masks.
    let mut finalists: Vec<Mask> = Vec::new();
    for sc in &stage1 {
        if !finalists.contains(&sc.mask) {
            finalists.push(sc.mask.clone());
        }
        if finalists.len() == FINALISTS {
            break;
        }
    }
    let best = finalists.iter().flat_map(|m| score(m, true)).min_by_key(|sc| sc.total).expect("raw always applies");

    let st = residual::Stats::of(&best.e);
    let residual_bits = residual::cost(best.residual, &best.e, &st).expect("chosen codec applies");
    Choice {
        report: BlockReport {
            n,
            family: best.mask.name(),
            mask_bits: best.mask.cost(),
            complement: best.complement,
            residual: best.residual,
            weight: st.t,
            residual_bits,
            sparse_floor: enumerative::log2_binom(n, st.t),
            candidates: cands.len(),
        },
        mask: best.mask,
        complement: best.complement,
        residual: best.residual,
        e: best.e,
    }
}

/// Encode with one fixed configuration.
pub fn compress_config(data: &[u8], repr: Repr, block_id: usize, opts: &Options) -> (Vec<u8>, Report) {
    let block_bits = BLOCK_BITS[block_id];
    let s = to_bits(data, repr);
    let index = MatchIndex::build(&s);
    let starts: Vec<usize> = (0..s.len).step_by(block_bits).collect();
    let choices: Vec<Choice> = starts
        .par_iter()
        .map(|&off| best_choice(&s, data, off, block_bits.min(s.len - off), repr, block_bits, opts, Some(&index)))
        .collect();

    let mut out = Vec::with_capacity(HEADER_BYTES + data.len() / 2);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.push(((repr as u8) << 7) | block_id as u8);
    let records: Vec<BitWriter> = choices
        .par_iter()
        .map(|ch| {
            let mut w = BitWriter::new();
            ch.mask.write(&mut w);
            w.put_bit(ch.complement);
            w.put(ch.residual as u64, residual::ID_BITS);
            residual::encode(ch.residual, &ch.e, &residual::Stats::of(&ch.e), &mut w);
            w
        })
        .collect();
    let mut w = BitWriter::new();
    for record in records {
        w.append(record);
    }
    out.extend_from_slice(&w.finish());
    let report = Report {
        repr,
        block_bits,
        bytes: out.len(),
        blocks: choices.into_iter().map(|c| c.report).collect(),
    };
    (out, report)
}

/// Try every configuration in `opts`, keep the smallest output.
pub fn compress_report(data: &[u8], opts: &Options) -> (Vec<u8>, Report) {
    let mut best: Option<(Vec<u8>, Report)> = None;
    for &repr in &opts.reprs {
        for &block_id in &opts.block_ids {
            let (out, report) = compress_config(data, repr, block_id, opts);
            if best.as_ref().map_or(true, |(b, _)| out.len() < b.len()) {
                best = Some((out, report));
            }
        }
    }
    best.expect("at least one configuration")
}

pub fn compress(data: &[u8], search: Search) -> Vec<u8> {
    let opts = match search {
        Search::Fast => Options::fast(),
        Search::Exhaustive => Options::exhaustive(),
    };
    compress_report(data, &opts).0
}

pub fn decompress(payload: &[u8]) -> Result<Vec<u8>, MinMaskError> {
    let header = payload.get(..HEADER_BYTES).ok_or(MinMaskError::Malformed)?;
    let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    let repr = if header[4] >> 7 == 0 { Repr::Linear } else { Repr::Planes };
    let block_bits = *BLOCK_BITS.get((header[4] & 0x0F) as usize).ok_or(MinMaskError::Malformed)?;
    if len > MAX_INPUT || header[4] & 0x70 != 0 {
        return Err(MinMaskError::Malformed);
    }

    let total = 8 * len;
    let mut s = BitVec::zeros(total);
    let mut r = BitReader::new(&payload[HEADER_BYTES..]);
    for off in (0..total).step_by(block_bits) {
        let n = block_bits.min(total - off);
        let mask = Mask::read(&mut r, repr).ok_or(MinMaskError::Malformed)?;
        let complement = r.get_bit().ok_or(MinMaskError::Malformed)?;
        let codec = r
            .get(residual::ID_BITS)
            .and_then(|id| Residual::from_id(id as u8))
            .ok_or(MinMaskError::Malformed)?;
        let e = residual::decode(codec, n, &mut r).ok_or(MinMaskError::Malformed)?;
        if let Mask::BmLfsr { conn, .. } = &mask {
            if conn.len > n {
                return Err(MinMaskError::Malformed);
            }
        }
        mask.reconstruct(&mut s, off, &e, complement, repr, block_bits)
            .ok_or(MinMaskError::Malformed)?;
    }
    Ok(from_bits(&s, repr, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state >> 33
    }

    fn samples() -> Vec<(&'static str, Vec<u8>)> {
        let mut s = 1u64;
        let random: Vec<u8> = (0..3000).map(|_| lcg(&mut s) as u8).collect();
        let mut flips = b"previous block with sparse flips! ".repeat(60);
        for _ in 0..12 {
            let p = (lcg(&mut s) % (8 * flips.len() as u64)) as usize;
            flips[p / 8] ^= 1 << (p % 8);
        }
        let numeric: Vec<u8> = (0..800u32).flat_map(|i| (1000 + 3 * i + i / 7).to_le_bytes()).collect();
        vec![
            ("empty", vec![]),
            ("one byte", vec![0xA5]),
            ("zeros", vec![0; 4000]),
            ("ones", vec![0xFF; 4000]),
            ("alternating", vec![0x55; 3000]),
            ("periodic", (0..5000u32).map(|i| (i % 37) as u8).collect()),
            ("sparse flips", flips),
            ("numeric", numeric),
            ("text", b"The quick brown fox jumps over the lazy dog. ".repeat(50)),
            ("random", random),
        ]
    }

    #[test]
    fn layouts_are_reversible() {
        for (_, data) in samples() {
            for repr in [Repr::Linear, Repr::Planes] {
                assert_eq!(from_bits(&to_bits(&data, repr), repr, data.len()), data);
            }
        }
    }

    #[test]
    fn roundtrip_every_layout_and_block_size() {
        for (name, data) in samples() {
            for repr in [Repr::Linear, Repr::Planes] {
                for block_id in 0..BLOCK_BITS.len() {
                    for opts in [Options::fast(), Options::exhaustive()] {
                        let (out, report) = compress_config(&data, repr, block_id, &opts);
                        assert_eq!(report.bytes, out.len());
                        assert_eq!(decompress(&out).expect(name), data, "{name} {repr:?} block {block_id}");
                    }
                }
            }
        }
    }

    #[test]
    fn reported_bits_match_serialized_size() {
        for (name, data) in samples() {
            let (out, report) = compress_report(&data, &Options::exhaustive());
            let bits: usize = report.blocks.iter().map(BlockReport::total_bits).sum();
            assert_eq!(out.len(), HEADER_BYTES + bits.div_ceil(8), "{name}");
        }
    }

    #[test]
    fn fuzz_roundtrip_thousands_of_blocks() {
        let mut s = 42u64;
        for case in 0..2000 {
            let len = (lcg(&mut s) % 700) as usize;
            let kind = case % 4;
            let data: Vec<u8> = (0..len)
                .map(|i| match kind {
                    0 => lcg(&mut s) as u8,
                    1 => (i % 5) as u8 ^ u8::from(lcg(&mut s) % 50 == 0),
                    2 => (i / 3) as u8,
                    _ => if lcg(&mut s) % 9 == 0 { lcg(&mut s) as u8 } else { 0 },
                })
                .collect();
            let repr = if case % 2 == 0 { Repr::Linear } else { Repr::Planes };
            let block_id = (lcg(&mut s) % BLOCK_BITS.len() as u64) as usize;
            let (out, _) = compress_config(&data, repr, block_id, &Options::fast());
            assert_eq!(decompress(&out).unwrap(), data, "case {case}");
        }
    }

    #[test]
    fn exact_predictors_leave_empty_residuals() {
        let periodic: Vec<u8> = vec![0b1100_1010; 4096];
        let (_, report) = compress_report(&periodic, &Options::exhaustive());
        assert!(report.blocks.iter().all(|b| b.weight == 0));
        assert!(report.bytes < 64, "periodic 4 KiB -> {} bytes", report.bytes);
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        let (out, _) = compress_config(b"some input bytes here", Repr::Linear, 2, &Options::fast());
        for cut in 0..out.len() {
            let _ = decompress(&out[..cut]);
        }
        assert!(decompress(&[1, 0, 0, 0, 0x7F]).is_err());
    }
}
