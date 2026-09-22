//! Context-mixing codec with stride-aware prediction contexts.
//!
//! Bytes are coded MSB-first as binary decisions. Each decision mixes, in the
//! logistic domain, order-0..6 context models and up to four models whose
//! context is a *prediction* from earlier bytes: the byte one element back
//! (stride s), the linear extrapolation 2·B[i−s] − B[i−2s], the element delta
//! B[i−s] − B[i−2s], and a planar predictor B[i−s] + B[i−r] − B[i−r−s] over a
//! record length r. The prediction is used as context rather than subtracted,
//! so the mixer learns per byte position how far to trust it.
//!
//! Wire format: `[4B length][1B stride][2B record][1B model mask][coded bits]`.

use super::bwt_cm::{Decoder, Encoder};
use std::sync::OnceLock;

pub const MODEL_COLUMN: u8 = 1;
pub const MODEL_LINEAR: u8 = 2;
pub const MODEL_DELTA: u8 = 4;
pub const MODEL_PLANE: u8 = 8;
/// Expected-bit models for the active column/linear/plane predictors.
pub const MODEL_BITS: u8 = 16;
pub const ALL_MODELS: u8 = MODEL_COLUMN | MODEL_LINEAR | MODEL_DELTA | MODEL_PLANE | MODEL_BITS;

const HEADER: usize = 8;
const STRIDES: [usize; 8] = [1, 2, 3, 4, 6, 8, 12, 16];
const MAX_RECORD: usize = 4096;
/// Order-0, order-1 and bias inputs, up to 8 hashed models and 3 bit models.
const MAX_INPUTS: usize = 16;
/// Predictors whose expected bit is also a direct context: column, linear, plane.
const PREDICTORS: usize = 3;
/// Bit-model contexts per predictor: phase x bit position x agree x expected x error class.
const BIT_CONTEXTS: usize = PHASES * 8 * 2 * 2 * 5;
/// Positions inside an element that get their own mixer weights.
const PHASES: usize = 16;
const SLOT_LIMIT: u32 = 127;
/// Largest block the decoder accepts (the adaptive container uses 4 MiB).
const MAX_LEN: usize = 64 << 20;

// ---------------------------------------------------------------------------
// Logistic helpers (integer, so encoder and decoder agree on every platform)
// ---------------------------------------------------------------------------

/// 1 / (1 + e^-d) for d in 1/256 units, as a 12-bit probability.
fn squash(d: i32) -> i32 {
    const T: [i32; 33] = [
        1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348,
        3607, 3785, 3901, 3975, 4022, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
    ];
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (T[i] * (128 - w) + T[i + 1] * w + 64) >> 7
}

/// Inverse of `squash` over 12-bit probabilities.
fn stretch(p: i32) -> i32 {
    static TABLE: OnceLock<Vec<i16>> = OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = vec![2047i16; 4096];
        let mut pi = 0usize;
        for x in -2047..=2047 {
            let v = squash(x) as usize;
            for slot in t.iter_mut().take(v + 1).skip(pi) {
                *slot = x as i16;
            }
            pi = v + 1;
        }
        t
    });
    i32::from(t[p as usize])
}

/// Adaptive rate 1 / (n + 1.5) in 16-bit fixed point.
fn rate(n: u32) -> i64 {
    static TABLE: OnceLock<Vec<i64>> = OnceLock::new();
    TABLE.get_or_init(|| (0..1024).map(|n| 131072 / (2 * n + 3)).collect())[n as usize]
}

/// A slot holds P(1) in its high 22 bits and a hit count in its low 10.
const SLOT_INIT: u32 = 1 << 31;

#[inline]
fn slot_p12(slot: u32) -> i32 {
    (slot >> 20) as i32
}

#[inline]
fn slot_update(slot: &mut u32, bit: bool) {
    let n = *slot & 1023;
    let p = i64::from(*slot >> 10);
    let target = if bit { (1 << 22) - 1 } else { 0 };
    let p = p + (((target - p) * rate(n)) >> 16);
    *slot = ((p as u32) << 10) | if n < SLOT_LIMIT { n + 1 } else { SLOT_LIMIT };
}

struct Mixer {
    n: usize,
    weights: Vec<i32>,
    inputs: [i32; MAX_INPUTS],
    set: usize,
    pr: i32,
}

impl Mixer {
    fn new(n: usize, sets: usize) -> Self {
        Self { n, weights: vec![1 << 14; n * sets], inputs: [0; MAX_INPUTS], set: 0, pr: 2048 }
    }

    fn predict(&mut self, set: usize) -> i32 {
        self.set = set;
        let w = &self.weights[set * self.n..(set + 1) * self.n];
        let dot: i64 = w.iter().zip(&self.inputs).map(|(&w, &x)| i64::from(w) * i64::from(x)).sum();
        self.pr = squash((dot >> 16).clamp(-2047, 2047) as i32);
        self.pr
    }

    fn update(&mut self, bit: bool) {
        let err = (i32::from(bit) << 12) - self.pr;
        let w = &mut self.weights[self.set * self.n..(self.set + 1) * self.n];
        for (w, &x) in w.iter_mut().zip(&self.inputs) {
            *w += (x * err) >> 11;
        }
    }
}

/// Secondary estimation: refines a probability by context, interpolating
/// between 33 buckets over the stretched input.
struct Apm {
    t: Vec<u16>,
    index: usize,
}

impl Apm {
    fn new(contexts: usize) -> Self {
        let t = (0..contexts * 33).map(|k| (squash((k % 33) as i32 * 128 - 2048) * 16) as u16).collect();
        Self { t, index: 0 }
    }

    fn pp(&mut self, pr: i32, cx: usize) -> i32 {
        let s = (stretch(pr) + 2048) * 23;
        let wt = s & 0xFFF;
        let base = cx * 33 + (s >> 12) as usize;
        self.index = base + (wt >> 11) as usize;
        (i32::from(self.t[base]) * (4096 - wt) + i32::from(self.t[base + 1]) * wt) >> 16
    }

    fn update(&mut self, bit: bool) {
        const RATE: i32 = 7;
        let b = i32::from(bit);
        let g = (b << 16) + (b << RATE) - 2 * b;
        let t = &mut self.t[self.index];
        *t = (i32::from(*t) + ((g - i32::from(*t)) >> RATE)) as u16;
    }
}

// ---------------------------------------------------------------------------
// The model shared by encoder and decoder
// ---------------------------------------------------------------------------

fn hash(tag: u64, x: u64) -> usize {
    let h = (x ^ (tag << 56)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((h ^ (h >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 32) as usize
}

struct Model {
    stride: usize,
    record: usize,
    models: u8,
    o0: Vec<u32>,
    o1: Vec<u32>,
    table: Vec<u32>,
    mask: usize,
    bases: [usize; MAX_INPUTS],
    n_hashed: usize,
    slots: [usize; MAX_INPUTS],
    /// Per active predictor: predicted byte, prefix still agrees, error class.
    predicted: [u8; PREDICTORS],
    agree: [bool; PREDICTORS],
    err_class: [usize; PREDICTORS],
    active: [bool; PREDICTORS],
    bit_tables: Vec<u32>,
    bit_slots: [usize; PREDICTORS],
    mixer: Mixer,
    apm: Apm,
    c0: usize,
    c1: usize,
    bitpos: usize,
    phase: usize,
    pr: i32,
}

/// How wrong a predictor was on the previous element, in 5 classes.
fn error_class(actual: u8, predicted: u8) -> usize {
    match actual.abs_diff(predicted) {
        0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=15 => 3,
        _ => 4,
    }
}

impl Model {
    fn new(len: usize, stride: usize, record: usize, models: u8) -> Self {
        let bits = (usize::BITS - len.max(1).leading_zeros() + 3).clamp(16, 23);
        let n_hashed = 4 + (models & !MODEL_BITS).count_ones() as usize;
        let bits_on = models & MODEL_BITS != 0;
        let active = [
            bits_on && models & MODEL_COLUMN != 0,
            bits_on && models & MODEL_LINEAR != 0,
            bits_on && models & MODEL_PLANE != 0,
        ];
        let n_bit = active.iter().filter(|&&a| a).count();
        Self {
            stride,
            record,
            models,
            o0: vec![SLOT_INIT; 256],
            o1: vec![SLOT_INIT; 1 << 16],
            table: vec![SLOT_INIT; 1 << bits],
            mask: (1 << bits) - 1,
            bases: [0; MAX_INPUTS],
            n_hashed,
            slots: [0; MAX_INPUTS],
            predicted: [0; PREDICTORS],
            agree: [true; PREDICTORS],
            err_class: [0; PREDICTORS],
            active,
            bit_tables: vec![SLOT_INIT; PREDICTORS * BIT_CONTEXTS],
            bit_slots: [0; PREDICTORS],
            mixer: Mixer::new(3 + n_hashed + n_bit, PHASES * 256),
            apm: Apm::new(256),
            c0: 1,
            c1: 0,
            bitpos: 0,
            phase: 0,
            pr: 2048,
        }
    }

    /// Set the per-byte contexts for coding byte `i = hist.len()`.
    fn start_byte(&mut self, hist: &[u8]) {
        let i = hist.len();
        let at = |back: usize| -> u64 { if back >= 1 && back <= i { u64::from(hist[i - back]) } else { 0 } };
        let s = self.stride;
        let r = self.record;
        self.c0 = 1;
        self.bitpos = 0;
        self.agree = [true; PREDICTORS];
        self.c1 = at(1) as usize;
        self.phase = i % s;
        let phase = self.phase as u64;
        let mut k = 0;
        let mut push = |tag: u64, x: u64| {
            self.bases[k] = hash(tag, x);
            k += 1;
        };
        let (c1, c2, c3, c4, c5, c6) = (at(1), at(2), at(3), at(4), at(5), at(6));
        push(2, c1 | c2 << 8);
        push(3, c1 | c2 << 8 | c3 << 16);
        push(4, c1 | c2 << 8 | c3 << 16 | c4 << 24);
        push(6, c1 | c2 << 8 | c3 << 16 | c4 << 24 | c5 << 32 | c6 << 40);
        let (b1, b2) = (at(s), at(2 * s));
        if self.models & MODEL_COLUMN != 0 {
            push(10, b1 | phase << 8);
        }
        if self.models & MODEL_LINEAR != 0 {
            push(11, (2 * b1).wrapping_sub(b2) & 0xFF | phase << 8);
        }
        if self.models & MODEL_DELTA != 0 {
            push(12, b1.wrapping_sub(b2) & 0xFF | phase << 8 | c1 << 16);
        }
        if self.models & MODEL_PLANE != 0 {
            let planar = (b1 + at(r)).wrapping_sub(at(r + s)) & 0xFF;
            push(13, planar | phase << 8 | (at(r) >> 4) << 16);
        }

        // Predicted bytes for this position and for the previous element,
        // whose error says how much to trust the predictor now.
        let byte = |v: u64| v as u8;
        let column = |back: usize| at(back + s);
        let linear = |back: usize| (2 * at(back + s)).wrapping_sub(at(back + 2 * s));
        let plane = |back: usize| (at(back + s) + at(back + r)).wrapping_sub(at(back + r + s));
        let now = [byte(column(0)), byte(linear(0)), byte(plane(0))];
        let before = [byte(column(s)), byte(linear(s)), byte(plane(s))];
        let actual = byte(at(s));
        for p in 0..PREDICTORS {
            self.predicted[p] = now[p];
            self.err_class[p] = error_class(actual, before[p]);
        }
    }

    /// Probability of a 1 for the next bit, 16-bit for the coder.
    fn predict(&mut self) -> u32 {
        let c0 = self.c0;
        let inputs = &mut self.mixer.inputs;
        inputs[0] = stretch(slot_p12(self.o0[c0]));
        inputs[1] = stretch(slot_p12(self.o1[self.c1 << 8 | c0]));
        for j in 0..self.n_hashed {
            let slot = self.bases[j].wrapping_add(c0.wrapping_mul(0x9E37_79B1)) & self.mask;
            self.slots[j] = slot;
            inputs[2 + j] = stretch(slot_p12(self.table[slot]));
        }
        let mut k = 2 + self.n_hashed;
        for p in 0..PREDICTORS {
            if !self.active[p] {
                continue;
            }
            let expected = usize::from((self.predicted[p] >> (7 - self.bitpos)) & 1);
            let cx = ((((self.phase.min(PHASES - 1) * 8 + self.bitpos) * 2 + usize::from(self.agree[p])) * 2
                + expected)
                * 5)
                + self.err_class[p];
            self.bit_slots[p] = p * BIT_CONTEXTS + cx;
            inputs[k] = stretch(slot_p12(self.bit_tables[self.bit_slots[p]]));
            k += 1;
        }
        inputs[k] = 256;
        let mixed = self.mixer.predict(self.phase.min(PHASES - 1) << 8 | c0);
        // An order-1 APM stage measured worse here; one order-0 stage, lightly weighted.
        let refined = self.apm.pp(mixed, c0);
        self.pr = ((3 * mixed + refined + 2) >> 2).clamp(1, 4095);
        (self.pr as u32) << 4
    }

    fn update(&mut self, bit: bool) {
        let c0 = self.c0;
        slot_update(&mut self.o0[c0], bit);
        slot_update(&mut self.o1[self.c1 << 8 | c0], bit);
        for j in 0..self.n_hashed {
            slot_update(&mut self.table[self.slots[j]], bit);
        }
        for p in 0..PREDICTORS {
            if self.active[p] {
                slot_update(&mut self.bit_tables[self.bit_slots[p]], bit);
                let expected = (self.predicted[p] >> (7 - self.bitpos)) & 1 == 1;
                self.agree[p] &= expected == bit;
            }
        }
        self.mixer.update(bit);
        self.apm.update(bit);
        self.c0 = (c0 << 1) | usize::from(bit);
        self.bitpos += 1;
    }
}

// ---------------------------------------------------------------------------
// Parameter detection
// ---------------------------------------------------------------------------

/// Element stride whose linear extrapolation predicts bytes best.
pub fn detect_stride(data: &[u8]) -> usize {
    let sample = &data[..data.len().min(1 << 20)];
    let mut best = (u64::MAX, 1);
    for &s in &STRIDES {
        if sample.len() < 4 * s + 64 {
            continue;
        }
        let err: u64 = (2 * s..sample.len())
            .map(|i| {
                let d = sample[i].wrapping_sub(sample[i - s].wrapping_mul(2).wrapping_sub(sample[i - 2 * s]));
                u64::from(d.min(d.wrapping_neg()))
            })
            .sum();
        let mean = err * 1024 / (sample.len() - 2 * s) as u64;
        if mean < best.0 {
            best = (mean, s);
        }
    }
    best.1
}

/// Record length (row or record size) with the smallest mean byte distance.
pub fn detect_record(data: &[u8], stride: usize) -> usize {
    let lo = 2 * stride + 1;
    let hi = MAX_RECORD.min(data.len() / 4);
    if hi <= lo {
        return 0;
    }
    let positions: Vec<usize> = (hi..data.len().min(1 << 22)).step_by(97).take(8192).collect();
    if positions.len() < 64 {
        return 0;
    }
    (lo..=hi)
        .min_by_key(|&d| {
            positions.iter().map(|&i| u32::from(data[i].abs_diff(data[i - d]))).sum::<u32>()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Encode a block of at most 64 MiB with a subset of [`ALL_MODELS`]; both
/// bounds are what [`decode`] accepts.
pub fn encode_with(data: &[u8], models: u8) -> Vec<u8> {
    assert!(data.len() <= MAX_LEN, "stride-cm blocks are at most {MAX_LEN} bytes");
    assert_eq!(models & !ALL_MODELS, 0, "unknown stride-cm model bits");
    let stride = detect_stride(data);
    let record = if models & MODEL_PLANE != 0 { detect_record(data, stride) } else { 0 };
    let mut out = Vec::with_capacity(HEADER + data.len() / 2);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.push(stride as u8);
    out.extend_from_slice(&(record as u16).to_le_bytes());
    out.push(models);

    let mut model = Model::new(data.len(), stride, record, models);
    let mut enc = Encoder::new();
    for i in 0..data.len() {
        model.start_byte(&data[..i]);
        for k in (0..8).rev() {
            let bit = (data[i] >> k) & 1 == 1;
            enc.encode(bit, model.predict());
            model.update(bit);
        }
    }
    out.extend_from_slice(&enc.finish());
    out
}

pub fn encode(data: &[u8]) -> Vec<u8> {
    encode_with(data, ALL_MODELS)
}

/// Decode a payload from [`encode`]; `None` if the header is malformed.
pub fn decode(payload: &[u8]) -> Option<Vec<u8>> {
    let header = payload.get(..HEADER)?;
    let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    let stride = usize::from(header[4]);
    let record = usize::from(u16::from_le_bytes([header[5], header[6]]));
    let models = header[7];
    if len > MAX_LEN || !STRIDES.contains(&stride) || record > MAX_RECORD || models & !ALL_MODELS != 0 {
        return None;
    }

    let mut model = Model::new(len, stride, record, models);
    let mut dec = Decoder::new(&payload[HEADER..]);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        model.start_byte(&out);
        let mut byte = 0u8;
        for _ in 0..8 {
            let bit = dec.decode(model.predict());
            model.update(bit);
            byte = (byte << 1) | u8::from(bit);
        }
        out.push(byte);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "unknown stride-cm model bits")]
    fn encode_refuses_model_bits_the_decoder_rejects() {
        encode_with(&[], 32);
    }

    #[test]
    #[should_panic(expected = "stride-cm blocks are at most")]
    fn encode_refuses_blocks_the_decoder_rejects() {
        encode(&vec![0u8; MAX_LEN + 1]);
    }

    fn lcg(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state >> 33
    }

    fn samples() -> Vec<Vec<u8>> {
        let mut s = 9u64;
        vec![
            vec![],
            vec![42],
            b"The quick brown fox jumps over the lazy dog. ".repeat(40),
            (0..4000u32).flat_map(|i| (1000 + 7 * i).to_le_bytes()).collect(),
            (0..3000u32).flat_map(|i| ((20000.0 + 9000.0 * (i as f64 / 50.0).sin()) as u16).to_le_bytes()).collect(),
            (0..20000).map(|_| lcg(&mut s) as u8).collect(),
            (0..64 * 100).map(|i| ((i % 64) as u8).wrapping_mul(3) ^ ((i / 64) as u8)).collect(),
        ]
    }

    #[test]
    fn logistic_tables_are_inverse() {
        for p in (1..4095).step_by(7) {
            assert!((squash(stretch(p)) - p).abs() <= 40, "p={p}");
        }
        assert!((squash(0) - 2048).abs() <= 1);
    }

    #[test]
    fn roundtrip_every_model_set() {
        for data in samples() {
            for models in [0, MODEL_COLUMN, MODEL_LINEAR, MODEL_LINEAR | MODEL_PLANE, ALL_MODELS] {
                let out = encode_with(&data, models);
                assert_eq!(decode(&out), Some(data.clone()), "models={models} len={}", data.len());
            }
        }
    }

    #[test]
    fn linear_data_detects_its_stride_and_compresses() {
        let data: Vec<u8> = (0..16384u32).flat_map(|i| (3 * i + 17).to_le_bytes()).collect();
        assert_eq!(detect_stride(&data), 4);
        let out = encode(&data);
        assert!(out.len() < data.len() / 20, "{} bytes", out.len());
    }

    #[test]
    fn malformed_headers_are_rejected_and_truncation_never_panics() {
        let out = encode(&samples()[3]);
        for cut in 0..out.len().min(64) {
            let _ = decode(&out[..cut]);
        }
        let mut bad = out.clone();
        bad[4] = 5; // not a valid stride
        assert_eq!(decode(&bad), None);
    }
}
