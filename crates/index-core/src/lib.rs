//! `index-core` — the learned ordered-index core.
//!
//! P0 thesis (see `ROADMAP.md`): an index is one function, `locate: key -> position`.
//! A B-tree hand-codes it as a tree of comparisons; a *learned* index replaces it with a
//! piecewise-linear model whose prediction is guaranteed within `±epsilon` of the true
//! position, so the last-mile step is a bounded search over a window of size `2*epsilon+1`.
//!
//! This v0 builds the segments with a **correct, anchor-pinned greedy PLA**: every key is
//! guaranteed within `epsilon` of its prediction *by construction*. It is intentionally not
//! the minimal-segment optimal convex-hull PLA (O'Rourke) that PGM uses — that swap only
//! reduces `bytes/key`, never correctness, and is tracked as a P0 refinement.

pub mod crack;
pub mod data;
pub mod fm;
mod optimal;
pub mod wavelet;

pub use crack::CrackerColumn;
pub use fm::FmIndex;

/// One piecewise-linear segment. Predicted position at `key` is
/// `slope * (key - key0) + intercept`, then rounded and clamped to `[0, n)`.
#[derive(Clone, Copy, Debug)]
struct Segment {
    key0: u64,
    slope: f64,
    intercept: f64,
}

/// Result of a point lookup, carrying the last-mile window actually scanned so the
/// ε-invariant benchmark can assert it never exceeds `2*epsilon + 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchResult {
    /// Exact index of `key` in the sorted array, if present.
    pub pos: Option<usize>,
    /// Number of slots the bounded last-mile search was allowed to scan.
    pub window: usize,
}

/// Exact position of `key` inside the ε-window `key_arr[lo..hi)`, or `None`.
///
/// A branchless count of the keys below `key` beats `binary_search` here, and the gap widens with
/// ε: at ε=16 the window is 33 keys spread over five cache lines, so a binary search pays four
/// unpredictable branches to save four sequential loads the prefetcher was going to issue anyway.
/// Measured across sequential/uniform/lognormal/hard at n=1M, the count wins at ε≥16 and wins by
/// ~2x at ε=64 — which is what makes the larger, cheaper models usable at all. Table:
/// `bench/roadmap/p74-core-primitive.md`.
#[inline]
fn last_mile(key_arr: &[u64], lo: usize, hi: usize, key: u64) -> Option<usize> {
    let pos = lo + key_arr[lo..hi].iter().filter(|&&k| k < key).count();
    if pos < key_arr.len() && key_arr[pos] == key {
        Some(pos)
    } else {
        None
    }
}

/// A learned index over a sorted, unique `&[u64]`. Maps a key to its position (rank).
#[derive(Clone, Debug)]
pub struct PlaIndex {
    keys: Vec<u64>,
    segments: Vec<Segment>,
    seg_first_key: Vec<u64>,
    epsilon: usize,
    n: usize,
}

impl PlaIndex {
    /// Build over `keys` (sorted ascending & unique) using the **optimal convex-hull PLA**
    /// (O'Rourke / PGM) — the minimum number of ε-bounded segments. This is the default.
    pub fn build(keys: &[u64], epsilon: usize) -> Self {
        debug_assert!(keys.windows(2).all(|w| w[0] < w[1]), "keys must be sorted & unique");
        Self::assemble(keys, optimal::build_segments(keys, epsilon), epsilon)
    }

    /// Build with the simple anchor-pinned greedy PLA. Correct and ε-bounded, but uses more
    /// segments than [`PlaIndex::build`]. Kept as the baseline for the segment-count comparison.
    pub fn build_greedy(keys: &[u64], epsilon: usize) -> Self {
        debug_assert!(keys.windows(2).all(|w| w[0] < w[1]), "keys must be sorted & unique");
        let n = keys.len();
        let eps = epsilon as f64;
        let mut segments: Vec<(u64, f64, f64)> = Vec::new();
        let mut i = 0usize;
        while i < n {
            let x_s = keys[i] as f64;
            let y_s = i as f64;
            // Feasible slope interval for a line pinned at (x_s, y_s) that keeps every
            // point in the segment within `eps` vertically.
            let mut lo = f64::NEG_INFINITY;
            let mut hi = f64::INFINITY;
            let mut j = i + 1;
            while j < n {
                let dx = keys[j] as f64 - x_s; // > 0: keys sorted & unique
                let dy = j as f64 - y_s;
                let new_lo = lo.max((dy - eps) / dx);
                let new_hi = hi.min((dy + eps) / dx);
                if new_lo > new_hi {
                    break; // including j is infeasible — close segment at j-1, do NOT commit j
                }
                lo = new_lo;
                hi = new_hi;
                j += 1;
            }
            let slope = match (lo.is_finite(), hi.is_finite()) {
                (true, true) => 0.5 * (lo + hi),
                (true, false) => lo,
                (false, true) => hi,
                (false, false) => 0.0, // single-point segment
            };
            segments.push((keys[i], slope, y_s));
            i = j;
        }
        Self::assemble(keys, segments, epsilon)
    }

    fn assemble(keys: &[u64], raw: Vec<(u64, f64, f64)>, epsilon: usize) -> Self {
        let segments: Vec<Segment> = raw
            .iter()
            .map(|&(key0, slope, intercept)| Segment { key0, slope, intercept })
            .collect();
        let seg_first_key: Vec<u64> = raw.iter().map(|&(key0, _, _)| key0).collect();
        PlaIndex { keys: keys.to_vec(), segments, seg_first_key, epsilon, n: keys.len() }
    }

    /// Raw predicted position, rounded and clamped to `[0, n)`.
    #[inline]
    pub fn predict(&self, key: u64) -> usize {
        let idx = self.seg_first_key.partition_point(|&k| k <= key);
        let seg_i = idx.saturating_sub(1);
        let seg = &self.segments[seg_i];
        let raw = seg.slope * (key as f64 - seg.key0 as f64) + seg.intercept;
        let r = raw.round();
        if r < 0.0 {
            0
        } else if r as usize >= self.n {
            self.n - 1
        } else {
            r as usize
        }
    }

    /// Locate `key` via prediction + a bounded last-mile scan.
    #[inline]
    pub fn search(&self, key: u64) -> SearchResult {
        let pred = self.predict(key);
        let lo = pred.saturating_sub(self.epsilon);
        let hi = (pred + self.epsilon + 1).min(self.n); // exclusive
        SearchResult { pos: last_mile(&self.keys, lo, hi, key), window: hi - lo }
    }

    /// Number of segments (PLA model size in segments).
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Index-structure overhead in bytes per key (excludes the key data itself).
    /// Each segment costs `size_of::<Segment>()` + 8 bytes for its `seg_first_key` entry.
    pub fn index_bytes_per_key(&self) -> f64 {
        let per_seg = core::mem::size_of::<Segment>() + core::mem::size_of::<u64>();
        (self.segments.len() * per_seg) as f64 / self.n.max(1) as f64
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn epsilon(&self) -> usize {
        self.epsilon
    }
}

/// One level of a recursive PGM index: PLA segments plus their first keys (for searching).
#[derive(Clone, Debug)]
struct Level {
    segments: Vec<Segment>,
    first_keys: Vec<u64>,
}

impl Level {
    fn from_raw(raw: Vec<(u64, f64, f64)>) -> Self {
        let segments = raw
            .iter()
            .map(|&(key0, slope, intercept)| Segment { key0, slope, intercept })
            .collect();
        let first_keys = raw.iter().map(|&(k, _, _)| k).collect();
        Level { segments, first_keys }
    }
    fn len(&self) -> usize {
        self.segments.len()
    }
}

#[inline]
fn predict_seg(seg: &Segment, key: u64, upper_excl: usize) -> usize {
    let raw = seg.slope * (key as f64 - seg.key0 as f64) + seg.intercept;
    let r = raw.round();
    if r < 0.0 {
        0
    } else if r as usize >= upper_excl {
        upper_excl - 1
    } else {
        r as usize
    }
}

/// Rightmost index `i` with `arr[i] <= key`, searched first in a `±slack` window around `pred`.
/// If the covering index might lie outside the window (window edge hit), falls back to a full
/// search — so this is *always correct*, with the window as the fast path.
#[inline]
fn covering(arr: &[u64], key: u64, pred: usize, slack: usize) -> usize {
    let lo = pred.saturating_sub(slack);
    let hi = (pred + slack + 1).min(arr.len());
    if lo == 0 && hi == arr.len() {
        return arr.partition_point(|&k| k <= key).saturating_sub(1);
    }
    let rel = arr[lo..hi].partition_point(|&k| k <= key);
    if rel == 0 || lo + rel == hi {
        // covering segment may be outside the predicted window — guarantee correctness.
        return arr.partition_point(|&k| k <= key).saturating_sub(1);
    }
    lo + rel - 1
}

/// A recursive PGM index: a stack of PLA levels. The leaf maps keys → data positions; each
/// higher level is a PLA over the level below's segment-start keys, so locating the leaf segment
/// is a chain of `O(1)` predictions instead of one `O(log #segments)` binary search.
#[derive(Clone, Debug)]
pub struct PgmIndex {
    keys: Vec<u64>,
    levels: Vec<Level>, // levels[0] = leaf (over data); levels[last] = root (small)
    epsilon: usize,
    eps_rec: usize,
    n: usize,
}

impl PgmIndex {
    /// Build with leaf error `epsilon` and a default recursive error of 16.
    pub fn build(keys: &[u64], epsilon: usize) -> Self {
        Self::build_with(keys, epsilon, 16)
    }

    /// Build with explicit leaf error and recursive (upper-level) error.
    pub fn build_with(keys: &[u64], epsilon: usize, eps_rec: usize) -> Self {
        debug_assert!(keys.windows(2).all(|w| w[0] < w[1]), "keys must be sorted & unique");
        const ROOT_MAX: usize = 64;
        let mut levels = vec![Level::from_raw(optimal::build_segments(keys, epsilon))];
        while levels.last().unwrap().len() > ROOT_MAX {
            let arr = levels.last().unwrap().first_keys.clone();
            let segs = optimal::build_segments(&arr, eps_rec);
            let stop = segs.len() >= arr.len(); // no further compression possible
            levels.push(Level::from_raw(segs));
            if stop {
                break;
            }
        }
        PgmIndex { keys: keys.to_vec(), levels, epsilon, eps_rec, n: keys.len() }
    }

    /// Locate `key` by descending the level stack, then a bounded last-mile search in the data.
    #[inline]
    pub fn search(&self, key: u64) -> SearchResult {
        let top = self.levels.len() - 1;
        // Root: full binary search over its (small) first-key array.
        let mut idx = self.levels[top].first_keys.partition_point(|&k| k <= key).saturating_sub(1);
        let mut lvl = top;
        while lvl >= 1 {
            let lower = &self.levels[lvl - 1];
            let pred = predict_seg(&self.levels[lvl].segments[idx], key, lower.first_keys.len());
            idx = covering(&lower.first_keys, key, pred, self.eps_rec + 2);
            lvl -= 1;
        }
        let seg = &self.levels[0].segments[idx];
        let pred = predict_seg(seg, key, self.n);
        let lo = pred.saturating_sub(self.epsilon);
        let hi = (pred + self.epsilon + 1).min(self.n);
        SearchResult { pos: last_mile(&self.keys, lo, hi, key), window: hi - lo }
    }

    pub fn height(&self) -> usize {
        self.levels.len()
    }
    pub fn leaf_segment_count(&self) -> usize {
        self.levels[0].len()
    }
    pub fn segment_count(&self) -> usize {
        self.levels.iter().map(|l| l.len()).sum()
    }
    pub fn index_bytes_per_key(&self) -> f64 {
        let per_seg = core::mem::size_of::<Segment>() + core::mem::size_of::<u64>();
        (self.segment_count() * per_seg) as f64 / self.n.max(1) as f64
    }
    pub fn len(&self) -> usize {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{gen_hard, gen_lognormal, gen_sequential, gen_uniform};

    /// bench/roadmap/p0-pla-epsilon-invariant.md — the property the whole project bets on.
    /// For 100% of keys: |predicted - true| <= epsilon AND last-mile window <= 2*epsilon + 1.
    fn assert_epsilon_invariant(keys: &[u64], epsilon: usize) {
        let idx = PlaIndex::build(keys, epsilon);
        for (true_pos, &key) in keys.iter().enumerate() {
            let pred = idx.predict(key) as i64;
            let err = (pred - true_pos as i64).unsigned_abs() as usize;
            assert!(
                err <= epsilon,
                "epsilon invariant violated: key={key} true={true_pos} pred={pred} err={err} > eps={epsilon}",
            );
            let res = idx.search(key);
            assert_eq!(res.pos, Some(true_pos), "lookup must find the key at its true position");
            assert!(
                res.window <= 2 * epsilon + 1,
                "window {} exceeded 2*eps+1 = {}",
                res.window,
                2 * epsilon + 1
            );
        }
    }

    #[test]
    fn epsilon_invariant_holds_across_distributions() {
        for &eps in &[8usize, 16, 32, 64] {
            assert_epsilon_invariant(&gen_sequential(50_000), eps);
            assert_epsilon_invariant(&gen_uniform(50_000, 0xC0FFEE), eps);
            assert_epsilon_invariant(&gen_lognormal(50_000, 0xBADF00D), eps);
        }
    }

    #[test]
    fn handles_tiny_and_edge_inputs() {
        for &eps in &[8usize, 64] {
            assert_epsilon_invariant(&[42], eps);
            assert_epsilon_invariant(&[1, 2, 3], eps);
            assert_epsilon_invariant(&gen_sequential(1000), eps);
        }
    }

    #[test]
    fn optimal_uses_no_more_segments_than_greedy() {
        for &eps in &[8usize, 32, 64] {
            for keys in [
                gen_sequential(50_000),
                gen_uniform(50_000, 0xC0FFEE),
                gen_lognormal(50_000, 0xBADF00D),
            ] {
                let opt = PlaIndex::build(&keys, eps);
                let greedy = PlaIndex::build_greedy(&keys, eps);
                assert!(
                    opt.segment_count() <= greedy.segment_count(),
                    "optimal {} should be <= greedy {} (eps={eps})",
                    opt.segment_count(),
                    greedy.segment_count()
                );
                // And optimal must still satisfy the invariant it was built for.
                for (true_pos, &key) in keys.iter().enumerate() {
                    let err = (opt.predict(key) as i64 - true_pos as i64).unsigned_abs() as usize;
                    assert!(err <= eps, "optimal violated eps: err={err} > {eps}");
                }
            }
        }
    }

    #[test]
    fn pgm_finds_all_keys_within_bounded_window() {
        for &eps in &[8usize, 32, 64] {
            for keys in [
                gen_sequential(50_000),
                gen_uniform(50_000, 1),
                gen_lognormal(50_000, 2),
                gen_hard(50_000, 3),
            ] {
                let idx = PgmIndex::build(&keys, eps);
                for (true_pos, &key) in keys.iter().enumerate() {
                    let r = idx.search(key);
                    assert_eq!(r.pos, Some(true_pos), "pgm must find key at its true position");
                    assert!(r.window <= 2 * eps + 1, "window {} > 2*eps+1", r.window);
                }
            }
        }
    }

    #[test]
    fn pgm_recurses_on_irregular_data() {
        // The "hard" distribution forces enough leaf segments that recursion must kick in.
        let keys = gen_hard(200_000, 9);
        let idx = PgmIndex::build(&keys, 64);
        assert!(idx.height() >= 2, "expected recursion (height>=2), got {}", idx.height());
    }

    #[test]
    fn missing_keys_return_none_within_bounded_window() {
        let keys = gen_uniform(10_000, 7);
        let idx = PlaIndex::build(&keys, 32);
        // A key guaranteed absent (odd offset between two consecutive uniques is unlikely
        // to collide; we pick max+1 which is definitely absent).
        let absent = keys.last().unwrap().wrapping_add(1);
        let res = idx.search(absent);
        assert_eq!(res.pos, None);
        assert!(res.window <= 2 * 32 + 1);
    }
}
