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
//!
//! Once stochastic cracking has converged, a query's cost is no longer the index lookup — it is
//! the two partitions of the surviving piece, i.e. `O(piece_limit)` element moves. That makes
//! `piece_limit` the single knob that decides converged latency, and it is
//! swept in `bench/roadmap/p74-core-primitive.md` rather than guessed.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};

/// Default piece limit: pieces larger than this are subdivided at random pivots (stochastic mode).
pub const CRACK_LIMIT: usize = 1024;

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

/// Cumulative work counters — the only honest way to say *where* a cracked query spent its time,
/// since the phases are far too short to time individually without the clock dominating.
#[derive(Clone, Copy, Debug, Default)]
pub struct CrackStat {
    /// Elements visited by `partition` (the data-movement cost).
    pub partition_element: u64,
    /// Elements handed to `sort_unstable` when a piece was promoted to sorted.
    pub sort_element: u64,
    /// Cracker-index lookups (two `BTreeMap` range walks each).
    pub index_probe: u64,
    /// Cracker-index insertions.
    pub index_insert: u64,
    /// Cracks answered by a binary search inside an already-sorted piece — zero data movement.
    pub sorted_hit: u64,
}

/// An adaptively-cracked column of `u64` keys.
pub struct CrackerColumn {
    data: Vec<u64>,
    /// `value -> position`: `data[..position] < value <= data[position..]`.
    index: BTreeMap<u64, usize>,
    /// Start offsets of pieces known to be in sorted order. A crack inside one of these is a
    /// binary search that moves nothing, and both halves stay sorted — so the piece never has to
    /// be re-partitioned again.
    sorted: BTreeSet<usize>,
    /// Start offsets of pieces a crack has already landed in once.
    seen: BTreeSet<usize>,
    stochastic: bool,
    piece_limit: usize,
    sort_limit: usize,
    rng: u64,
    stat: CrackStat,
}

impl CrackerColumn {
    /// Wrap `data` (any order; cracking sorts it lazily). `stochastic` enables random pivot splits.
    pub fn new(data: Vec<u64>, stochastic: bool, seed: u64) -> Self {
        Self::with_limit(data, stochastic, seed, CRACK_LIMIT, CRACK_LIMIT)
    }

    /// As [`CrackerColumn::new`], with explicit limits. `piece_limit` bounds the piece a
    /// stochastic split leaves behind; `sort_limit` is the size at or below which a piece is
    /// sorted once instead of partitioned again (0 disables that).
    pub fn with_limit(
        data: Vec<u64>,
        stochastic: bool,
        seed: u64,
        piece_limit: usize,
        sort_limit: usize,
    ) -> Self {
        CrackerColumn {
            data,
            index: BTreeMap::new(),
            sorted: BTreeSet::new(),
            seen: BTreeSet::new(),
            stochastic,
            piece_limit: piece_limit.max(1),
            sort_limit,
            rng: seed | 1,
            stat: CrackStat::default(),
        }
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
    /// Number of pieces currently held in sorted order.
    pub fn sorted_piece_count(&self) -> usize {
        self.sorted.len()
    }
    pub fn stat(&self) -> CrackStat {
        self.stat
    }

    /// The piece `[l, r)` currently known to bracket the crack point for `v`.
    fn piece_bounds(&mut self, v: u64) -> (usize, usize) {
        self.stat.index_probe += 1;
        let l = self.index.range(..=v).next_back().map(|(_, &p)| p).unwrap_or(0);
        let r = self
            .index
            .range((Excluded(v), Unbounded))
            .next()
            .map(|(_, &p)| p)
            .unwrap_or(self.data.len());
        (l, r)
    }

    /// Crack a piece that is already sorted: a binary search, no data movement, and both halves
    /// inherit sortedness.
    fn crack_sorted(&mut self, l: usize, r: usize, v: u64) -> usize {
        let b = l + self.data[l..r].partition_point(|&x| x < v);
        self.index.insert(v, b);
        self.stat.index_insert += 1;
        self.stat.sorted_hit += 1;
        if b < r {
            self.sorted.insert(b);
        }
        b
    }

    /// Crack so that `data[..b] < v <= data[b..]`, returning `b`.
    fn crack(&mut self, v: u64) -> usize {
        let (mut l, mut r) = self.piece_bounds(v);
        if l >= r {
            return l; // already cracked exactly here
        }
        if self.sorted.contains(&l) {
            return self.crack_sorted(l, r, v);
        }
        if self.stochastic {
            // Subdivide an oversized piece at random pivots until it's small, narrowing toward v.
            while r - l > self.piece_limit {
                let m = l + (splitmix(&mut self.rng) as usize % (r - l));
                let pivot = self.data[m];
                let b = partition(&mut self.data, l, r, pivot);
                self.stat.partition_element += (r - l) as u64;
                if b == l || b == r {
                    break; // pivot was an extreme of the piece; no useful split this round
                }
                self.index.insert(pivot, b);
                self.stat.index_insert += 1;
                if v < pivot {
                    r = b;
                } else {
                    l = b;
                }
            }
        }
        if self.sort_limit > 0 && r - l <= self.sort_limit && !self.seen.insert(l) {
            self.data[l..r].sort_unstable();
            self.stat.sort_element += (r - l) as u64;
            self.sorted.insert(l);
            return self.crack_sorted(l, r, v);
        }
        let b = partition(&mut self.data, l, r, v);
        self.stat.partition_element += (r - l) as u64;
        self.index.insert(v, b);
        self.stat.index_insert += 1;
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

    fn check_mode(stochastic: bool, piece_limit: usize, sort_limit: usize) {
        let original = shuffled(20_000, 42);
        let mut col =
            CrackerColumn::with_limit(original.clone(), stochastic, 7, piece_limit, sort_limit);
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
        check_mode(false, CRACK_LIMIT, CRACK_LIMIT);
    }

    #[test]
    fn stochastic_cracking_is_correct() {
        check_mode(true, CRACK_LIMIT, CRACK_LIMIT);
    }

    /// The sorted-piece promotion is the only step that reorders data outside `partition`, so it
    /// gets checked at every limit combination, including with promotion switched off.
    #[test]
    fn correct_across_limit_settings() {
        for &(piece, sort) in &[(64usize, 0usize), (64, 64), (1, 1), (1 << 20, 0), (256, 4096)] {
            check_mode(true, piece, sort);
            check_mode(false, piece, sort);
        }
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

    /// A sorted piece is never partitioned again, so data movement has to *stop*, not merely
    /// shrink, once the workload has converged onto sorted pieces.
    #[test]
    fn converged_queries_stop_moving_data() {
        fn burst(col: &mut CrackerColumn, n: usize, s: &mut u64) {
            for _ in 0..n {
                let a = splitmix(s) % 800_000;
                col.query(a, a + 500);
            }
        }
        let mut col = CrackerColumn::new(shuffled(100_000, 3), true, 3);
        let mut s = 5u64;
        burst(&mut col, 2000, &mut s);
        let warm = col.stat().partition_element;
        burst(&mut col, 2000, &mut s);
        let late = col.stat().partition_element - warm;
        assert!(late * 5 < warm, "late queries still move data: {late} moved vs {warm} warming");
        assert!(col.sorted_piece_count() > 100, "expected many sorted pieces");
        assert!(col.stat().sorted_hit > 2000, "expected most late cracks to be binary searches");
    }

    #[test]
    fn degenerate_columns() {
        for data in [vec![], vec![7u64], vec![5u64; 1000]] {
            let mut col = CrackerColumn::new(data.clone(), true, 1);
            assert_eq!(col.len(), data.len());
            assert_eq!(col.is_empty(), data.is_empty());
            let mut got = col.query(0, u64::MAX).to_vec();
            got.sort_unstable();
            let mut want = data.clone();
            want.sort_unstable();
            assert_eq!(got, want);
            assert!(col.query(9, 9).is_empty());
        }
    }
}
