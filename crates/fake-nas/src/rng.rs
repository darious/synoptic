//! A tiny seedable generator (SplitMix64) so the synthetic world is the
//! same on every run and for every window queried, without a `rand`
//! dependency.

#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// A generator keyed by several values (seed, camera, period, …).
    pub fn from_parts(parts: &[u64]) -> Self {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        for p in parts {
            s = (s ^ p).wrapping_mul(0xBF58_476D_1CE4_E5B9).rotate_left(23);
        }
        Rng(s)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (0 when `n == 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// Uniform in `lo..hi` (`lo` when the range is empty).
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        lo + self.below((hi - lo) as u64) as i64
    }

    /// `true` with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        unit < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_bounded() {
        let mut a = Rng::from_parts(&[1, 2, 3]);
        let mut b = Rng::from_parts(&[1, 2, 3]);
        for _ in 0..100 {
            let x = a.range(10, 20);
            assert_eq!(x, b.range(10, 20));
            assert!((10..20).contains(&x));
        }
        assert_ne!(
            Rng::from_parts(&[1, 2, 3]).next_u64(),
            Rng::from_parts(&[1, 2, 4]).next_u64()
        );
        assert_eq!(Rng::new(5).range(7, 7), 7);
    }
}
