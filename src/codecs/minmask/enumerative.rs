//! Exact enumerative coding: a weight-t bitmap of n bits is stored as the
//! rank of its set of positions among all C(n, t) subsets (combinatorial
//! number system), in exactly `rank_width(n, t)` bits.

use super::bits::BitVec;
use std::cmp::Ordering;

/// Unsigned big integer, little-endian 64-bit limbs, no leading zero limbs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Big(Vec<u64>);

impl Big {
    pub fn zero() -> Self {
        Big(Vec::new())
    }

    pub fn one() -> Self {
        Big(vec![1])
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    pub fn mul_small(&mut self, m: u64) {
        let mut carry = 0u128;
        for limb in &mut self.0 {
            let x = u128::from(*limb) * u128::from(m) + carry;
            *limb = x as u64;
            carry = x >> 64;
        }
        if carry > 0 {
            self.0.push(carry as u64);
        }
        self.trim();
    }

    /// Divide in place by `0 < d < 2^32`, returning the remainder. Works in
    /// 32-bit halves so every step is a native 64-bit division.
    pub fn div_small(&mut self, d: u64) -> u64 {
        debug_assert!(d > 0 && d >> 32 == 0);
        let mut rem = 0u64;
        for limb in self.0.iter_mut().rev() {
            let hi = (rem << 32) | (*limb >> 32);
            let (q_hi, r_hi) = (hi / d, hi % d);
            let lo = (r_hi << 32) | (*limb & 0xFFFF_FFFF);
            let (q_lo, r_lo) = (lo / d, lo % d);
            *limb = (q_hi << 32) | q_lo;
            rem = r_lo;
        }
        self.trim();
        rem
    }

    pub fn add(&mut self, o: &Big) {
        if self.0.len() < o.0.len() {
            self.0.resize(o.0.len(), 0);
        }
        let mut carry = 0u64;
        for (i, limb) in self.0.iter_mut().enumerate() {
            let (s1, c1) = limb.overflowing_add(o.0.get(i).copied().unwrap_or(0));
            let (s2, c2) = s1.overflowing_add(carry);
            *limb = s2;
            carry = u64::from(c1) + u64::from(c2);
        }
        if carry > 0 {
            self.0.push(carry);
        }
    }

    /// `self -= o`; requires `self >= o`.
    pub fn sub(&mut self, o: &Big) {
        let mut borrow = 0u64;
        for (i, limb) in self.0.iter_mut().enumerate() {
            let (d1, b1) = limb.overflowing_sub(o.0.get(i).copied().unwrap_or(0));
            let (d2, b2) = d1.overflowing_sub(borrow);
            *limb = d2;
            borrow = u64::from(b1) + u64::from(b2);
        }
        debug_assert_eq!(borrow, 0, "Big::sub underflow");
        self.trim();
    }

    pub fn cmp_big(&self, o: &Big) -> Ordering {
        self.0
            .len()
            .cmp(&o.0.len())
            .then_with(|| self.0.iter().rev().cmp(o.0.iter().rev()))
    }

    pub fn bit_length(&self) -> usize {
        match self.0.last() {
            None => 0,
            Some(top) => 64 * (self.0.len() - 1) + 64 - top.leading_zeros() as usize,
        }
    }

    pub fn bit(&self, i: usize) -> bool {
        self.0.get(i / 64).is_some_and(|w| (w >> (i % 64)) & 1 == 1)
    }

    pub fn from_bits_msb_first(bits: impl Iterator<Item = bool>) -> Self {
        let mut v = Big::zero();
        for b in bits {
            v.mul_small(2);
            if b {
                v.add(&Big::one());
            }
        }
        v
    }
}

/// C(n, k) exactly.
pub fn binom(n: u64, k: u64) -> Big {
    if k > n {
        return Big::zero();
    }
    let k = k.min(n - k);
    let mut c = Big::one();
    for i in 0..k {
        c.mul_small(n - i);
        c.div_small(i + 1);
    }
    c
}

/// Bits needed to store any rank in `0..C(n, t)`, i.e. ceil(log2 C(n, t)).
///
/// The floating-point log2 is exact enough to decide the ceiling unless it
/// lands within 1e-5 of an integer; only then is C(n, t) computed exactly.
/// Encoder and decoder share this function, so the width always agrees.
pub fn rank_width(n: usize, t: usize) -> usize {
    if t == 0 || t >= n {
        return 0;
    }
    let est = log2_binom(n, t);
    let frac = est - est.floor();
    if frac > 1e-5 && frac < 1.0 - 1e-5 {
        return est.ceil() as usize;
    }
    let mut c = binom(n as u64, t as u64);
    c.sub(&Big::one());
    c.bit_length()
}

/// Largest n served by the log2-factorial table (the largest block size).
const LOG2_FACT_MAX: usize = 1 << 19;

/// log2(k!) for k <= LOG2_FACT_MAX, summed with Kahan compensation.
fn log2_fact() -> &'static [f64] {
    static TABLE: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Vec::with_capacity(LOG2_FACT_MAX + 1);
        let (mut sum, mut comp) = (0.0f64, 0.0f64);
        table.push(0.0);
        for k in 1..=LOG2_FACT_MAX {
            let y = (k as f64).log2() - comp;
            let t = sum + y;
            comp = (t - sum) - y;
            sum = t;
            table.push(sum);
        }
        table
    })
}

/// log2 C(n, t) in floating point.
pub fn log2_binom(n: usize, t: usize) -> f64 {
    if t > n {
        return f64::NEG_INFINITY;
    }
    if n <= LOG2_FACT_MAX {
        let f = log2_fact();
        return f[n] - f[t] - f[n - t];
    }
    let t = t.min(n - t);
    (0..t).map(|i| ((n - i) as f64 / (i + 1) as f64).log2()).sum()
}

/// Rank of the set bits of `e`: sum over the i-th set position p_i (1-based
/// i, ascending) of C(p_i, i).
pub fn rank(e: &BitVec) -> Big {
    let ones = e.ones();
    let t = ones.len();
    if t * t / 2 < e.len {
        // Sparse: build each C(p_i, i) directly, O(t^2) small steps.
        let mut rank = Big::zero();
        for (i, &p) in ones.iter().enumerate() {
            rank.add(&binom(u64::from(p), i as u64 + 1));
        }
        return rank;
    }
    rank_incremental(&ones)
}

/// Same sum, walking every position once, O(n) small steps.
fn rank_incremental(ones: &[u32]) -> Big {
    let mut rank = Big::zero();
    let mut v = Big::zero(); // C(a, i)
    let mut a = 0u64;
    for (i, &p) in (1u64..).zip(ones.iter()) {
        let p = u64::from(p);
        while a < p {
            a += 1;
            if a == i {
                v = Big::one();
            } else if a > i {
                v.mul_small(a);
                v.div_small(a - i);
            }
        }
        rank.add(&v);
        // C(a, i + 1) = C(a, i) * (a - i) / (i + 1)
        if a > i {
            v.mul_small(a - i);
            v.div_small(i + 1);
        } else {
            v = Big::zero();
        }
    }
    rank
}

/// Inverse of [`rank`]; `None` if `r` is not a valid rank for (n, t).
pub fn unrank(r: &Big, n: usize, t: usize) -> Option<BitVec> {
    let mut out = BitVec::zeros(n);
    if t == 0 {
        return r.is_zero().then_some(out);
    }
    if t > n {
        return None;
    }
    let mut rest = r.clone();
    let mut p = n as u64 - 1;
    let mut i = t as u64;
    let mut v = binom(p, i); // C(p, i)
    loop {
        // Largest p with C(p, i) <= rest, scanning down: C(p-1, i) = C(p, i) (p - i) / p
        while v.cmp_big(&rest) == Ordering::Greater {
            if p == 0 {
                return None;
            }
            if p > i {
                v.mul_small(p - i);
                v.div_small(p);
            } else {
                v = Big::zero();
            }
            p -= 1;
        }
        out.set(p as usize, true);
        rest.sub(&v);
        if i == 1 {
            break;
        }
        if p == 0 {
            return None;
        }
        // C(p - 1, i - 1) = C(p, i) * i / p
        v.mul_small(i);
        v.div_small(p);
        p -= 1;
        i -= 1;
    }
    rest.is_zero().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state >> 33
    }

    #[test]
    fn ranks_are_a_bijection_on_small_sets() {
        // Every 3-subset of 10 positions gets a distinct rank in 0..120.
        let mut seen = [false; 120];
        for mask in 0u32..1024 {
            if mask.count_ones() != 3 {
                continue;
            }
            let mut e = BitVec::zeros(10);
            for b in 0..10 {
                e.set(b, (mask >> b) & 1 == 1);
            }
            let r = rank(&e);
            let idx = r.0.first().copied().unwrap_or(0) as usize;
            assert!(!seen[idx]);
            seen[idx] = true;
            assert_eq!(unrank(&r, 10, 3), Some(e));
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(rank_width(10, 3), 7);
    }

    #[test]
    fn random_sparse_and_dense_roundtrip() {
        let mut s = 7u64;
        for &(n, t) in &[(1023usize, 1usize), (1023, 64), (1023, 511), (8191, 200), (4096, 4000), (65536, 300)] {
            let mut e = BitVec::zeros(n);
            let mut placed = 0;
            while placed < t {
                let p = (lcg(&mut s) % n as u64) as usize;
                if !e.get(p) {
                    e.set(p, true);
                    placed += 1;
                }
            }
            let r = rank(&e);
            assert!(r.bit_length() <= rank_width(n, t));
            assert_eq!(unrank(&r, n, t), Some(e));
            let floor = log2_binom(n, t);
            assert!((rank_width(n, t) as f64) - floor < 1.0 + 1e-9);
        }
    }

    #[test]
    fn fast_rank_width_equals_exact_width() {
        for &n in &[255usize, 1023, 2048, 8191, 65536] {
            for t in (0..=n.min(600)).chain([n / 2, n - 1, n]) {
                let exact = {
                    let mut c = binom(n as u64, t as u64);
                    c.sub(&Big::one());
                    c.bit_length()
                };
                assert_eq!(rank_width(n, t), exact, "n={n} t={t}");
            }
        }
    }

    #[test]
    fn out_of_range_rank_is_rejected() {
        let c = binom(50, 5);
        assert_eq!(unrank(&c, 50, 5), None);
    }
}
