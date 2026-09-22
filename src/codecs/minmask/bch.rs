//! Binary primitive BCH syndrome coding, n = 2^m - 1 for m = 8..=13.
//!
//! A residual E of weight t is described by t and r(x) = E(x) mod g_t(x),
//! where g_t is the generator of the t-error-correcting BCH code, so r takes
//! exactly deg(g_t) bits. Every E of weight <= t is recovered from r with
//! Berlekamp-Massey and a Chien search. A pattern heavier than t cannot be
//! recovered, so the encoder only offers BCH with t = weight(E).

use super::bits::BitVec;
use std::sync::OnceLock;

/// Primitive polynomials over GF(2), indexed by m.
const PRIM_POLY: [u32; 14] = [0, 0, 0, 0, 0, 0, 0, 0, 0x11D, 0x211, 0x409, 0x805, 0x1053, 0x201B];
pub const MIN_M: u32 = 8;
pub const MAX_M: u32 = 13;
/// Largest designed t offered per code.
pub const MAX_T: usize = 256;

pub struct Gf {
    pub m: u32,
    pub n: usize,
    exp: Vec<u16>,
    log: Vec<u16>,
}

impl Gf {
    pub fn new(m: u32) -> Self {
        let n = (1usize << m) - 1;
        let mut exp = vec![0u16; 2 * n];
        let mut log = vec![0u16; n + 1];
        let mut x = 1u32;
        for (i, e) in exp.iter_mut().take(n).enumerate() {
            *e = x as u16;
            log[x as usize] = i as u16;
            x <<= 1;
            if x & (1 << m) != 0 {
                x ^= PRIM_POLY[m as usize];
            }
        }
        exp.copy_within(0..n, n);
        Self { m, n, exp, log }
    }

    #[inline]
    fn mul(&self, a: u16, b: u16) -> u16 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }

    #[inline]
    fn div(&self, a: u16, b: u16) -> u16 {
        if a == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.n - self.log[b as usize] as usize]
        }
    }

    #[inline]
    fn alpha_pow(&self, e: usize) -> u16 {
        self.exp[e % self.n]
    }
}

/// One BCH family (fixed m): field plus generator polynomials by designed t.
pub struct Bch {
    pub gf: Gf,
    /// `generators[t]` = g_t(x) as a GF(2) bit polynomial (bit k = coeff of x^k).
    generators: Vec<BitVec>,
}

impl Bch {
    fn new(m: u32) -> Self {
        let gf = Gf::new(m);
        let n = gf.n;
        let mut marked = vec![false; n];
        let mut g = BitVec::zeros(1);
        g.set(0, true);
        let mut generators = vec![g.clone()];
        // The BCH bound (distance >= 2t + 1) needs the roots alpha^1..alpha^2t
        // to be distinct, so t stops at (n - 1) / 2.
        for t in (1..=MAX_T).take_while(|&t| 2 * t < n) {
            for i in [2 * t - 1, 2 * t] {
                if !marked[i] {
                    let coset = cyclotomic_coset(i, n);
                    for &j in &coset {
                        marked[j] = true;
                    }
                    g = gf2_mul(&g, &minimal_poly(&gf, &coset));
                }
            }
            generators.push(g.clone());
        }
        Self { gf, generators }
    }

    pub fn max_t(&self) -> usize {
        self.generators.len() - 1
    }

    /// Syndrome size in bits for designed t (= deg g_t).
    pub fn syndrome_bits(&self, t: usize) -> Option<usize> {
        self.generators.get(t).map(|g| g.len - 1)
    }

    /// r(x) = E(x) mod g_t(x), exactly `syndrome_bits(t)` bits.
    pub fn syndrome(&self, e: &BitVec, t: usize) -> BitVec {
        let g = &self.generators[t];
        let deg = g.len - 1;
        let mut r = e.clone();
        for p in (deg..e.len).rev() {
            if r.get(p) {
                xor_shifted(&mut r, g, p - deg);
            }
        }
        r.slice_from(0, deg)
    }

    /// Recover the unique E of weight <= t with syndrome `r`.
    pub fn decode(&self, r: &BitVec, t: usize) -> Option<BitVec> {
        let gf = &self.gf;
        let n = gf.n;
        let mut e = BitVec::zeros(n);
        if t == 0 {
            return r.ones().is_empty().then_some(e);
        }
        let set: Vec<usize> = r.ones().into_iter().map(|p| p as usize).collect();
        // S_j = r(alpha^j) = E(alpha^j), j = 1..=2t
        let syn: Vec<u16> = (1..=2 * t)
            .map(|j| set.iter().fold(0u16, |acc, &p| acc ^ gf.alpha_pow(j * p)))
            .collect();

        // Berlekamp-Massey over GF(2^m)
        let mut c = vec![1u16];
        let mut b = vec![1u16];
        let mut l = 0usize;
        let mut shift = 1usize;
        let mut bd = 1u16;
        for k in 0..2 * t {
            let mut d = syn[k];
            for i in 1..=l.min(c.len() - 1) {
                d ^= gf.mul(c[i], syn[k - i]);
            }
            if d == 0 {
                shift += 1;
                continue;
            }
            let coef = gf.div(d, bd);
            let prev = c.clone();
            if c.len() < b.len() + shift {
                c.resize(b.len() + shift, 0);
            }
            for (i, &bi) in b.iter().enumerate() {
                c[i + shift] ^= gf.mul(coef, bi);
            }
            if 2 * l <= k {
                l = k + 1 - l;
                b = prev;
                bd = d;
                shift = 1;
            } else {
                shift += 1;
            }
        }
        if l > t {
            return None;
        }

        // Chien search: E has an error at p iff Lambda(alpha^-p) = 0
        let mut roots = 0;
        for p in 0..n {
            let mut acc = 0u16;
            for (i, &ci) in c.iter().enumerate().take(l + 1) {
                acc ^= gf.mul(ci, gf.alpha_pow((n - p % n) * i));
            }
            if acc == 0 {
                e.set(p, true);
                roots += 1;
            }
        }
        // A consistent decode has exactly `l` roots and reproduces the syndrome.
        (roots == l && self.syndrome(&e, t) == *r).then_some(e)
    }
}

fn cyclotomic_coset(i: usize, n: usize) -> Vec<usize> {
    let mut coset = vec![i];
    let mut j = (2 * i) % n;
    while j != i {
        coset.push(j);
        j = (2 * j) % n;
    }
    coset
}

/// Product of (x - alpha^j) over the coset; its coefficients lie in GF(2).
fn minimal_poly(gf: &Gf, coset: &[usize]) -> BitVec {
    let mut poly = vec![1u16];
    for &j in coset {
        let root = gf.alpha_pow(j);
        let mut next = vec![0u16; poly.len() + 1];
        for (k, &pk) in poly.iter().enumerate() {
            next[k + 1] ^= pk;
            next[k] ^= gf.mul(pk, root);
        }
        poly = next;
    }
    let mut bits = BitVec::zeros(poly.len());
    for (k, &pk) in poly.iter().enumerate() {
        debug_assert!(pk <= 1, "minimal polynomial must be binary");
        bits.set(k, pk == 1);
    }
    bits
}

fn gf2_mul(a: &BitVec, b: &BitVec) -> BitVec {
    let mut out = BitVec::zeros(a.len + b.len - 1);
    for k in b.ones() {
        xor_shifted(&mut out, a, k as usize);
    }
    out
}

/// `dst ^= src << shift` (bits of `src` placed starting at `shift`).
fn xor_shifted(dst: &mut BitVec, src: &BitVec, shift: usize) {
    let (w, b) = (shift / 64, shift % 64);
    for (k, &word) in src.words.iter().enumerate() {
        if word == 0 {
            continue;
        }
        dst.words[w + k] ^= word << b;
        if b != 0 && w + k + 1 < dst.words.len() {
            dst.words[w + k + 1] ^= word >> (64 - b);
        }
    }
}

/// The BCH family for block length n, if n = 2^m - 1 with m in 8..=13.
pub fn for_len(n: usize) -> Option<&'static Bch> {
    static CODES: OnceLock<Vec<Bch>> = OnceLock::new();
    let m = (n + 1).trailing_zeros();
    if n + 1 != 1 << m || !(MIN_M..=MAX_M).contains(&m) {
        return None;
    }
    let codes = CODES.get_or_init(|| (MIN_M..=MAX_M).map(Bch::new).collect());
    codes.get((m - MIN_M) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state >> 33
    }

    fn random_pattern(n: usize, t: usize, s: &mut u64) -> BitVec {
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
    fn fields_are_generated_by_a_primitive_element() {
        for m in MIN_M..=MAX_M {
            let gf = Gf::new(m);
            let mut seen = vec![false; gf.n + 1];
            for i in 0..gf.n {
                let v = gf.exp[i] as usize;
                assert!(v != 0 && !seen[v], "m={m} alpha not primitive");
                seen[v] = true;
            }
        }
    }

    #[test]
    fn syndromes_decode_up_to_t_errors() {
        let mut s = 99u64;
        for n in [255usize, 1023, 8191] {
            let bch = for_len(n).unwrap();
            for t in [0usize, 1, 2, 3, 8, 16, 33] {
                for _ in 0..4 {
                    let e = random_pattern(n, t, &mut s);
                    let r = bch.syndrome(&e, t);
                    assert_eq!(r.len, bch.syndrome_bits(t).unwrap());
                    assert_eq!(bch.decode(&r, t), Some(e), "n={n} t={t}");
                }
            }
        }
    }

    #[test]
    fn one_error_beyond_t_is_never_silently_accepted() {
        let mut s = 5u64;
        let bch = for_len(1023).unwrap();
        for t in [1usize, 4, 16] {
            for _ in 0..20 {
                let e = random_pattern(1023, t + 1, &mut s);
                let r = bch.syndrome(&e, t);
                // Decoding may fail or land on another pattern of weight <= t,
                // but it can never return the true heavier E.
                assert_ne!(bch.decode(&r, t), Some(e));
            }
        }
    }

    #[test]
    fn syndrome_size_matches_known_bch_parameters() {
        // (255, 239) corrects 2, (255, 231) corrects 3, (1023, 1013) corrects 1.
        assert_eq!(for_len(255).unwrap().syndrome_bits(2), Some(16));
        assert_eq!(for_len(255).unwrap().syndrome_bits(3), Some(24));
        assert_eq!(for_len(1023).unwrap().syndrome_bits(1), Some(10));
        assert!(for_len(511).is_some() && for_len(512).is_none());
    }
}
