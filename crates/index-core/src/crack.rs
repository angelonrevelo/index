//! Database cracking — an adaptive index that physically partitions the data *toward the queries
//! actually asked*, so it self-organizes from the workload instead of being pre-sorted.
//!
//! `query(lo, hi)` cracks the column around `lo` and `hi` (quicksort-style two-way partitions),
//! records the resulting pivots in a cracker index, and returns the slice holding exactly the
//! keys in `[lo, hi)`. Repeated queries make pieces smaller, so lookups get faster over time.
//!
//! **Naive cracking** only partitions at query bounds — pathological on sequential/adversarial
//! workloads (each query shaves one end of a huge remaining piece → O(n) per query). **Stochastic
//! cracking** (Halim et al.) also splits oversized pieces at random pivots, bounding piece size
//! regardless of query order. See `bench/roadmap/p1-cracking-convergence.md`.

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

/// Pieces larger than this are subdivided at random pivots (stochastic mode only).
const CRACK_LIMIT: usize = 1024;

/// Two-way partition of `data[from..to)` around `v`: afterwards `data[from..b] < v` and
/// `data[b..to] >= v`. Returns the boundary `b`.
fn partition(data: &mut [u64], from: usize, to: usize, v: u64) -> usize {
    let mut i = from;
    let mut j = to;
    while i < j {
        if data[i] < v {
            i += 1;
        } else {
            j -= 1;
            data.swap(i, j);
        }
    }
    i
}

#[inline]
fn splitmix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// An adaptively-cracked column of `u64` keys.
pub struct CrackerColumn {
    data: Vec<u64>,
    /// `value -> position`: `data[..position] < value <= data[position..]`.
    index: BTreeMap<u64, usize>,
    stochastic: bool,
    rng: u64,
}

impl CrackerColumn {
    /// Wrap `data` (any order; cracking sorts it lazily). `stochastic` enables random pivot splits.
    pub fn new(data: Vec<u64>, stochastic: bool, seed: u64) -> Self {
        CrackerColumn { data, index: BTreeMap::new(), stochastic, rng: seed | 1 }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
    /// Number of contiguous pieces the column is currently partitioned into (index size + 1).
    pub fn piece_count(&self) -> usize {
        self.index.len() + 1
    }

    /// The piece `[l, r)` currently known to bracket the crack point for `v`.
    fn piece_bounds(&self, v: u64) -> (usize, usize) {
        let l = self.index.range(..=v).next_back().map(|(_, &p)| p).unwrap_or(0);
        let r = self
            .index
            .range((Excluded(v), Unbounded))
            .next()
            .map(|(_, &p)| p)
            .unwrap_or(self.data.len());
        (l, r)
    }

    /// Crack so that `data[..b] < v <= data[b..]`, returning `b`.
    fn crack(&mut self, v: u64) -> usize {
        let (mut l, mut r) = self.piece_bounds(v);
        if l >= r {
            return l; // already cracked exactly here
        }
        if self.stochastic {
            // Subdivide an oversized piece at random pivots until it's small, narrowing toward v.
            while r - l > CRACK_LIMIT {
                let m = l + (splitmix(&mut self.rng) as usize % (r - l));
                let pivot = self.data[m];
                let b = partition(&mut self.data, l, r, pivot);
                if b == l || b == r {
                    break; // pivot was an extreme of the piece; no useful split this round
                }
                self.index.insert(pivot, b);
                if v < pivot {
                    r = b;
                } else {
                    l = b;
                }
            }
        }
        let b = partition(&mut self.data, l, r, v);
        self.index.insert(v, b);
        b
    }

    /// Return the keys in `[lo, hi)` (unordered), cracking the column as a side effect.
    pub fn query(&mut self, lo: u64, hi: u64) -> &[u64] {
        debug_assert!(lo <= hi);
        let a = self.crack(lo);
        let b = self.crack(hi);
        &self.data[a..b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::gen_uniform;

    fn shuffled(n: usize, seed: u64) -> Vec<u64> {
        let mut v = gen_uniform(n, seed); // sorted unique
        let mut s = seed | 1;
        // Fisher–Yates with the same deterministic rng.
        for i in (1..v.len()).rev() {
            let j = (splitmix(&mut s) as usize) % (i + 1);
            v.swap(i, j);
        }
        v
    }

    fn brute(keys: &[u64], lo: u64, hi: u64) -> Vec<u64> {
        let mut r: Vec<u64> = keys.iter().copied().filter(|&k| k >= lo && k < hi).collect();
        r.sort_unstable();
        r
    }

    fn check_mode(stochastic: bool) {
        let original = shuffled(20_000, 42);
        let mut col = CrackerColumn::new(original.clone(), stochastic, 7);
        let mut s = 123u64;
        for _ in 0..2000 {
            let a = splitmix(&mut s) % 200_000;
            let b = splitmix(&mut s) % 200_000;
            let (lo, hi) = (a.min(b), a.max(b));
            let mut got = col.query(lo, hi).to_vec();
            got.sort_unstable();
            assert_eq!(got, brute(&original, lo, hi), "range [{lo},{hi}) mismatch");
        }
        // No keys lost or duplicated: the column is always a permutation of the input.
        let mut after = col.data.clone();
        after.sort_unstable();
        let mut orig = original.clone();
        orig.sort_unstable();
        assert_eq!(after, orig, "cracking must preserve the multiset of keys");
    }

    #[test]
    fn naive_cracking_is_correct() {
        check_mode(false);
    }

    #[test]
    fn stochastic_cracking_is_correct() {
        check_mode(true);
    }

    #[test]
    fn cracking_converges_more_pieces_over_time() {
        let mut col = CrackerColumn::new(shuffled(100_000, 1), true, 1);
        let mut s = 9u64;
        for _ in 0..500 {
            let a = splitmix(&mut s) % 800_000;
            let w = 1 + splitmix(&mut s) % 1000;
            col.query(a, a + w);
        }
        assert!(col.piece_count() > 100, "expected many pieces after 500 queries");
    }
}
